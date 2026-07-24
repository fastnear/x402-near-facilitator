use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use near_crypto::{InMemorySigner, KeyType, SecretKey};
use near_primitives::types::AccountId;
use uuid::Uuid;
use x402_chain_near::{JsonRpcNearRpc, NearChainProvider, NearNetwork, NearRpc, V2NearExact};
use x402_facilitator_local::FacilitatorLocal;
use x402_near_facilitator::auth::{ApiKeyAuthenticator, GeneratedApiKey};
use x402_near_facilitator::chain::ChainProvider;
use x402_near_facilitator::config::{Environment, SecretFiles, ServiceConfig, read_secret};
use x402_near_facilitator::leadership::{LeadershipHandle, ReadinessState};
use x402_near_facilitator::service::{AppState, reconcile};
use x402_near_facilitator::store::{ApiClient, PgStore};
use x402_near_facilitator::telemetry::TelemetryGuard;
use x402_types::chain::{ChainIdPattern, ChainProviderOps, ChainRegistry};
use x402_types::scheme::{SchemeBlueprints, SchemeConfig, SchemeRegistry};

#[derive(Debug, Parser)]
#[command(
    name = "x402-near-admin",
    version,
    about = "Offline administration for the NEAR x402 facilitator"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Apply forward-only `PostgreSQL` migrations.
    Migrate(DatabaseArgs),
    /// Manage API clients and their exact policy.
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },
    /// Reconcile the journal while holding service leadership.
    Reconcile {
        #[arg(long)]
        config: PathBuf,
    },
    /// Generate service keys without printing private material.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
}

#[derive(Debug, Args)]
struct DatabaseArgs {
    #[arg(long)]
    database_url_file: PathBuf,
}

#[derive(Debug, Subcommand)]
enum ClientCommand {
    /// Create an active client and print its API key exactly once.
    Create {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        pepper_file: PathBuf,
        #[arg(long, value_enum)]
        environment: Environment,
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = 60)]
        verify_rate_per_minute: u32,
        #[arg(long, default_value_t = 10)]
        settle_rate_per_minute: u32,
        #[arg(long)]
        daily_yocto_near: Option<String>,
    },
    /// Atomically revoke old keys and print one replacement key.
    Rotate {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        pepper_file: PathBuf,
        #[arg(long)]
        client_id: Uuid,
    },
    /// Immediately revoke a client and all its keys.
    Revoke {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        client_id: Uuid,
    },
    /// Add one exact network/asset/payee allowlist row.
    AllowPayee {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        client_id: Uuid,
        #[arg(long)]
        network: String,
        #[arg(long)]
        asset: String,
        #[arg(long)]
        pay_to: String,
    },
    /// Replace the client's UTC daily sponsorship cap.
    SetBudget {
        #[command(flatten)]
        database: DatabaseArgs,
        #[arg(long)]
        client_id: Uuid,
        #[arg(long)]
        daily_yocto_near: String,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// Write a new ED25519 secret to a mode-0600, create-new file.
    GenerateRelayer {
        #[arg(long)]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Migrate(database) => migrate(&database).await,
        Command::Client { command } => client(command).await,
        Command::Reconcile { config } => reconcile_command(&config).await,
        Command::Key { command } => key(command),
    }
}

async fn migrate(database: &DatabaseArgs) -> Result<()> {
    let store = connect(&database.database_url_file).await?;
    store.migrate().await.context("apply migrations")
}

// Client policy and key mutations share one audited administrative boundary.
#[allow(clippy::too_many_lines)]
async fn client(command: ClientCommand) -> Result<()> {
    match command {
        ClientCommand::Create {
            database,
            pepper_file,
            environment,
            name,
            verify_rate_per_minute,
            settle_rate_per_minute,
            daily_yocto_near,
        } => {
            let store = connect(&database.database_url_file).await?;
            let pepper = read_secret(&pepper_file).context("read HMAC pepper")?;
            let generated = GeneratedApiKey::generate(environment, pepper.as_bytes())
                .context("generate API key")?;
            let (prefix, digest, raw) = generated.into_parts();
            let client = ApiClient {
                id: Uuid::new_v4(),
                name,
                environment: environment_name(environment).to_owned(),
                daily_budget_yocto_near: daily_yocto_near
                    .unwrap_or_else(|| environment.default_client_budget().to_owned()),
                verify_rate_per_minute,
                settle_rate_per_minute,
            };
            validate_decimal(&client.daily_budget_yocto_near)?;
            store
                .create_client(&client, Uuid::new_v4(), &prefix, &digest)
                .await
                .context("create client")?;
            println!("client_id={}", client.id);
            println!("api_key={}", raw.as_str());
            Ok(())
        }
        ClientCommand::Rotate {
            database,
            pepper_file,
            client_id,
        } => {
            let store = connect(&database.database_url_file).await?;
            let environment = parse_environment(
                &store
                    .client_environment(client_id)
                    .await
                    .context("load client environment")?,
            )?;
            let pepper = read_secret(&pepper_file).context("read HMAC pepper")?;
            let generated = GeneratedApiKey::generate(environment, pepper.as_bytes())
                .context("generate API key")?;
            let (prefix, digest, raw) = generated.into_parts();
            store
                .rotate_client_key(client_id, Uuid::new_v4(), &prefix, &digest)
                .await
                .context("rotate client key")?;
            println!("client_id={client_id}");
            println!("api_key={}", raw.as_str());
            Ok(())
        }
        ClientCommand::Revoke {
            database,
            client_id,
        } => {
            let store = connect(&database.database_url_file).await?;
            if !store
                .revoke_client(client_id)
                .await
                .context("revoke client")?
            {
                bail!("active client not found");
            }
            println!("revoked_client_id={client_id}");
            Ok(())
        }
        ClientCommand::AllowPayee {
            database,
            client_id,
            network,
            asset,
            pay_to,
        } => {
            let store = connect(&database.database_url_file).await?;
            let environment = parse_environment(
                &store
                    .client_environment(client_id)
                    .await
                    .context("load client environment")?,
            )?;
            if environment.network() != network {
                bail!("payee network does not match the client environment");
            }
            validate_exact_policy(&network, &asset, &pay_to)?;
            store
                .allow_payee(client_id, &network, &asset, &pay_to)
                .await
                .context("allow payee")?;
            println!("updated_client_id={client_id}");
            Ok(())
        }
        ClientCommand::SetBudget {
            database,
            client_id,
            daily_yocto_near,
        } => {
            validate_decimal(&daily_yocto_near)?;
            let store = connect(&database.database_url_file).await?;
            if !store
                .set_client_budget(client_id, &daily_yocto_near)
                .await
                .context("set client budget")?
            {
                bail!("active client not found");
            }
            println!("updated_client_id={client_id}");
            Ok(())
        }
    }
}

fn key(command: KeyCommand) -> Result<()> {
    match command {
        KeyCommand::GenerateRelayer { output } => {
            let secret = SecretKey::from_random(KeyType::ED25519);
            let public = secret.public_key();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&output)
                .with_context(|| format!("create {}", output.display()))?;
            writeln!(file, "{secret}").context("write relayer credential")?;
            file.sync_all().context("sync relayer credential")?;
            println!("{public}");
            Ok(())
        }
    }
}

async fn reconcile_command(config_path: &Path) -> Result<()> {
    let config = ServiceConfig::load(config_path).context("load service config")?;
    let secrets = SecretFiles::from_environment()
        .context("locate credentials")?
        .load()
        .context("load credentials")?;
    let telemetry =
        TelemetryGuard::initialize(config.environment, None).context("initialize telemetry")?;
    let store = PgStore::connect(
        secrets.database_url.as_str(),
        config.database_max_connections,
    )
    .await
    .context("connect application database")?;
    let auth = ApiKeyAuthenticator::new(
        store.clone(),
        config.environment,
        secrets.api_key_pepper.as_bytes(),
    )
    .context("initialize authentication")?;
    let account =
        AccountId::from_str(&config.relayer_account_id).context("parse relayer account")?;
    let secret =
        SecretKey::from_str(secrets.relayer_key.as_str()).context("parse relayer service key")?;
    let signer = InMemorySigner::from_secret_key(account, secret);
    let primary: Arc<JsonRpcNearRpc> =
        Arc::new(JsonRpcNearRpc::connect(config.primary_rpc_url.as_str()));
    let backup: Arc<JsonRpcNearRpc> =
        Arc::new(JsonRpcNearRpc::connect(config.backup_rpc_url.as_str()));
    let network = match config.environment {
        Environment::Mainnet => NearNetwork::Mainnet,
        Environment::Testnet => NearNetwork::Testnet,
    };
    let provider = NearChainProvider::new(
        network,
        Arc::clone(&primary) as Arc<dyn NearRpc>,
        Arc::new(signer),
    )
    .with_backup_rpc(Arc::clone(&backup) as Arc<dyn NearRpc>);
    let readiness = ReadinessState::default();
    let state = AppState::new(
        config.clone(),
        store,
        auth,
        build_facilitator(provider.clone()),
        ChainProvider::Near(provider),
        readiness.clone(),
        telemetry.metrics(),
    );
    state.refresh_chain_readiness().await;
    let leadership = LeadershipHandle::spawn(
        secrets.database_direct_url,
        &config.network,
        readiness.clone(),
    );
    let acquired = tokio::time::timeout(Duration::from_secs(10), async {
        while !readiness.snapshot().leadership {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .is_ok();
    if !acquired {
        leadership.shutdown().await;
        bail!("another service instance holds settlement leadership");
    }
    reconcile(&state).await.context("reconcile journal")?;
    leadership.shutdown().await;
    println!("reconciliation=complete");
    Ok(())
}

async fn connect(database_url_file: &Path) -> Result<PgStore> {
    let database_url = read_secret(database_url_file).context("read database URL")?;
    PgStore::connect(database_url.as_str(), 2)
        .await
        .context("connect database")
}

fn build_facilitator(provider: NearChainProvider) -> FacilitatorLocal<SchemeRegistry> {
    let chain_id = provider.chain_id();
    let mut providers = HashMap::new();
    providers.insert(chain_id.clone(), provider);
    let chains = ChainRegistry::new(providers);
    let blueprints = SchemeBlueprints::new().and_register(V2NearExact);
    let schemes = vec![SchemeConfig {
        enabled: true,
        id: "v2-near-exact".to_owned(),
        chains: ChainIdPattern::exact(chain_id.namespace, chain_id.reference),
        config: None,
    }];
    FacilitatorLocal::new(SchemeRegistry::build(chains, blueprints, &schemes))
}

fn validate_decimal(value: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("value must be an unsigned base-10 integer");
    }
    Ok(())
}

fn validate_exact_policy(network: &str, asset: &str, pay_to: &str) -> Result<()> {
    if !matches!(network, "near:mainnet" | "near:testnet") {
        bail!("network must be near:mainnet or near:testnet");
    }
    AccountId::from_str(asset).context("parse asset account")?;
    AccountId::from_str(pay_to).context("parse payee account")?;
    Ok(())
}

const fn environment_name(environment: Environment) -> &'static str {
    match environment {
        Environment::Mainnet => "mainnet",
        Environment::Testnet => "testnet",
    }
}

fn parse_environment(value: &str) -> Result<Environment> {
    match value {
        "mainnet" => Ok(Environment::Mainnet),
        "testnet" => Ok(Environment::Testnet),
        _ => Err(anyhow!("invalid client environment")),
    }
}
