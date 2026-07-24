use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use near_crypto::{InMemorySigner, SecretKey};
use near_primitives::types::AccountId;
use x402_chain_near::{JsonRpcNearRpc, NearChainProvider, NearNetwork, NearRpc, V2NearExact};
use x402_facilitator_local::{FacilitatorLocal, util::SigDown};
use x402_near_facilitator::auth::ApiKeyAuthenticator;
use x402_near_facilitator::chain::ChainProvider;
use x402_near_facilitator::config::{
    ChainKind, Environment, OtelConfig, SecretFiles, ServiceConfig,
};
use x402_near_facilitator::leadership::{LeadershipHandle, ReadinessState};
use x402_near_facilitator::service::{AppState, reconcile, router};
use x402_near_facilitator::store::PgStore;
use x402_near_facilitator::telemetry::TelemetryGuard;
use x402_types::chain::{ChainIdPattern, ChainProviderOps, ChainRegistry};
use x402_types::scheme::{SchemeBlueprints, SchemeConfig, SchemeRegistry};

#[derive(Debug, Parser)]
#[command(
    name = "x402-near-facilitator",
    version,
    about = "Durable NEAR x402 facilitator"
)]
struct Cli {
    /// Non-secret JSON service configuration.
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
// Startup is kept as one ordered fail-closed sequence so no listener can
// become ready before configuration, storage, leadership, and recovery.
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = ServiceConfig::load(&cli.config).context("load service config")?;
    let secret_files = SecretFiles::from_environment().context("locate credentials")?;
    let secrets = secret_files.load().context("load credentials")?;
    let otel = OtelConfig::from_environment().context("load telemetry configuration")?;
    let telemetry = TelemetryGuard::initialize(config.environment, otel.as_ref())
        .context("initialize telemetry")?;

    let store = PgStore::connect(
        secrets.database_url.as_str(),
        config.database_max_connections,
    )
    .await
    .context("connect application database")?;
    // Migrations are intentionally never run by the service process.
    ensure!(
        store
            .schema_compatible()
            .await
            .context("validate database schema")?,
        "database schema is missing, incomplete, or incompatible"
    );
    let auth = ApiKeyAuthenticator::new(
        store.clone(),
        config.environment,
        secrets.api_key_pepper.as_bytes(),
    )
    .context("initialize API authentication")?;

    // The settlement provider and relayer-key algorithm are chain-specific; only
    // NEAR (ed25519) is wired today. `validate()` already rejects an eip155
    // config at load; this guards the NEAR construction below and marks where the
    // eip155 (secp256k1 + EVM provider) branch slots in.
    ensure!(
        config.chain_kind == ChainKind::Near,
        "eip155 (EVM) settlement is not yet available in this build"
    );
    let relayer_account =
        AccountId::from_str(&config.relayer_account_id).context("parse relayer account")?;
    let secret_key =
        SecretKey::from_str(secrets.relayer_key.as_str()).context("parse relayer service key")?;
    let signer = InMemorySigner::from_secret_key(relayer_account, secret_key);
    let relayer_public_key = signer.public_key().to_string();
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
    let facilitator = build_facilitator(provider.clone());

    store
        .upsert_relayer(
            &config.network,
            &config.relayer_account_id,
            &relayer_public_key,
        )
        .await
        .context("register relayer identity")?;
    let readiness = ReadinessState::default();
    let state = AppState::new(
        config.clone(),
        store.clone(),
        auth,
        facilitator,
        ChainProvider::Near(provider.clone()),
        readiness.clone(),
        telemetry.metrics(),
    );
    state.refresh_chain_readiness().await;
    let leadership = LeadershipHandle::spawn(
        secrets.database_direct_url,
        &config.network,
        readiness.clone(),
    );

    let reconciliation_state = state.clone();
    let reconciliation_task = tokio::spawn(async move {
        loop {
            let snapshot = reconciliation_state.readiness().snapshot();
            if snapshot.leadership
                && !snapshot.reconciliation
                && reconcile(&reconciliation_state).await.is_err()
            {
                tracing::warn!(event = "startup_reconciliation_failed");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
    let monitor_state = state.clone();
    let readiness_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            monitor_state.refresh_chain_readiness().await;
        }
    });

    let listener = tokio::net::TcpListener::bind(config.bind_address)
        .await
        .context("bind HTTP listener")?;
    let signals = SigDown::try_new().context("register shutdown signals")?;
    let cancellation = signals.cancellation_token();
    tracing::info!(
        event = "service_started",
        environment = ?config.environment,
        network = %config.network,
        bind = %config.bind_address,
        version = x402_near_facilitator::VERSION,
    );
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            cancellation.cancelled().await;
        })
        .await
        .context("serve HTTP")?;

    reconciliation_task.abort();
    readiness_task.abort();
    leadership.shutdown().await;
    Ok(())
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
