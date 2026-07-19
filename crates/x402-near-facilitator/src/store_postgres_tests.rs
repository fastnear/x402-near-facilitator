use std::error::Error;
use std::str::FromStr;
use std::sync::Arc;

use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};
use tokio::sync::Barrier;
use url::Url;
use uuid::Uuid;

use super::{
    ApiClient, ClaimOutcome, NewSettlement, PgStore, PreparedJournalEntry, SettlementState,
    StoreError, TerminalJournalEntry,
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const RESERVATION: &str = "100";
const GLOBAL_LIMIT: &str = "1000";
const CLIENT_LIMIT: &str = "500";

struct TestDatabase {
    store: PgStore,
    pool: PgPool,
    admin: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn new() -> TestResult<Option<Self>> {
        let Some(database_url) = loopback_database_url()? else {
            eprintln!(
                "skipping PostgreSQL integration test: \
                 X402_FACILITATOR_TEST_DATABASE_URL is unset or not loopback"
            );
            return Ok(None);
        };

        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await?;
        let schema = format!("x402_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await?;

        let options =
            PgConnectOptions::from_str(&database_url)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(48)
            .connect_with(options)
            .await?;
        let store = PgStore { pool: pool.clone() };
        store.migrate().await?;

        Ok(Some(Self {
            store,
            pool,
            admin,
            schema,
        }))
    }

    async fn cleanup(self) -> TestResult {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await?;
        self.admin.close().await;
        Ok(())
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

async fn seeded_store(database: &TestDatabase) -> TestResult<(ApiClient, NewSettlement)> {
    let client = ApiClient {
        id: Uuid::new_v4(),
        name: "postgres-test-client".to_owned(),
        environment: "testnet".to_owned(),
        daily_budget_yocto_near: CLIENT_LIMIT.to_owned(),
        verify_rate_per_minute: 60,
        settle_rate_per_minute: 10,
    };
    database
        .store
        .create_client(
            &client,
            Uuid::new_v4(),
            &format!("x402_test_{}", Uuid::new_v4().simple()),
            &[9; 32],
        )
        .await?;
    Ok((client.clone(), settlement_for(&client, 1)))
}

fn settlement_for(client: &ApiClient, seed: u8) -> NewSettlement {
    NewSettlement {
        id: Uuid::new_v4(),
        api_client_id: client.id,
        payment_identifier: Some(format!("payment-id-{}", Uuid::new_v4().simple())),
        payment_hash: [seed; 32],
        request_fingerprint: [seed.wrapping_add(1); 32],
        x402_version: 2,
        scheme: "exact".to_owned(),
        network: "near:testnet".to_owned(),
        asset: "usdc.fakes.testnet".to_owned(),
        pay_to: "merchant.mike.testnet".to_owned(),
        amount: "1000".to_owned(),
        payer: "payer.testnet".to_owned(),
        delegate_public_key: "ed25519:11111111111111111111111111111111".to_owned(),
        delegate_nonce: u64::from(seed).to_string(),
        delegate_max_block_height: "1000".to_owned(),
        policy_snapshot: json!({"test": true, "seed": seed}),
        reservation_yocto_near: RESERVATION.to_owned(),
        global_daily_budget_yocto_near: GLOBAL_LIMIT.to_owned(),
        client_daily_budget_yocto_near: CLIENT_LIMIT.to_owned(),
    }
}

#[tokio::test]
async fn schema_and_active_client_policy_gate_database_readiness() -> TestResult {
    let Some(database) = TestDatabase::new().await? else {
        return Ok(());
    };
    assert!(database.store.schema_compatible().await?);
    assert!(
        database
            .store
            .operationally_ready("near:testnet", "usdc.fakes.testnet")
            .await?
    );

    let (client, _settlement) = seeded_store(&database).await?;
    assert!(
        !database
            .store
            .operationally_ready("near:testnet", "usdc.fakes.testnet")
            .await?
    );
    database
        .store
        .allow_payee(
            client.id,
            "near:testnet",
            "usdc.fakes.testnet",
            "merchant.mike.testnet",
        )
        .await?;
    assert!(
        database
            .store
            .operationally_ready("near:testnet", "usdc.fakes.testnet")
            .await?
    );

    database.cleanup().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn two_hundred_identical_claims_create_one_reservation() -> TestResult {
    let Some(database) = TestDatabase::new().await? else {
        return Ok(());
    };
    let (_client, settlement) = seeded_store(&database).await?;
    let concurrency = 200;
    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let mut tasks = Vec::with_capacity(concurrency);

    for _ in 0..concurrency {
        let store = database.store.clone();
        let candidate = settlement.clone();
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.claim_settlement(&candidate).await
        }));
    }
    barrier.wait().await;

    let mut inserted = 0;
    let mut joined = 0;
    for task in tasks {
        match task.await?? {
            ClaimOutcome::New(record) => {
                inserted += 1;
                assert_eq!(record.state, SettlementState::Reserved);
            }
            ClaimOutcome::Existing(record) => {
                joined += 1;
                assert_eq!(record.id, settlement.id);
            }
            other => {
                return Err(std::io::Error::other(format!(
                    "unexpected concurrent claim outcome: {other:?}"
                ))
                .into());
            }
        }
    }

    assert_eq!(inserted, 1);
    assert_eq!(joined, concurrency - 1);
    let settlement_count: i64 = sqlx::query_scalar("SELECT count(*) FROM settlements")
        .fetch_one(&database.pool)
        .await?;
    let event_count: i64 = sqlx::query_scalar("SELECT count(*) FROM settlement_events")
        .fetch_one(&database.pool)
        .await?;
    let global_reserved: String =
        sqlx::query_scalar("SELECT reserved_yocto_near::text FROM daily_global_sponsorship")
            .fetch_one(&database.pool)
            .await?;
    let client_reserved: String =
        sqlx::query_scalar("SELECT reserved_yocto_near::text FROM daily_client_sponsorship")
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(settlement_count, 1);
    assert_eq!(event_count, 1);
    assert_eq!(global_reserved, RESERVATION);
    assert_eq!(client_reserved, RESERVATION);

    database.cleanup().await
}

#[tokio::test]
async fn identifier_conflicts_and_delegate_duplicates_do_not_reserve_twice() -> TestResult {
    let Some(database) = TestDatabase::new().await? else {
        return Ok(());
    };
    let (client, original) = seeded_store(&database).await?;
    assert!(matches!(
        database.store.claim_settlement(&original).await?,
        ClaimOutcome::New(_)
    ));

    let mut identifier_conflict = settlement_for(&client, 3);
    identifier_conflict.payment_identifier = original.payment_identifier.clone();
    assert!(matches!(
        database
            .store
            .claim_settlement(&identifier_conflict)
            .await?,
        ClaimOutcome::IdentifierConflict
    ));

    let mut duplicate_delegate = settlement_for(&client, 4);
    duplicate_delegate.payment_hash = original.payment_hash;
    assert!(matches!(
        database.store.claim_settlement(&duplicate_delegate).await?,
        ClaimOutcome::DuplicateSettlement
    ));

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM settlements")
        .fetch_one(&database.pool)
        .await?;
    let budget = sqlx::query(
        "SELECT reserved_yocto_near::text AS reserved, spent_yocto_near::text AS spent \
         FROM daily_global_sponsorship",
    )
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(count, 1);
    assert_eq!(budget.try_get::<String, _>("reserved")?, RESERVATION);
    assert_eq!(budget.try_get::<String, _>("spent")?, "0");

    database.cleanup().await
}

#[tokio::test]
async fn client_budget_failure_rolls_back_global_reservation_atomically() -> TestResult {
    let Some(database) = TestDatabase::new().await? else {
        return Ok(());
    };
    let (client, mut settlement) = seeded_store(&database).await?;
    settlement.client_daily_budget_yocto_near = "99".to_owned();
    assert!(matches!(
        database.store.claim_settlement(&settlement).await?,
        ClaimOutcome::BudgetExceeded
    ));

    for table in [
        "settlements",
        "settlement_events",
        "daily_global_sponsorship",
        "daily_client_sponsorship",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(count, 0, "{table} retained a row after rollback");
    }

    let mut first = settlement_for(&client, 5);
    first.reservation_yocto_near = "60".to_owned();
    first.global_daily_budget_yocto_near = "100".to_owned();
    first.client_daily_budget_yocto_near = "100".to_owned();
    assert!(matches!(
        database.store.claim_settlement(&first).await?,
        ClaimOutcome::New(_)
    ));

    let mut second = settlement_for(&client, 6);
    second.reservation_yocto_near = "60".to_owned();
    second.global_daily_budget_yocto_near = "1000".to_owned();
    second.client_daily_budget_yocto_near = "100".to_owned();
    assert!(matches!(
        database.store.claim_settlement(&second).await?,
        ClaimOutcome::BudgetExceeded
    ));

    let mut third = settlement_for(&client, 7);
    third.reservation_yocto_near = "60".to_owned();
    third.global_daily_budget_yocto_near = "100".to_owned();
    third.client_daily_budget_yocto_near = "1000".to_owned();
    assert!(matches!(
        database.store.claim_settlement(&third).await?,
        ClaimOutcome::BudgetExceeded
    ));

    let global_reserved: String =
        sqlx::query_scalar("SELECT reserved_yocto_near::text FROM daily_global_sponsorship")
            .fetch_one(&database.pool)
            .await?;
    let client_reserved: String =
        sqlx::query_scalar("SELECT reserved_yocto_near::text FROM daily_client_sponsorship")
            .fetch_one(&database.pool)
            .await?;
    assert_eq!(global_reserved, "60");
    assert_eq!(client_reserved, "60");
    let settlement_count: i64 = sqlx::query_scalar("SELECT count(*) FROM settlements")
        .fetch_one(&database.pool)
        .await?;
    let event_count: i64 = sqlx::query_scalar("SELECT count(*) FROM settlement_events")
        .fetch_one(&database.pool)
        .await?;
    assert_eq!(settlement_count, 1);
    assert_eq!(event_count, 1);

    database.cleanup().await
}

#[tokio::test]
async fn lifecycle_terminalization_and_replay_are_durable_and_idempotent() -> TestResult {
    let Some(database) = TestDatabase::new().await? else {
        return Ok(());
    };
    let (_client, settlement) = seeded_store(&database).await?;
    assert!(matches!(
        database.store.claim_settlement(&settlement).await?,
        ClaimOutcome::New(_)
    ));

    let invalid_success = TerminalJournalEntry {
        settlement_id: settlement.id,
        state: SettlementState::Succeeded,
        http_status: 200,
        response_bytes: br#"{"success":true}"#.to_vec(),
        error_code: None,
        error_detail: None,
        gas_burnt: Some("3".to_owned()),
        tokens_burnt: Some("7".to_owned()),
        actual_yocto_near: "7".to_owned(),
    };
    assert!(matches!(
        database.store.mark_terminal(&invalid_success).await,
        Err(StoreError::Transition { .. })
    ));

    let prepared = PreparedJournalEntry {
        settlement_id: settlement.id,
        relayer_account_id: "x402-relayer.mike.testnet".to_owned(),
        relayer_public_key: "ed25519:11111111111111111111111111111111".to_owned(),
        relayer_nonce: "42".to_owned(),
        transaction_bytes: vec![1, 2, 3, 4],
        transaction_hash: "transaction-hash".to_owned(),
    };
    database.store.mark_prepared(&prepared).await?;
    database.store.mark_submitted(settlement.id).await?;

    let terminal = TerminalJournalEntry {
        settlement_id: settlement.id,
        state: SettlementState::Succeeded,
        http_status: 200,
        response_bytes: br#"{"success":true,"transaction":"transaction-hash"}"#.to_vec(),
        error_code: None,
        error_detail: None,
        gas_burnt: Some("3".to_owned()),
        tokens_burnt: Some("7".to_owned()),
        actual_yocto_near: "7".to_owned(),
    };
    database.store.mark_terminal(&terminal).await?;
    database.store.mark_terminal(&terminal).await?;

    let replay = database.store.claim_settlement(&settlement).await?;
    let ClaimOutcome::Existing(record) = replay else {
        return Err(
            std::io::Error::other("terminal settlement was not replayed as existing").into(),
        );
    };
    assert_eq!(record.state, SettlementState::Succeeded);
    assert_eq!(record.terminal_http_status, Some(200));
    assert_eq!(
        record.terminal_response_bytes.as_deref(),
        Some(terminal.response_bytes.as_slice())
    );
    assert_eq!(
        record.outer_transaction_bytes.as_deref(),
        Some(prepared.transaction_bytes.as_slice())
    );

    let mut mismatched_replay = terminal.clone();
    mismatched_replay.response_bytes = br#"{"success":false}"#.to_vec();
    assert!(matches!(
        database.store.mark_terminal(&mismatched_replay).await,
        Err(StoreError::Transition { .. })
    ));

    let ledger = sqlx::query(
        "SELECT reserved_yocto_near::text AS reserved, spent_yocto_near::text AS spent \
         FROM daily_global_sponsorship",
    )
    .fetch_one(&database.pool)
    .await?;
    assert_eq!(ledger.try_get::<String, _>("reserved")?, "0");
    assert_eq!(ledger.try_get::<String, _>("spent")?, "7");

    let states: Vec<String> = sqlx::query_scalar(
        "SELECT to_state FROM settlement_events WHERE settlement_id = $1 ORDER BY id",
    )
    .bind(settlement.id)
    .fetch_all(&database.pool)
    .await?;
    assert_eq!(
        states,
        vec!["reserved", "prepared", "submitted", "succeeded"]
    );

    database.cleanup().await
}
