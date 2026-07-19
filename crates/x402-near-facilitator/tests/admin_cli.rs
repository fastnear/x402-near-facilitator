#![cfg(unix)]

use std::error::Error;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::str::FromStr;

use near_crypto::SecretKey;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};
use url::Url;
use uuid::Uuid;
use x402_near_facilitator::auth::digest_api_key;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const TEST_ASSET: &str = "usdc.admin-tests.testnet";
const TEST_PAYEE: &str = "merchant.admin-tests.testnet";

#[derive(Debug)]
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!(
            "x402-near-admin-{label}-{}",
            Uuid::new_v4().simple()
        ));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn write_secret(&self, name: &str, value: &str) -> TestResult<PathBuf> {
        let path = self.path(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        writeln!(file, "{value}")?;
        file.sync_all()?;
        Ok(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
struct TestDatabase {
    admin: PgPool,
    scoped: PgPool,
    schema: String,
    directory: TestDirectory,
    database_url_file: PathBuf,
    pepper_file: PathBuf,
    pepper: String,
}

impl TestDatabase {
    async fn new() -> TestResult<Option<Self>> {
        let Some(database_url) = loopback_database_url()? else {
            eprintln!(
                "skipping admin CLI PostgreSQL integration test: \
                 X402_FACILITATOR_TEST_DATABASE_URL is unset or not loopback"
            );
            return Ok(None);
        };

        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await?;
        let schema = format!("x402_admin_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await?;

        let options =
            PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let scoped = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;

        let directory = TestDirectory::new("postgres")?;
        let mut command_url = Url::parse(&database_url)?;
        command_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let database_url_file = directory.write_secret("database-url", command_url.as_str())?;
        let pepper = "42".repeat(32);
        let pepper_file = directory.write_secret("pepper", &pepper)?;

        Ok(Some(Self {
            admin,
            scoped,
            schema,
            directory,
            database_url_file,
            pepper_file,
            pepper,
        }))
    }

    async fn cleanup(self) -> TestResult {
        self.scoped.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await?;
        self.admin.close().await;
        Ok(())
    }

    fn args(&self, command: &[&str]) -> Vec<OsString> {
        let mut args = command
            .iter()
            .map(|value| OsString::from(*value))
            .collect::<Vec<_>>();
        args.extend([
            OsString::from("--database-url-file"),
            self.database_url_file.as_os_str().to_owned(),
        ]);
        args
    }
}

fn loopback_database_url() -> TestResult<Option<String>> {
    let Ok(raw) = std::env::var("X402_FACILITATOR_TEST_DATABASE_URL") else {
        return Ok(None);
    };
    let url = Url::parse(&raw)?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Ok(None);
    }
    let is_loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    Ok(is_loopback.then_some(raw))
}

fn run_admin(args: impl IntoIterator<Item = OsString>) -> TestResult<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_x402-near-admin"))
        .args(args)
        .output()?)
}

fn require_success(output: &Output) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "admin command exited unsuccessfully: {}",
            output.status
        ))
        .into())
    }
}

fn require_failure(output: &Output) -> TestResult {
    if output.status.success() {
        Err(io::Error::other("admin command unexpectedly succeeded").into())
    } else {
        Ok(())
    }
}

fn stdout(output: &Output) -> TestResult<String> {
    Ok(String::from_utf8(output.stdout.clone())?)
}

fn stderr(output: &Output) -> TestResult<String> {
    Ok(String::from_utf8(output.stderr.clone())?)
}

fn output_field<'a>(output: &'a str, field: &str) -> TestResult<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .ok_or_else(|| io::Error::other(format!("missing {field} output")).into())
}

fn validate_api_key(raw: &str, expected_label: &str) -> TestResult<String> {
    let (prefix, secret) = raw
        .split_once('.')
        .ok_or_else(|| io::Error::other("API key has no separator"))?;
    let expected_prefix = format!("x402_{expected_label}_");
    let public_id = prefix
        .strip_prefix(&expected_prefix)
        .ok_or_else(|| io::Error::other("API key has the wrong environment prefix"))?;
    if public_id.len() != 24
        || secret.len() != 64
        || !public_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::other("API key has an invalid shape").into());
    }
    Ok(prefix.to_owned())
}

#[test]
fn generate_relayer_is_create_new_mode_0600_and_never_prints_private_material() -> TestResult {
    let directory = TestDirectory::new("key")?;
    let key_path = directory.path("relayer-key");
    let first = run_admin([
        OsString::from("key"),
        OsString::from("generate-relayer"),
        OsString::from("--output"),
        key_path.as_os_str().to_owned(),
    ])?;
    require_success(&first)?;

    let secret_text = fs::read_to_string(&key_path)?;
    let secret = SecretKey::from_str(secret_text.trim())?;
    let public_key = secret.public_key().to_string();
    let first_stdout = stdout(&first)?;
    let first_stderr = stderr(&first)?;
    assert_eq!(first_stdout.trim(), public_key);
    assert!(!first_stdout.contains(secret_text.trim()));
    assert!(!first_stderr.contains(secret_text.trim()));
    assert_eq!(fs::metadata(&key_path)?.permissions().mode() & 0o777, 0o600);

    let before = fs::read(&key_path)?;
    let second = run_admin([
        OsString::from("key"),
        OsString::from("generate-relayer"),
        OsString::from("--output"),
        key_path.as_os_str().to_owned(),
    ])?;
    require_failure(&second)?;
    assert!(second.stdout.is_empty());
    assert_eq!(fs::read(&key_path)?, before);
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn admin_cli_migrates_and_enforces_the_full_client_lifecycle() -> TestResult {
    let Some(database) = TestDatabase::new().await? else {
        return Ok(());
    };

    let missing_url = database.directory.path("missing-database-url");
    let missing_migration = run_admin([
        OsString::from("migrate"),
        OsString::from("--database-url-file"),
        missing_url.into_os_string(),
    ])?;
    require_failure(&missing_migration)?;
    assert!(missing_migration.stdout.is_empty());

    for _ in 0..2 {
        let migration = run_admin(database.args(&["migrate"]))?;
        require_success(&migration)?;
        assert!(migration.stdout.is_empty());
    }
    let migration_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success = true")
            .fetch_one(&database.scoped)
            .await?;
    assert_eq!(migration_count, 1);

    let short_pepper = database.directory.write_secret("short-pepper", "short")?;
    let mut short_pepper_args = database.args(&["client", "create"]);
    short_pepper_args.extend([
        OsString::from("--pepper-file"),
        short_pepper.into_os_string(),
        OsString::from("--environment"),
        OsString::from("testnet"),
        OsString::from("--name"),
        OsString::from("short-pepper"),
    ]);
    let short_pepper_create = run_admin(short_pepper_args)?;
    require_failure(&short_pepper_create)?;
    assert!(short_pepper_create.stdout.is_empty());

    let mut invalid_budget_args = database.args(&["client", "create"]);
    invalid_budget_args.extend([
        OsString::from("--pepper-file"),
        database.pepper_file.as_os_str().to_owned(),
        OsString::from("--environment"),
        OsString::from("testnet"),
        OsString::from("--name"),
        OsString::from("invalid-budget"),
        OsString::from("--daily-yocto-near"),
        OsString::from("not-a-number"),
    ]);
    let invalid_budget_create = run_admin(invalid_budget_args)?;
    require_failure(&invalid_budget_create)?;
    assert!(invalid_budget_create.stdout.is_empty());

    let mut create_args = database.args(&["client", "create"]);
    create_args.extend([
        OsString::from("--pepper-file"),
        database.pepper_file.as_os_str().to_owned(),
        OsString::from("--environment"),
        OsString::from("testnet"),
        OsString::from("--name"),
        OsString::from("admin-integration"),
        OsString::from("--verify-rate-per-minute"),
        OsString::from("61"),
        OsString::from("--settle-rate-per-minute"),
        OsString::from("11"),
        OsString::from("--daily-yocto-near"),
        OsString::from("98765"),
    ]);
    let created = run_admin(create_args)?;
    require_success(&created)?;
    let created_stdout = stdout(&created)?;
    let client_id = Uuid::parse_str(output_field(&created_stdout, "client_id=")?)?;
    let first_raw_key = output_field(&created_stdout, "api_key=")?.to_owned();
    let first_prefix = validate_api_key(&first_raw_key, "test")?;
    assert_eq!(created_stdout.matches(&first_raw_key).count(), 1);
    assert!(!stderr(&created)?.contains(&first_raw_key));
    assert_eq!(created_stdout.lines().count(), 2);

    let client_row = sqlx::query(
        "SELECT name, environment, status, daily_budget_yocto_near::text AS budget, \
         verify_rate_per_minute, settle_rate_per_minute FROM api_clients WHERE id = $1",
    )
    .bind(client_id)
    .fetch_one(&database.scoped)
    .await?;
    assert_eq!(
        client_row.try_get::<String, _>("name")?,
        "admin-integration"
    );
    assert_eq!(client_row.try_get::<String, _>("environment")?, "testnet");
    assert_eq!(client_row.try_get::<String, _>("status")?, "active");
    assert_eq!(client_row.try_get::<String, _>("budget")?, "98765");
    assert_eq!(client_row.try_get::<i32, _>("verify_rate_per_minute")?, 61);
    assert_eq!(client_row.try_get::<i32, _>("settle_rate_per_minute")?, 11);

    let first_key_row =
        sqlx::query("SELECT key_prefix, key_digest, status FROM api_keys WHERE client_id = $1")
            .bind(client_id)
            .fetch_one(&database.scoped)
            .await?;
    assert_eq!(
        first_key_row.try_get::<String, _>("key_prefix")?,
        first_prefix
    );
    assert_eq!(first_key_row.try_get::<String, _>("status")?, "active");
    let first_digest = digest_api_key(database.pepper.as_bytes(), first_raw_key.as_bytes())?;
    assert_eq!(
        first_key_row.try_get::<Vec<u8>, _>("key_digest")?,
        first_digest
    );

    let unknown_client = Uuid::new_v4();
    let mut missing_rotate_args = database.args(&["client", "rotate"]);
    missing_rotate_args.extend([
        OsString::from("--pepper-file"),
        database.pepper_file.as_os_str().to_owned(),
        OsString::from("--client-id"),
        OsString::from(unknown_client.to_string()),
    ]);
    let missing_rotate = run_admin(missing_rotate_args)?;
    require_failure(&missing_rotate)?;
    assert!(missing_rotate.stdout.is_empty());

    let mut wrong_network_args = database.args(&["client", "allow-payee"]);
    wrong_network_args.extend([
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
        OsString::from("--network"),
        OsString::from("near:mainnet"),
        OsString::from("--asset"),
        OsString::from(TEST_ASSET),
        OsString::from("--pay-to"),
        OsString::from(TEST_PAYEE),
    ]);
    let wrong_network = run_admin(wrong_network_args)?;
    require_failure(&wrong_network)?;
    assert!(wrong_network.stdout.is_empty());

    let mut invalid_payee_args = database.args(&["client", "allow-payee"]);
    invalid_payee_args.extend([
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
        OsString::from("--network"),
        OsString::from("near:testnet"),
        OsString::from("--asset"),
        OsString::from("INVALID ACCOUNT"),
        OsString::from("--pay-to"),
        OsString::from(TEST_PAYEE),
    ]);
    let invalid_payee = run_admin(invalid_payee_args)?;
    require_failure(&invalid_payee)?;
    assert!(invalid_payee.stdout.is_empty());

    let mut allow_args = database.args(&["client", "allow-payee"]);
    allow_args.extend([
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
        OsString::from("--network"),
        OsString::from("near:testnet"),
        OsString::from("--asset"),
        OsString::from(TEST_ASSET),
        OsString::from("--pay-to"),
        OsString::from(TEST_PAYEE),
    ]);
    for _ in 0..2 {
        let allowed = run_admin(allow_args.clone())?;
        require_success(&allowed)?;
        let allowed_stdout = stdout(&allowed)?;
        assert!(!allowed_stdout.contains(&first_raw_key));
        assert_eq!(
            output_field(&allowed_stdout, "updated_client_id=")?,
            client_id.to_string()
        );
    }
    let payee_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_client_payees \
         WHERE client_id = $1 AND network = 'near:testnet' AND asset = $2 AND pay_to = $3",
    )
    .bind(client_id)
    .bind(TEST_ASSET)
    .bind(TEST_PAYEE)
    .fetch_one(&database.scoped)
    .await?;
    assert_eq!(payee_count, 1);

    let mut invalid_set_budget_args = database.args(&["client", "set-budget"]);
    invalid_set_budget_args.extend([
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
        OsString::from("--daily-yocto-near"),
        OsString::from("-1"),
    ]);
    let invalid_set_budget = run_admin(invalid_set_budget_args)?;
    require_failure(&invalid_set_budget)?;
    assert!(invalid_set_budget.stdout.is_empty());

    let mut set_budget_args = database.args(&["client", "set-budget"]);
    set_budget_args.extend([
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
        OsString::from("--daily-yocto-near"),
        OsString::from("123456"),
    ]);
    let budget_updated = run_admin(set_budget_args)?;
    require_success(&budget_updated)?;
    assert!(!stdout(&budget_updated)?.contains(&first_raw_key));
    let updated_budget: String =
        sqlx::query_scalar("SELECT daily_budget_yocto_near::text FROM api_clients WHERE id = $1")
            .bind(client_id)
            .fetch_one(&database.scoped)
            .await?;
    assert_eq!(updated_budget, "123456");

    let mut rotate_args = database.args(&["client", "rotate"]);
    rotate_args.extend([
        OsString::from("--pepper-file"),
        database.pepper_file.as_os_str().to_owned(),
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
    ]);
    let rotated = run_admin(rotate_args)?;
    require_success(&rotated)?;
    let rotated_stdout = stdout(&rotated)?;
    let second_raw_key = output_field(&rotated_stdout, "api_key=")?.to_owned();
    let second_prefix = validate_api_key(&second_raw_key, "test")?;
    assert_ne!(second_prefix, first_prefix);
    assert_eq!(rotated_stdout.matches(&second_raw_key).count(), 1);
    assert!(!rotated_stdout.contains(&first_raw_key));
    assert!(!stderr(&rotated)?.contains(&second_raw_key));
    assert_eq!(rotated_stdout.lines().count(), 2);

    let key_rows = sqlx::query(
        "SELECT key_prefix, key_digest, status FROM api_keys \
         WHERE client_id = $1 ORDER BY created_at",
    )
    .bind(client_id)
    .fetch_all(&database.scoped)
    .await?;
    assert_eq!(key_rows.len(), 2);
    let active_row = key_rows
        .iter()
        .find(|row| matches!(row.try_get::<String, _>("status").as_deref(), Ok("active")))
        .ok_or_else(|| io::Error::other("rotated active key is missing"))?;
    assert_eq!(
        active_row.try_get::<String, _>("key_prefix")?,
        second_prefix
    );
    let second_digest = digest_api_key(database.pepper.as_bytes(), second_raw_key.as_bytes())?;
    assert_eq!(
        active_row.try_get::<Vec<u8>, _>("key_digest")?,
        second_digest
    );
    let revoked_count = key_rows
        .iter()
        .filter(|row| matches!(row.try_get::<String, _>("status").as_deref(), Ok("revoked")))
        .count();
    assert_eq!(revoked_count, 1);

    let mut revoke_args = database.args(&["client", "revoke"]);
    revoke_args.extend([
        OsString::from("--client-id"),
        OsString::from(client_id.to_string()),
    ]);
    let revoked = run_admin(revoke_args.clone())?;
    require_success(&revoked)?;
    let revoked_stdout = stdout(&revoked)?;
    assert!(!revoked_stdout.contains(&first_raw_key));
    assert!(!revoked_stdout.contains(&second_raw_key));
    assert_eq!(
        output_field(&revoked_stdout, "revoked_client_id=")?,
        client_id.to_string()
    );

    let client_status: String = sqlx::query_scalar("SELECT status FROM api_clients WHERE id = $1")
        .bind(client_id)
        .fetch_one(&database.scoped)
        .await?;
    let active_key_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM api_keys WHERE client_id = $1 AND status = 'active'",
    )
    .bind(client_id)
    .fetch_one(&database.scoped)
    .await?;
    assert_eq!(client_status, "revoked");
    assert_eq!(active_key_count, 0);

    let second_revoke = run_admin(revoke_args)?;
    require_failure(&second_revoke)?;
    assert!(second_revoke.stdout.is_empty());

    let active_client_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM api_clients WHERE status = 'active'")
            .fetch_one(&database.scoped)
            .await?;
    assert_eq!(active_client_count, 0);

    database.cleanup().await
}
