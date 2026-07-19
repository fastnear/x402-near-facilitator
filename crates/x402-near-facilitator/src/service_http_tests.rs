//! Offline and loopback-only HTTP protocol conformance tests.

use std::error::Error;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use near_crypto::{InMemorySigner, KeyType, SecretKey, Signer};
use near_primitives::action::delegate::{DelegateAction, NonDelegateAction, SignedDelegateAction};
use near_primitives::action::{Action, FunctionCallAction};
use near_primitives::borsh;
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::{SignedTransaction, Transaction};
use near_primitives::types::{AccountId, Balance, Gas};
use near_primitives::views::{
    AccessKeyPermissionView, AccessKeyView, AccountView, ExecutionMetadataView,
    ExecutionOutcomeView, ExecutionOutcomeWithIdView, ExecutionStatusView,
    FinalExecutionOutcomeView, FinalExecutionStatus,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt as _;
use url::Url;
use uuid::Uuid;
use x402_chain_near::{
    FinalBlock, NearChainProvider, NearNetwork, NearRpc, NearRpcError, TransactionLookup,
    V2NearExact,
};
use x402_facilitator_local::FacilitatorLocal;
use x402_types::chain::{ChainIdPattern, ChainProviderOps, ChainRegistry};
use x402_types::scheme::{SchemeBlueprints, SchemeConfig, SchemeRegistry};

use super::{AppState, router};
use crate::VERSION;
use crate::auth::{ApiKeyAuthenticator, digest_api_key};
use crate::config::{
    Environment, PaymentIdentifierConfig, RequestLimits, ServiceConfig, SponsorshipConfig,
};
use crate::leadership::ReadinessState;
use crate::store::{ApiClient, PgStore};
use crate::telemetry::{Metrics, TelemetryGuard};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const TESTNET_USDC: &str = "3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af";
const TEST_PAYEE: &str = "merchant.mike.testnet";
const TEST_PAYER: &str = "x402-http-payer.testnet";
const TEST_RELAYER: &str = "x402-http-relayer.testnet";
const TEST_PEPPER: [u8; 32] = [0x42; 32];

#[derive(Debug)]
struct MockRpc {
    block: FinalBlock,
    sends: AtomicUsize,
    payer_nonce: AtomicU64,
    relayer_nonce: AtomicU64,
}

impl MockRpc {
    fn new() -> Self {
        Self {
            block: FinalBlock {
                height: 1_000,
                hash: CryptoHash::hash_bytes(b"http-conformance-final-block"),
            },
            sends: AtomicUsize::new(0),
            payer_nonce: AtomicU64::new(0),
            relayer_nonce: AtomicU64::new(0),
        }
    }

    fn account() -> AccountView {
        AccountView {
            amount: Balance::from_yoctonear(10_u128.pow(24)),
            locked: Balance::ZERO,
            code_hash: CryptoHash::hash_bytes(b"deployed-contract"),
            storage_usage: 0,
            storage_paid_at: 0,
            global_contract_hash: None,
            global_contract_account_id: None,
        }
    }

    fn ensure_pinned(&self, block_hash: CryptoHash) -> Result<(), NearRpcError> {
        if block_hash == self.block.hash {
            Ok(())
        } else {
            Err(NearRpcError::InvalidResponse(
                "HTTP conformance query was not pinned",
            ))
        }
    }
}

#[async_trait]
impl NearRpc for MockRpc {
    async fn network_id(&self) -> Result<String, NearRpcError> {
        Ok("testnet".to_owned())
    }

    async fn final_block(&self) -> Result<FinalBlock, NearRpcError> {
        Ok(self.block)
    }

    async fn view_account(
        &self,
        block_hash: CryptoHash,
        _account_id: AccountId,
    ) -> Result<AccountView, NearRpcError> {
        self.ensure_pinned(block_hash)?;
        Ok(Self::account())
    }

    async fn view_access_key(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
        _public_key: near_crypto::PublicKey,
    ) -> Result<AccessKeyView, NearRpcError> {
        self.ensure_pinned(block_hash)?;
        Ok(AccessKeyView {
            nonce: if account_id.as_str() == TEST_RELAYER {
                self.relayer_nonce.load(Ordering::SeqCst)
            } else {
                self.payer_nonce.load(Ordering::SeqCst)
            },
            permission: AccessKeyPermissionView::FullAccess,
        })
    }

    async fn call_function(
        &self,
        block_hash: CryptoHash,
        _contract_id: AccountId,
        method_name: String,
        _args: Vec<u8>,
    ) -> Result<Vec<u8>, NearRpcError> {
        self.ensure_pinned(block_hash)?;
        match method_name.as_str() {
            "ft_balance_of" => Ok(br#""1000000000""#.to_vec()),
            "storage_balance_of" => Ok(b"{}".to_vec()),
            _ => Err(NearRpcError::MethodNotFound),
        }
    }

    async fn send_transaction_final(
        &self,
        signed_transaction: SignedTransaction,
    ) -> Result<TransactionLookup, NearRpcError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        let transaction = &signed_transaction.transaction;
        self.relayer_nonce
            .store(transaction.nonce().nonce(), Ordering::SeqCst);
        let Transaction::V0(transaction) = transaction else {
            return Err(NearRpcError::InvalidResponse(
                "HTTP fixture expected transaction V0",
            ));
        };
        let Some(Action::Delegate(delegate)) = transaction.actions.first() else {
            return Err(NearRpcError::InvalidResponse(
                "HTTP fixture expected delegate action",
            ));
        };
        self.payer_nonce
            .store(delegate.delegate_action.nonce, Ordering::SeqCst);
        Ok(TransactionLookup::Final(Box::new(successful_outcome(
            signed_transaction,
        )?)))
    }

    async fn transaction_status_final(
        &self,
        _transaction_hash: CryptoHash,
        _signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError> {
        Ok(TransactionLookup::Unknown)
    }
}

fn outcome(
    id: CryptoHash,
    executor_id: AccountId,
    receipt_ids: Vec<CryptoHash>,
    status: ExecutionStatusView,
) -> ExecutionOutcomeWithIdView {
    ExecutionOutcomeWithIdView {
        proof: Vec::new(),
        block_hash: CryptoHash::hash_bytes(b"http-outcome-block"),
        id,
        outcome: ExecutionOutcomeView {
            logs: Vec::new(),
            receipt_ids,
            gas_burnt: Gas::from_gas(0),
            tokens_burnt: Balance::ZERO,
            executor_id,
            status,
            metadata: ExecutionMetadataView::default(),
        },
    }
}

fn successful_outcome(
    signed_transaction: SignedTransaction,
) -> Result<FinalExecutionOutcomeView, NearRpcError> {
    let relayer = TEST_RELAYER
        .parse()
        .map_err(|_| NearRpcError::InvalidResponse("invalid test relayer"))?;
    let payer = TEST_PAYER
        .parse()
        .map_err(|_| NearRpcError::InvalidResponse("invalid test payer"))?;
    let asset = TESTNET_USDC
        .parse()
        .map_err(|_| NearRpcError::InvalidResponse("invalid test asset"))?;
    let transaction_hash = signed_transaction.get_hash();
    let delegate_id = CryptoHash::hash_bytes(b"http-delegate-receipt");
    let token_id = CryptoHash::hash_bytes(b"http-token-receipt");
    Ok(FinalExecutionOutcomeView {
        status: FinalExecutionStatus::SuccessValue(Vec::new()),
        transaction: signed_transaction.into(),
        transaction_outcome: outcome(
            transaction_hash,
            relayer,
            vec![delegate_id],
            ExecutionStatusView::SuccessReceiptId(delegate_id),
        ),
        receipts_outcome: vec![
            outcome(
                delegate_id,
                payer,
                vec![token_id],
                ExecutionStatusView::SuccessReceiptId(token_id),
            ),
            outcome(
                token_id,
                asset,
                Vec::new(),
                ExecutionStatusView::SuccessValue(Vec::new()),
            ),
        ],
    })
}

struct TestApplication {
    router: Router,
    readiness: ReadinessState,
    rpc: Arc<MockRpc>,
    relayer_public_key: String,
}

fn test_signer(account_id: &str) -> TestResult<Signer> {
    let account_id = account_id.parse::<AccountId>()?;
    let secret_key = SecretKey::from_random(KeyType::ED25519);
    Ok(InMemorySigner::from_secret_key(account_id, secret_key))
}

fn service_config() -> TestResult<ServiceConfig> {
    Ok(ServiceConfig {
        environment: Environment::Testnet,
        network: "near:testnet".to_owned(),
        bind_address: "127.0.0.1:0".parse()?,
        primary_rpc_url: Url::parse("https://primary.test.invalid")?,
        backup_rpc_url: Url::parse("https://backup.test.invalid")?,
        asset: TESTNET_USDC.to_owned(),
        asset_symbol: "USDC".to_owned(),
        minimum_amount: "1000".to_owned(),
        relayer_account_id: TEST_RELAYER.to_owned(),
        max_inner_gas: 30_000_000_000_000,
        database_max_connections: 16,
        request_limits: RequestLimits {
            body_bytes: 65_536,
            verify_per_minute: 100,
            settle_per_minute: 100,
            verify_timeout_seconds: 15,
            settle_timeout_seconds: 5,
            max_concurrent_verify: 64,
        },
        sponsorship: SponsorshipConfig {
            global_daily_yocto_near: "1000000".to_owned(),
            default_client_daily_yocto_near: "100000".to_owned(),
            reservation_yocto_near: "100".to_owned(),
            balance_warning_yocto_near: "200".to_owned(),
            balance_hard_stop_yocto_near: "100".to_owned(),
        },
        payment_identifier: PaymentIdentifierConfig::default(),
    })
}

fn build_facilitator(provider: NearChainProvider) -> FacilitatorLocal<SchemeRegistry> {
    let chain_id = provider.chain_id();
    let mut providers = std::collections::HashMap::new();
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

fn build_application(store: PgStore, metrics: Metrics) -> TestResult<TestApplication> {
    let config = service_config()?;
    let rpc = Arc::new(MockRpc::new());
    let primary: Arc<dyn NearRpc> = rpc.clone();
    let backup: Arc<dyn NearRpc> = rpc.clone();
    let relayer_signer = test_signer(TEST_RELAYER)?;
    let relayer_public_key = relayer_signer.public_key().to_string();
    let provider = NearChainProvider::new(NearNetwork::Testnet, primary, Arc::new(relayer_signer))
        .with_backup_rpc(backup);
    let facilitator = build_facilitator(provider.clone());
    let auth = ApiKeyAuthenticator::new(store.clone(), Environment::Testnet, TEST_PEPPER)?;
    let readiness = ReadinessState::default();
    let state = AppState::new(
        config,
        store,
        auth,
        facilitator,
        provider,
        readiness.clone(),
        metrics,
    );
    Ok(TestApplication {
        router: router(state),
        readiness,
        rpc,
        relayer_public_key,
    })
}

fn valid_request(signer: &Signer, nonce: u64, identifier: Option<&str>) -> TestResult<Value> {
    let transfer = Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: "ft_transfer".to_owned(),
        args: serde_json::to_vec(&json!({
            "receiver_id": TEST_PAYEE,
            "amount": "1000",
        }))?,
        gas: Gas::from_gas(30_000_000_000_000),
        deposit: Balance::from_yoctonear(1),
    }));
    let action = NonDelegateAction::try_from(transfer)?;
    let delegate = DelegateAction {
        sender_id: TEST_PAYER.parse()?,
        receiver_id: TESTNET_USDC.parse()?,
        actions: vec![action],
        nonce,
        max_block_height: 1_050,
        public_key: signer.public_key(),
    };
    let encoded = STANDARD.encode(borsh::to_vec(&SignedDelegateAction::sign(
        signer, delegate,
    ))?);
    let requirements = json!({
        "scheme": "exact",
        "network": "near:testnet",
        "amount": "1000",
        "payTo": TEST_PAYEE,
        "maxTimeoutSeconds": 60,
        "asset": TESTNET_USDC,
    });
    let mut payment_payload = json!({
        "x402Version": 2,
        "accepted": requirements.clone(),
        "payload": {
            "signedDelegateAction": encoded,
        },
    });
    if let Some(identifier) = identifier {
        payment_payload["extensions"] = json!({
            "payment-identifier": {
                "info": {
                    "required": true,
                    "id": identifier,
                },
                "schema": {},
            },
        });
    }
    Ok(json!({
        "x402Version": 2,
        "paymentPayload": payment_payload,
        "paymentRequirements": requirements,
    }))
}

fn invalid_version_request(signer: &Signer) -> TestResult<Value> {
    invalid_version_request_with_nonce(signer, 1)
}

fn invalid_version_request_with_nonce(signer: &Signer, nonce: u64) -> TestResult<Value> {
    let mut request = valid_request(signer, nonce, None)?;
    request["x402Version"] = json!(1);
    request["paymentPayload"]["x402Version"] = json!(1);
    Ok(request)
}

fn api_key(seed: u8) -> (String, String) {
    let prefix = format!("x402_test_{}", hex::encode([seed; 12]));
    let raw = format!("{prefix}.{}", hex::encode([seed.wrapping_add(1); 32]));
    (prefix, raw)
}

async fn seed_client(
    store: &PgStore,
    seed: u8,
    verify_rate: u32,
    settle_rate: u32,
) -> TestResult<(ApiClient, String)> {
    let client = ApiClient {
        id: Uuid::new_v4(),
        name: format!("http-conformance-{seed}"),
        environment: "testnet".to_owned(),
        daily_budget_yocto_near: "100000".to_owned(),
        verify_rate_per_minute: verify_rate,
        settle_rate_per_minute: settle_rate,
    };
    let (prefix, raw) = api_key(seed);
    let digest = digest_api_key(&TEST_PEPPER, raw.as_bytes())?;
    store
        .create_client(&client, Uuid::new_v4(), &prefix, &digest)
        .await?;
    store
        .allow_payee(client.id, "near:testnet", TESTNET_USDC, TEST_PAYEE)
        .await?;
    Ok((client, raw))
}

fn http_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    content_type: Option<&str>,
    api_key: Option<&str>,
    bearer: Option<&str>,
) -> TestResult<Request<Body>> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(content_type) = content_type {
        builder = builder.header(CONTENT_TYPE, content_type);
    }
    if let Some(api_key) = api_key {
        builder = builder.header("x-api-key", api_key);
    }
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    Ok(builder.body(Body::from(body))?)
}

struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl TestResponse {
    fn json(&self) -> TestResult<Value> {
        Ok(serde_json::from_slice(&self.bytes)?)
    }
}

async fn call(router: &Router, request: Request<Body>) -> TestResult<TestResponse> {
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1_048_576).await?.to_vec();
    Ok(TestResponse {
        status,
        headers,
        bytes,
    })
}

fn ready(readiness: &ReadinessState) {
    readiness.set_leadership(true);
    readiness.set_reconciliation(true);
    readiness.set_rpc(true);
    readiness.set_relayer(true);
}

struct TestDatabase {
    store: PgStore,
    pool: PgPool,
    admin: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn from_explicit_environment() -> TestResult<Option<Self>> {
        let Ok(raw) = std::env::var("X402_FACILITATOR_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping protected HTTP checks: X402_FACILITATOR_TEST_DATABASE_URL is unset"
            );
            return Ok(None);
        };
        let url = Url::parse(&raw)?;
        if !matches!(url.scheme(), "postgres" | "postgresql")
            || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
        {
            return Err(std::io::Error::other(
                "X402_FACILITATOR_TEST_DATABASE_URL must be a loopback PostgreSQL URL",
            )
            .into());
        }
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&raw)
            .await?;
        let schema = format!("x402_http_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await?;
        let options = PgConnectOptions::from_str(&raw)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(24)
            .connect_with(options)
            .await?;
        let store = PgStore::from_explicit_test_pool(pool.clone());
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

fn disconnected_store() -> PgStore {
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(1)
        .username("x402_explicit_disconnected_test")
        .database("x402_explicit_disconnected_test");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy_with(options);
    PgStore::from_explicit_test_pool(pool)
}

async fn assert_public_contract(application: &TestApplication, payer: &Signer) -> TestResult {
    let health = call(
        &application.router,
        http_request(Method::GET, "/healthz", Vec::new(), None, None, None)?,
    )
    .await?;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(
        health.json()?,
        json!({
            "status": "ok",
            "service": "x402-near-facilitator",
            "version": VERSION,
        })
    );

    let readiness = call(
        &application.router,
        http_request(Method::GET, "/readyz", Vec::new(), None, None, None)?,
    )
    .await?;
    assert_eq!(readiness.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        readiness
            .headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    let readiness_json = readiness.json()?;
    assert_eq!(readiness_json["ready"], false);
    assert_eq!(readiness_json["checks"]["database"], "not_ready");
    assert_eq!(
        readiness_json["checks"]
            .as_object()
            .map(serde_json::Map::len),
        Some(5)
    );
    assert_eq!(
        readiness_json.as_object().map(serde_json::Map::len),
        Some(2)
    );

    let supported = call(
        &application.router,
        http_request(Method::GET, "/supported", Vec::new(), None, None, None)?,
    )
    .await?;
    assert_eq!(supported.status, StatusCode::OK);
    assert!(supported.headers.get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    let supported_json = supported.json()?;
    assert_eq!(supported_json["kinds"][0]["x402Version"], 2);
    assert_eq!(supported_json["kinds"][0]["scheme"], "exact");
    assert_eq!(supported_json["kinds"][0]["network"], "near:testnet");
    assert_eq!(supported_json["extensions"], json!(["payment-identifier"]));
    assert_eq!(supported_json["signers"]["near:testnet"][0], TEST_RELAYER);

    let request = serde_json::to_vec(&invalid_version_request(payer)?)?;
    let unauthorized = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            request.clone(),
            Some("application/json"),
            None,
            None,
        )?,
    )
    .await?;
    assert_eq!(unauthorized.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized.json()?["error"]["code"], "invalid_api_key");

    let (_, first) = api_key(90);
    let (_, second) = api_key(91);
    let conflicting = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            request,
            Some("application/json"),
            Some(&first),
            Some(&second),
        )?,
    )
    .await?;
    assert_eq!(conflicting.status, StatusCode::UNAUTHORIZED);

    let options = call(
        &application.router,
        http_request(Method::OPTIONS, "/verify", Vec::new(), None, None, None)?,
    )
    .await?;
    assert_eq!(options.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(options.headers.get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn assert_protected_contract(
    database: &TestDatabase,
    metrics: Metrics,
    payer: &Signer,
) -> TestResult {
    let (_client, key) = seed_client(&database.store, 1, 100, 100).await?;
    let application = build_application(database.store.clone(), metrics)?;
    database
        .store
        .upsert_relayer(
            "near:testnet",
            TEST_RELAYER,
            &application.relayer_public_key,
        )
        .await?;

    let unavailable = call(
        &application.router,
        http_request(Method::GET, "/readyz", Vec::new(), None, None, None)?,
    )
    .await?;
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    ready(&application.readiness);
    let available = call(
        &application.router,
        http_request(Method::GET, "/readyz", Vec::new(), None, None, None)?,
    )
    .await?;
    assert_eq!(available.status, StatusCode::OK);
    assert_eq!(available.json()?["ready"], true);

    let invalid_version = invalid_version_request(payer)?;
    let invalid_bytes = serde_json::to_vec(&invalid_version)?;
    for bearer_only in [false, true] {
        let response = call(
            &application.router,
            http_request(
                Method::POST,
                "/verify",
                invalid_bytes.clone(),
                Some("application/json; charset=utf-8"),
                (!bearer_only).then_some(key.as_str()),
                bearer_only.then_some(key.as_str()),
            )?,
        )
        .await?;
        assert_eq!(response.status, StatusCode::OK);
        let value = response.json()?;
        assert_eq!(value["isValid"], false);
        assert_eq!(value["invalidReason"], "invalid_x402_version");
        assert_eq!(value.as_object().map(serde_json::Map::len), Some(2));
    }

    let valid_verification = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            serde_json::to_vec(&valid_request(payer, 50, None)?)?,
            Some("application/json"),
            Some(&key),
            Some(&key),
        )?,
    )
    .await?;
    assert_eq!(valid_verification.status, StatusCode::OK);
    assert_eq!(
        valid_verification.json()?,
        json!({"isValid": true, "payer": TEST_PAYER})
    );

    let missing_media = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            invalid_bytes.clone(),
            None,
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(missing_media.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let wrong_media = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            invalid_bytes.clone(),
            Some("text/plain"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(wrong_media.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let malformed = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            b"{".to_vec(),
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);

    let mut wrong_amount_type = invalid_version.clone();
    wrong_amount_type["paymentRequirements"]["amount"] = json!(1000);
    wrong_amount_type["paymentPayload"]["accepted"]["amount"] = json!(1000);
    let wrong_shape = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            serde_json::to_vec(&wrong_amount_type)?,
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(wrong_shape.status, StatusCode::BAD_REQUEST);

    let too_large = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            vec![b' '; 65_537],
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(too_large.status, StatusCode::PAYLOAD_TOO_LARGE);

    let mut bad_identifier = invalid_version.clone();
    bad_identifier["paymentPayload"]["extensions"] = json!({
        "payment-identifier": {
            "info": {"required": true, "id": "too-short"},
        },
    });
    let bad_identifier = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            serde_json::to_vec(&bad_identifier)?,
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(bad_identifier.status, StatusCode::BAD_REQUEST);

    let settle_failure = call(
        &application.router,
        http_request(
            Method::POST,
            "/settle",
            invalid_bytes.clone(),
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(settle_failure.status, StatusCode::OK);
    let settle_failure_json = settle_failure.json()?;
    assert_eq!(settle_failure_json["success"], false);
    assert_eq!(settle_failure_json["errorReason"], "invalid_x402_version");
    assert_eq!(settle_failure_json["transaction"], "");
    assert_eq!(settle_failure_json["network"], "near:testnet");
    assert_eq!(
        settle_failure_json.as_object().map(serde_json::Map::len),
        Some(4)
    );

    let (_rate_client, rate_key) = seed_client(&database.store, 2, 2, 100).await?;
    for expected in [
        StatusCode::OK,
        StatusCode::OK,
        StatusCode::TOO_MANY_REQUESTS,
    ] {
        let response = call(
            &application.router,
            http_request(
                Method::POST,
                "/verify",
                invalid_bytes.clone(),
                Some("application/json"),
                Some(&rate_key),
                None,
            )?,
        )
        .await?;
        assert_eq!(response.status, expected);
        if expected == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(
                response
                    .headers
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
                Some("1")
            );
        }
    }

    let (revoked_client, revoked_key) = seed_client(&database.store, 3, 100, 100).await?;
    let before_revoke = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            invalid_bytes.clone(),
            Some("application/json"),
            None,
            Some(&revoked_key),
        )?,
    )
    .await?;
    assert_eq!(before_revoke.status, StatusCode::OK);
    assert!(database.store.revoke_client(revoked_client.id).await?);
    let after_revoke = call(
        &application.router,
        http_request(
            Method::POST,
            "/verify",
            invalid_bytes.clone(),
            Some("application/json"),
            Some(&revoked_key),
            None,
        )?,
    )
    .await?;
    assert_eq!(after_revoke.status, StatusCode::UNAUTHORIZED);

    let identifier = "payment_http_0000000000000001";
    let settlement = valid_request(payer, 1, Some(identifier))?;
    let settlement_bytes = serde_json::to_vec(&settlement)?;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let router = application.router.clone();
        let body = settlement_bytes.clone();
        let key = key.clone();
        tasks.spawn(async move {
            let request = http_request(
                Method::POST,
                "/settle",
                body,
                Some("application/json"),
                Some(&key),
                None,
            )?;
            call(&router, request).await
        });
    }
    let mut terminal_bytes: Option<Vec<u8>> = None;
    while let Some(joined) = tasks.join_next().await {
        let response = joined??;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json()?["success"], true);
        if let Some(expected) = &terminal_bytes {
            assert_eq!(&response.bytes, expected);
        } else {
            terminal_bytes = Some(response.bytes);
        }
    }
    assert_eq!(application.rpc.sends.load(Ordering::SeqCst), 1);

    let replay = call(
        &application.router,
        http_request(
            Method::POST,
            "/settle",
            settlement_bytes,
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(replay.bytes, terminal_bytes.unwrap_or_default());
    assert_eq!(application.rpc.sends.load(Ordering::SeqCst), 1);

    let mut conflict = settlement.clone();
    conflict["paymentRequirements"]["extra"] = json!({"changed": true});
    let conflict = call(
        &application.router,
        http_request(
            Method::POST,
            "/settle",
            serde_json::to_vec(&conflict)?,
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(
        conflict.json()?["error"]["code"],
        "payment_identifier_conflict"
    );

    let mut duplicate = settlement;
    duplicate["paymentPayload"]["extensions"]["payment-identifier"]["info"]["id"] =
        json!("payment_http_0000000000000002");
    let duplicate = call(
        &application.router,
        http_request(
            Method::POST,
            "/settle",
            serde_json::to_vec(&duplicate)?,
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(duplicate.status, StatusCode::OK);
    assert_eq!(duplicate.json()?["errorReason"], "duplicate_settlement");

    let (_official_client, official_key) = seed_client(&database.store, 4, 100, 100).await?;
    let (_official_rate_client, official_rate_key) =
        seed_client(&database.store, 5, 2, 100).await?;
    let (_, invalid_official_key) = api_key(99);
    let official_valid = valid_request(payer, 2, Some("payment_official_000000000001"))?;
    let official_invalid_version = invalid_version_request_with_nonce(payer, 99)?;
    let mut official_conflict = official_valid.clone();
    official_conflict["paymentRequirements"]["extra"] = json!({"changed": true});
    let mut official_duplicate = official_valid.clone();
    official_duplicate["paymentPayload"]["extensions"]["payment-identifier"]["info"]["id"] =
        json!("payment_official_000000000002");
    let official_scenario = json!({
        "apiKey": official_key,
        "rateApiKey": official_rate_key,
        "invalidApiKey": invalid_official_key,
        "expectedPayer": TEST_PAYER,
        "invalidVersion": official_invalid_version,
        "valid": official_valid,
        "conflict": official_conflict,
        "duplicate": official_duplicate,
    });
    let official_client_ran =
        run_official_client_if_requested(&application.router, &official_scenario).await?;
    assert_eq!(
        application.rpc.sends.load(Ordering::SeqCst),
        if official_client_ran { 2 } else { 1 }
    );

    assert!(
        database
            .store
            .set_client_budget(
                database
                    .store
                    .lookup_api_key(&api_key(1).0)
                    .await?
                    .ok_or_else(|| std::io::Error::other("test client disappeared"))?
                    .client
                    .id,
                "99",
            )
            .await?
    );
    let budget_request = valid_request(payer, 3, Some("payment_http_0000000000000003"))?;
    let budget = call(
        &application.router,
        http_request(
            Method::POST,
            "/settle",
            serde_json::to_vec(&budget_request)?,
            Some("application/json"),
            Some(&key),
            None,
        )?,
    )
    .await?;
    assert_eq!(budget.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        budget.json()?["error"]["code"],
        "sponsorship_budget_exhausted"
    );

    Ok(())
}

async fn run_official_client_if_requested(router: &Router, scenario: &Value) -> TestResult<bool> {
    if std::env::var("X402_RUN_NODE_CLIENT_CONFORMANCE").as_deref() != Ok("1") {
        eprintln!(
            "skipping official HTTPFacilitatorClient check: \
             X402_RUN_NODE_CLIENT_CONFORMANCE is not 1"
        );
        return Ok(false);
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| std::io::Error::other("workspace root is unavailable"))?;
    let harness = root.join("conformance/http-client/check.mjs");
    let installed = root.join("conformance/http-client/node_modules/@x402/core");
    if !installed.is_dir() {
        return Err(std::io::Error::other(
            "run `npm --prefix conformance/http-client ci` before the official-client check",
        )
        .into());
    }
    let scenario_path = std::env::temp_dir().join(format!(
        "x402-http-client-scenario-{}.json",
        Uuid::new_v4().simple()
    ));
    let mut scenario_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&scenario_path)?;
    scenario_file.write_all(&serde_json::to_vec(scenario)?)?;
    scenario_file.sync_all()?;
    drop(scenario_file);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(axum::serve(listener, router.clone()).into_future());
    let harness_path = harness.clone();
    let scenario_path_for_process = scenario_path.clone();
    let output_result = tokio::task::spawn_blocking(move || {
        Command::new("node")
            .arg(harness_path)
            .arg(format!("http://{address}"))
            .arg(scenario_path_for_process)
            .output()
    })
    .await?;
    server.abort();
    std::fs::remove_file(scenario_path)?;
    let output = output_result?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "official HTTPFacilitatorClient harness failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    let summary: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(summary["supported"], true);
    assert_eq!(summary["invalidVersion"]["verify"], "invalid_x402_version");
    assert_eq!(summary["invalidVersion"]["settle"], "invalid_x402_version");
    assert_eq!(summary["valid"]["verify"], true);
    assert_eq!(summary["valid"]["settle"], true);
    assert_eq!(summary["valid"]["replay"], true);
    assert_eq!(summary["conflict"], 409);
    assert_eq!(summary["duplicate"], "duplicate_settlement");
    assert_eq!(summary["authentication"], true);
    assert_eq!(summary["rateLimit"], 429);
    Ok(true)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn custom_http_surface_matches_x402_contract() -> TestResult {
    let telemetry = TelemetryGuard::initialize(Environment::Testnet, None)?;
    let metrics = telemetry.metrics();
    let payer = test_signer(TEST_PAYER)?;

    let public_application = build_application(disconnected_store(), metrics.clone())?;
    assert_public_contract(&public_application, &payer).await?;

    let Some(database) = TestDatabase::from_explicit_environment().await? else {
        return Ok(());
    };
    let result = assert_protected_contract(&database, metrics, &payer).await;
    let cleanup = database.cleanup().await;
    result?;
    cleanup
}
