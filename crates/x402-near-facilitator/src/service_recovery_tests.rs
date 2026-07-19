//! Deterministic `PostgreSQL` and mocked-RPC regression tests for settlement recovery.

use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use near_crypto::{InMemorySigner, KeyType, Signer};
use near_primitives::action::delegate::{DelegateAction, NonDelegateAction, SignedDelegateAction};
use near_primitives::action::{Action, FunctionCallAction};
use near_primitives::borsh;
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::SignedTransaction;
use near_primitives::types::{AccountId, Balance, Gas};
use near_primitives::views::{
    AccessKeyPermissionView, AccessKeyView, AccountView, ExecutionMetadataView,
    ExecutionOutcomeView, ExecutionOutcomeWithIdView, ExecutionStatusView,
    FinalExecutionOutcomeView, FinalExecutionStatus, TxExecutionStatus,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use url::Url;
use uuid::Uuid;
use x402_chain_near::{
    FinalBlock, NearChainProvider, NearNetwork, NearRpc, NearRpcError, RelayerHead,
    TransactionLookup, V2NearExact, VerificationPolicy, VerifiedPayment,
};
use x402_facilitator_local::FacilitatorLocal;
use x402_types::chain::{ChainIdPattern, ChainProviderOps, ChainRegistry};
use x402_types::scheme::{SchemeBlueprints, SchemeConfig, SchemeRegistry};

use super::{AppState, reconcile, run_new_settlement};
use crate::auth::ApiKeyAuthenticator;
use crate::config::{
    Environment, PaymentIdentifierConfig, RequestLimits, ServiceConfig, SponsorshipConfig,
};
use crate::leadership::ReadinessState;
use crate::protocol::{ParsedRequest, parse_request, request_fingerprint};
use crate::store::{
    ApiClient, ClaimOutcome, NewSettlement, PgStore, PreparedJournalEntry, SettlementRecord,
    SettlementState,
};
use crate::telemetry::Metrics;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const TESTNET_USDC: &str = "3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af";
const TEST_PAYEE: &str = "merchant.mike.testnet";
const TEST_PAYER: &str = "x402-recovery-payer.testnet";
const TEST_RELAYER: &str = "x402-recovery-relayer.testnet";
const TEST_PEPPER: [u8; 32] = [0x24; 32];
const FUNDED_BALANCE: u64 = 10_000;
const HARD_STOP: &str = "100";

#[derive(Clone)]
enum LookupPlan {
    Unknown,
    Pending,
    Final(Box<FinalExecutionOutcomeView>),
}

#[derive(Clone, Copy)]
enum SendPlan {
    Unknown,
    FinalSuccess,
}

struct MockRpcState {
    block_height: AtomicU64,
    relayer_nonce: AtomicU64,
    payer_nonce: AtomicU64,
    relayer_balance: AtomicU64,
    sends: AtomicUsize,
    lookups: StdMutex<HashMap<CryptoHash, LookupPlan>>,
    send_plan: StdMutex<SendPlan>,
    sent_bytes: StdMutex<Vec<Vec<u8>>>,
}

#[derive(Clone)]
struct MockRpc {
    state: Arc<MockRpcState>,
}

impl MockRpc {
    fn new() -> Self {
        Self {
            state: Arc::new(MockRpcState {
                block_height: AtomicU64::new(1_000),
                relayer_nonce: AtomicU64::new(0),
                payer_nonce: AtomicU64::new(0),
                relayer_balance: AtomicU64::new(FUNDED_BALANCE),
                sends: AtomicUsize::new(0),
                lookups: StdMutex::new(HashMap::new()),
                send_plan: StdMutex::new(SendPlan::Unknown),
                sent_bytes: StdMutex::new(Vec::new()),
            }),
        }
    }

    fn block(&self) -> FinalBlock {
        FinalBlock {
            height: self.state.block_height.load(Ordering::SeqCst),
            hash: CryptoHash::hash_bytes(b"recovery-final-block"),
        }
    }

    fn set_block_height(&self, height: u64) {
        self.state.block_height.store(height, Ordering::SeqCst);
    }

    fn set_relayer_balance(&self, balance: u64) {
        self.state.relayer_balance.store(balance, Ordering::SeqCst);
    }

    fn set_lookup(&self, hash: CryptoHash, plan: LookupPlan) {
        self.state
            .lookups
            .lock()
            .unwrap_or_else(|_| std::process::abort())
            .insert(hash, plan);
    }

    fn set_send_plan(&self, plan: SendPlan) {
        *self
            .state
            .send_plan
            .lock()
            .unwrap_or_else(|_| std::process::abort()) = plan;
    }

    fn sends(&self) -> usize {
        self.state.sends.load(Ordering::SeqCst)
    }

    fn sent_bytes(&self) -> Vec<Vec<u8>> {
        self.state
            .sent_bytes
            .lock()
            .unwrap_or_else(|_| std::process::abort())
            .clone()
    }

    fn account(amount: u128) -> AccountView {
        AccountView {
            amount: Balance::from_yoctonear(amount),
            locked: Balance::ZERO,
            code_hash: CryptoHash::hash_bytes(b"deployed-contract"),
            storage_usage: 0,
            storage_paid_at: 0,
            global_contract_hash: None,
            global_contract_account_id: None,
        }
    }
}

#[async_trait]
impl NearRpc for MockRpc {
    async fn network_id(&self) -> Result<String, NearRpcError> {
        Ok("testnet".to_owned())
    }

    async fn final_block(&self) -> Result<FinalBlock, NearRpcError> {
        Ok(self.block())
    }

    async fn view_account(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
    ) -> Result<AccountView, NearRpcError> {
        if block_hash != self.block().hash {
            return Err(NearRpcError::InvalidResponse(
                "recovery query was not pinned",
            ));
        }
        let amount = if account_id.as_str() == TEST_RELAYER {
            u128::from(self.state.relayer_balance.load(Ordering::SeqCst))
        } else {
            1_000_000_000
        };
        Ok(Self::account(amount))
    }

    async fn view_access_key(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
        _public_key: near_crypto::PublicKey,
    ) -> Result<AccessKeyView, NearRpcError> {
        if block_hash != self.block().hash {
            return Err(NearRpcError::InvalidResponse(
                "recovery query was not pinned",
            ));
        }
        let nonce = if account_id.as_str() == TEST_RELAYER {
            self.state.relayer_nonce.load(Ordering::SeqCst)
        } else {
            self.state.payer_nonce.load(Ordering::SeqCst)
        };
        Ok(AccessKeyView {
            nonce,
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
        if block_hash != self.block().hash {
            return Err(NearRpcError::InvalidResponse(
                "recovery query was not pinned",
            ));
        }
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
        self.state.sends.fetch_add(1, Ordering::SeqCst);
        let bytes = borsh::to_vec(&signed_transaction)
            .map_err(|_| NearRpcError::InvalidSignedTransaction)?;
        self.state
            .sent_bytes
            .lock()
            .map_err(|_| NearRpcError::InvalidResponse("mock send lock poisoned"))?
            .push(bytes);
        let plan = *self
            .state
            .send_plan
            .lock()
            .map_err(|_| NearRpcError::InvalidResponse("mock send plan lock poisoned"))?;
        match plan {
            SendPlan::Unknown => Ok(TransactionLookup::Unknown),
            SendPlan::FinalSuccess => Ok(TransactionLookup::Final(Box::new(final_outcome(
                signed_transaction,
                OutcomeShape::Successful,
            )?))),
        }
    }

    async fn transaction_status_final(
        &self,
        transaction_hash: CryptoHash,
        _signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError> {
        let plan = self
            .state
            .lookups
            .lock()
            .map_err(|_| NearRpcError::InvalidResponse("mock lookup lock poisoned"))?
            .get(&transaction_hash)
            .cloned()
            .unwrap_or(LookupPlan::Unknown);
        Ok(match plan {
            LookupPlan::Unknown => TransactionLookup::Unknown,
            LookupPlan::Pending => TransactionLookup::Pending(TxExecutionStatus::Included),
            LookupPlan::Final(outcome) => TransactionLookup::Final(outcome),
        })
    }
}

#[derive(Clone, Copy)]
enum OutcomeShape {
    Successful,
    MissingTokenReceipt,
    WrongTransactionHash,
}

fn execution_outcome(
    id: CryptoHash,
    executor_id: AccountId,
    receipt_ids: Vec<CryptoHash>,
    status: ExecutionStatusView,
) -> ExecutionOutcomeWithIdView {
    ExecutionOutcomeWithIdView {
        proof: Vec::new(),
        block_hash: CryptoHash::hash_bytes(b"recovery-outcome-block"),
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

fn final_outcome(
    signed_transaction: SignedTransaction,
    shape: OutcomeShape,
) -> Result<FinalExecutionOutcomeView, NearRpcError> {
    let relayer = TEST_RELAYER
        .parse()
        .map_err(|_| NearRpcError::InvalidResponse("invalid recovery relayer"))?;
    let payer = TEST_PAYER
        .parse()
        .map_err(|_| NearRpcError::InvalidResponse("invalid recovery payer"))?;
    let asset = TESTNET_USDC
        .parse()
        .map_err(|_| NearRpcError::InvalidResponse("invalid recovery asset"))?;
    let transaction_hash = signed_transaction.get_hash();
    let delegate_id = CryptoHash::hash_bytes(b"recovery-delegate-receipt");
    let token_id = CryptoHash::hash_bytes(b"recovery-token-receipt");
    let mut receipts = vec![execution_outcome(
        delegate_id,
        payer,
        vec![token_id],
        ExecutionStatusView::SuccessReceiptId(token_id),
    )];
    if !matches!(shape, OutcomeShape::MissingTokenReceipt) {
        receipts.push(execution_outcome(
            token_id,
            asset,
            Vec::new(),
            ExecutionStatusView::SuccessValue(Vec::new()),
        ));
    }
    let mut outcome = FinalExecutionOutcomeView {
        status: FinalExecutionStatus::SuccessValue(Vec::new()),
        transaction: signed_transaction.into(),
        transaction_outcome: execution_outcome(
            transaction_hash,
            relayer,
            vec![delegate_id],
            ExecutionStatusView::SuccessReceiptId(delegate_id),
        ),
        receipts_outcome: receipts,
    };
    if matches!(shape, OutcomeShape::WrongTransactionHash) {
        outcome.transaction.hash = CryptoHash::hash_bytes(b"wrong-final-transaction");
    }
    Ok(outcome)
}

struct TestDatabase {
    store: PgStore,
    pool: PgPool,
    admin: PgPool,
    schema: String,
}

impl TestDatabase {
    async fn from_explicit_environment(test_name: &str) -> TestResult<Option<Self>> {
        let Ok(raw) = std::env::var("X402_FACILITATOR_TEST_DATABASE_URL") else {
            eprintln!("skipping {test_name}: X402_FACILITATOR_TEST_DATABASE_URL is unset");
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
        let schema = format!("x402_recovery_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await?;
        let options = PgConnectOptions::from_str(&raw)?.options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(12)
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

struct TestContext {
    state: AppState,
    rpc: MockRpc,
    payer: Signer,
    client_id: Uuid,
}

async fn build_context(database: &TestDatabase) -> TestResult<TestContext> {
    let config = service_config()?;
    let rpc = MockRpc::new();
    let primary: Arc<dyn NearRpc> = Arc::new(rpc.clone());
    let backup: Arc<dyn NearRpc> = Arc::new(rpc.clone());
    let relayer_account: AccountId = TEST_RELAYER.parse()?;
    let relayer =
        InMemorySigner::from_seed(relayer_account, KeyType::ED25519, "recovery-relayer-seed");
    let provider = NearChainProvider::new(NearNetwork::Testnet, primary, Arc::new(relayer))
        .with_backup_rpc(backup);
    let facilitator = build_facilitator(provider.clone());
    let auth = ApiKeyAuthenticator::new(database.store.clone(), Environment::Testnet, TEST_PEPPER)?;
    let readiness = ReadinessState::default();
    let state = AppState::new(
        config,
        database.store.clone(),
        auth,
        facilitator,
        provider,
        readiness,
        Metrics::for_tests(),
    );
    database
        .store
        .upsert_relayer(
            "near:testnet",
            TEST_RELAYER,
            &state.provider.relayer_public_key().to_string(),
        )
        .await?;
    let client_id = seed_client(&database.store).await?;
    let payer_account: AccountId = TEST_PAYER.parse()?;
    let payer = InMemorySigner::from_seed(payer_account, KeyType::ED25519, "recovery-payer-seed");
    Ok(TestContext {
        state,
        rpc,
        payer,
        client_id,
    })
}

fn service_config() -> TestResult<ServiceConfig> {
    Ok(ServiceConfig {
        environment: Environment::Testnet,
        network: "near:testnet".to_owned(),
        bind_address: "127.0.0.1:0".parse()?,
        primary_rpc_url: Url::parse("https://primary.recovery.invalid")?,
        backup_rpc_url: Url::parse("https://backup.recovery.invalid")?,
        asset: TESTNET_USDC.to_owned(),
        asset_symbol: "USDC".to_owned(),
        minimum_amount: "1000".to_owned(),
        relayer_account_id: TEST_RELAYER.to_owned(),
        max_inner_gas: 30_000_000_000_000,
        database_max_connections: 12,
        request_limits: RequestLimits {
            body_bytes: 65_536,
            verify_per_minute: 100,
            settle_per_minute: 100,
            verify_timeout_seconds: 15,
            settle_timeout_seconds: 5,
            max_concurrent_verify: 16,
        },
        sponsorship: SponsorshipConfig {
            global_daily_yocto_near: "1000000".to_owned(),
            default_client_daily_yocto_near: "100000".to_owned(),
            reservation_yocto_near: "100".to_owned(),
            balance_warning_yocto_near: "200".to_owned(),
            balance_hard_stop_yocto_near: HARD_STOP.to_owned(),
        },
        payment_identifier: PaymentIdentifierConfig::default(),
    })
}

fn build_facilitator(provider: NearChainProvider) -> FacilitatorLocal<SchemeRegistry> {
    let chain_id = provider.chain_id();
    let mut providers = HashMap::new();
    providers.insert(chain_id.clone(), provider);
    let chains = ChainRegistry::new(providers);
    let blueprints = SchemeBlueprints::new().and_register(V2NearExact);
    let schemes = vec![SchemeConfig {
        enabled: true,
        id: "v2-near-exact-recovery".to_owned(),
        chains: ChainIdPattern::exact(chain_id.namespace, chain_id.reference),
        config: None,
    }];
    FacilitatorLocal::new(SchemeRegistry::build(chains, blueprints, &schemes))
}

async fn seed_client(store: &PgStore) -> TestResult<Uuid> {
    let client_id = Uuid::new_v4();
    let client = ApiClient {
        id: client_id,
        name: "recovery-regression".to_owned(),
        environment: "testnet".to_owned(),
        daily_budget_yocto_near: "100000".to_owned(),
        verify_rate_per_minute: 100,
        settle_rate_per_minute: 100,
    };
    store
        .create_client(
            &client,
            Uuid::new_v4(),
            &format!("x402_test_{}", hex::encode([0x71; 12])),
            &[0x72; 32],
        )
        .await?;
    store
        .allow_payee(client_id, "near:testnet", TESTNET_USDC, TEST_PAYEE)
        .await?;
    Ok(client_id)
}

fn valid_request(
    signer: &Signer,
    delegate_nonce: u64,
    max_block_height: u64,
    identifier: &str,
) -> TestResult<ParsedRequest> {
    let transfer = Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: "ft_transfer".to_owned(),
        args: serde_json::to_vec(&json!({
            "receiver_id": TEST_PAYEE,
            "amount": "1000",
        }))?,
        gas: Gas::from_gas(30_000_000_000_000),
        deposit: Balance::from_yoctonear(1),
    }));
    let delegate = DelegateAction {
        sender_id: TEST_PAYER.parse()?,
        receiver_id: TESTNET_USDC.parse()?,
        actions: vec![NonDelegateAction::try_from(transfer)?],
        nonce: delegate_nonce,
        max_block_height,
        public_key: signer.public_key(),
    };
    let signed_delegate = SignedDelegateAction::sign(signer, delegate);
    let encoded = STANDARD.encode(borsh::to_vec(&signed_delegate)?);
    let requirements = json!({
        "scheme": "exact",
        "network": "near:testnet",
        "amount": "1000",
        "payTo": TEST_PAYEE,
        "maxTimeoutSeconds": 60,
        "asset": TESTNET_USDC,
    });
    let request = json!({
        "x402Version": 2,
        "paymentPayload": {
            "x402Version": 2,
            "accepted": requirements.clone(),
            "payload": {
                "signedDelegateAction": encoded,
            },
            "extensions": {
                "payment-identifier": {
                    "info": {
                        "required": true,
                        "id": identifier,
                    },
                    "schema": {},
                },
            },
        },
        "paymentRequirements": requirements,
    });
    Ok(parse_request(
        &serde_json::to_vec(&request)?,
        &PaymentIdentifierConfig::default(),
    )?)
}

struct ReservedPayment {
    id: Uuid,
    request: ParsedRequest,
    payment: VerifiedPayment,
}

async fn reserve_payment(
    context: &TestContext,
    delegate_nonce: u64,
    max_block_height: u64,
    identifier: &str,
) -> TestResult<ReservedPayment> {
    let request = valid_request(&context.payer, delegate_nonce, max_block_height, identifier)?;
    let payment = context
        .state
        .provider
        .verify(
            &request.raw,
            &VerificationPolicy {
                max_sponsored_gas: 30_000_000_000_000,
            },
        )
        .await?;
    let id = Uuid::new_v4();
    let fingerprint = request_fingerprint(&request.value, payment.payment_hash())?;
    let claim = context
        .state
        .store
        .claim_settlement(&NewSettlement {
            id,
            api_client_id: context.client_id,
            payment_identifier: Some(identifier.to_owned()),
            payment_hash: *payment.payment_hash(),
            request_fingerprint: fingerprint,
            x402_version: 2,
            scheme: "exact".to_owned(),
            network: "near:testnet".to_owned(),
            asset: TESTNET_USDC.to_owned(),
            pay_to: TEST_PAYEE.to_owned(),
            amount: "1000".to_owned(),
            payer: TEST_PAYER.to_owned(),
            delegate_public_key: payment.payer_public_key.to_string(),
            delegate_nonce: payment.delegate_nonce.to_string(),
            delegate_max_block_height: payment.max_block_height.to_string(),
            policy_snapshot: json!({"test": "recovery-regression"}),
            reservation_yocto_near: "100".to_owned(),
            global_daily_budget_yocto_near: "1000000".to_owned(),
            client_daily_budget_yocto_near: "100000".to_owned(),
        })
        .await?;
    assert!(matches!(claim, ClaimOutcome::New(_)));
    Ok(ReservedPayment {
        id,
        request,
        payment,
    })
}

struct PreparedPayment {
    record: SettlementRecord,
    transaction_bytes: Vec<u8>,
    transaction_hash: CryptoHash,
}

async fn prepare_payment(
    context: &TestContext,
    reserved: &ReservedPayment,
    previous_relayer_nonce: u64,
    submitted: bool,
) -> TestResult<PreparedPayment> {
    let prepared = context.state.provider.prepare_outer_transaction(
        &reserved.payment,
        RelayerHead {
            block_height: context.rpc.block().height,
            block_hash: context.rpc.block().hash,
            access_key_nonce: previous_relayer_nonce,
        },
    )?;
    let transaction_bytes = prepared.signed_transaction_bytes().to_vec();
    context
        .state
        .store
        .mark_prepared(&PreparedJournalEntry {
            settlement_id: reserved.id,
            relayer_account_id: prepared.signer_id.to_string(),
            relayer_public_key: prepared.signer_public_key.to_string(),
            relayer_nonce: prepared.relayer_nonce.to_string(),
            transaction_bytes: transaction_bytes.clone(),
            transaction_hash: prepared.transaction_hash.to_string(),
        })
        .await?;
    if submitted {
        context.state.store.mark_submitted(reserved.id).await?;
    }
    let record = context
        .state
        .store
        .settlement(reserved.id)
        .await?
        .ok_or_else(|| std::io::Error::other("prepared settlement disappeared"))?;
    Ok(PreparedPayment {
        record,
        transaction_bytes,
        transaction_hash: prepared.transaction_hash,
    })
}

fn make_ready(state: &AppState) {
    state.readiness.set_leadership(true);
    state.readiness.set_reconciliation(true);
    state.readiness.set_rpc(true);
    state.readiness.set_relayer(true);
}

fn terminal_json(record: &SettlementRecord) -> TestResult<Value> {
    Ok(serde_json::from_slice(
        record
            .terminal_response_bytes
            .as_deref()
            .ok_or_else(|| std::io::Error::other("terminal body is absent"))?,
    )?)
}

#[tokio::test]
async fn incomplete_inner_receipt_remains_submitted_and_unready() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("incomplete_inner_receipt").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_incomplete_0001").await?;
        let prepared = prepare_payment(&context, &reserved, 0, true).await?;
        let signed = x402_chain_near::decode_signed_transaction(&prepared.transaction_bytes)?;
        let outcome = final_outcome(signed, OutcomeShape::MissingTokenReceipt)?;
        context.rpc.set_lookup(
            prepared.transaction_hash,
            LookupPlan::Final(Box::new(outcome)),
        );
        make_ready(&context.state);

        reconcile(&context.state).await?;

        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Submitted);
        assert!(record.terminal_response_bytes.is_none());
        assert!(!context.state.readiness.snapshot().reconciliation);
        assert_eq!(context.rpc.sends(), 0);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn wrong_final_transaction_identity_remains_submitted_and_unready() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("wrong_final_transaction_identity").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_wrong_identity_001").await?;
        let prepared = prepare_payment(&context, &reserved, 0, true).await?;
        let signed = x402_chain_near::decode_signed_transaction(&prepared.transaction_bytes)?;
        let outcome = final_outcome(signed, OutcomeShape::WrongTransactionHash)?;
        context.rpc.set_lookup(
            prepared.transaction_hash,
            LookupPlan::Final(Box::new(outcome)),
        );
        make_ready(&context.state);

        reconcile(&context.state).await?;

        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Submitted);
        assert!(record.terminal_response_bytes.is_none());
        assert!(!context.state.readiness.snapshot().reconciliation);
        assert_eq!(context.rpc.sends(), 0);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn expired_prepared_and_submitted_rows_never_rebroadcast() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("expired_prepared_and_submitted").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let first = reserve_payment(&context, 1, 1_050, "recovery_expired_prepared_01").await?;
        let prepared = prepare_payment(&context, &first, 0, false).await?;
        let second = reserve_payment(&context, 2, 1_050, "recovery_expired_submitted_1").await?;
        let submitted = prepare_payment(&context, &second, 1, true).await?;
        context.rpc.set_block_height(1_050);
        make_ready(&context.state);

        reconcile(&context.state).await?;

        let submitted_hash = submitted.transaction_hash.to_string();
        for (fixture, expected_transaction) in
            [(&prepared, String::new()), (&submitted, submitted_hash)]
        {
            let record = context
                .state
                .store
                .settlement(fixture.record.id)
                .await?
                .ok_or_else(|| std::io::Error::other("expired settlement disappeared"))?;
            assert_eq!(record.state, SettlementState::Failed);
            assert_eq!(record.terminal_http_status, Some(200));
            let body = terminal_json(&record)?;
            assert_eq!(
                body["errorReason"],
                "invalid_exact_near_payload_delegate_action_expired"
            );
            assert_eq!(body["transaction"], expected_transaction);
            assert_eq!(body["network"], "near:testnet");
        }
        assert_eq!(context.rpc.sends(), 0);
        assert!(context.state.readiness.snapshot().reconciliation);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn hard_balance_stop_prevents_preparation_and_broadcast() -> TestResult {
    let Some(database) = TestDatabase::from_explicit_environment("hard_balance_stop").await? else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_balance_stop_0001").await?;
        context.rpc.set_relayer_balance(99);
        make_ready(&context.state);

        run_new_settlement(
            context.state.clone(),
            reserved.id,
            reserved.request.raw.clone(),
        )
        .await;

        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Failed);
        assert_eq!(record.terminal_http_status, Some(503));
        assert!(record.outer_transaction_bytes.is_none());
        assert_eq!(
            terminal_json(&record)?["error"]["code"],
            "relayer_unavailable"
        );
        assert!(!context.state.readiness.snapshot().relayer);
        assert_eq!(context.rpc.sends(), 0);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn quarantined_relayer_policy_prevents_preparation_and_broadcast() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("quarantined_relayer_policy").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_policy_stop_00001").await?;
        context
            .state
            .store
            .quarantine_relayer(
                "near:testnet",
                TEST_RELAYER,
                &context.state.provider.relayer_public_key().to_string(),
                "test policy stop",
                "0",
            )
            .await?;
        make_ready(&context.state);

        run_new_settlement(
            context.state.clone(),
            reserved.id,
            reserved.request.raw.clone(),
        )
        .await;

        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Failed);
        assert_eq!(record.terminal_http_status, Some(503));
        assert!(record.outer_transaction_bytes.is_none());
        assert_eq!(
            terminal_json(&record)?["error"]["code"],
            "relayer_unavailable"
        );
        assert!(!context.state.readiness.snapshot().relayer);
        assert_eq!(context.rpc.sends(), 0);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn corrupt_stored_transaction_fails_readiness_without_broadcast() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("corrupt_stored_transaction").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_corrupt_stored_001").await?;
        let prepared = prepare_payment(&context, &reserved, 0, false).await?;
        let mut corrupt = prepared.transaction_bytes.clone();
        let last = corrupt
            .last_mut()
            .ok_or_else(|| std::io::Error::other("prepared bytes were empty"))?;
        *last ^= 0x01;
        sqlx::query("UPDATE settlements SET outer_transaction_bytes = $2 WHERE id = $1")
            .bind(reserved.id)
            .bind(corrupt)
            .execute(&database.pool)
            .await?;
        make_ready(&context.state);

        let Err(error) = reconcile(&context.state).await else {
            return Err(std::io::Error::other(
                "corrupt stored transaction unexpectedly reconciled",
            )
            .into());
        };
        assert!(error.to_string().contains("database state is inconsistent"));
        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Prepared);
        assert!(!context.state.readiness.snapshot().reconciliation);
        assert!(!context.state.readiness.snapshot().relayer);
        assert_eq!(context.rpc.sends(), 0);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn reconciliation_stops_before_later_nonce_after_pending_row() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("reconciliation_nonce_order").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let first = reserve_payment(&context, 1, 1_050, "recovery_pending_first_0001").await?;
        let first = prepare_payment(&context, &first, 0, true).await?;
        let second = reserve_payment(&context, 2, 1_050, "recovery_pending_second_001").await?;
        let second = prepare_payment(&context, &second, 1, false).await?;
        sqlx::query(
            "UPDATE settlements SET created_at = CASE \
                 WHEN id = $1 THEN now() - interval '2 seconds' \
                 WHEN id = $2 THEN now() - interval '1 second' \
                 ELSE created_at END \
             WHERE id IN ($1, $2)",
        )
        .bind(first.record.id)
        .bind(second.record.id)
        .execute(&database.pool)
        .await?;
        context
            .rpc
            .set_lookup(first.transaction_hash, LookupPlan::Pending);
        make_ready(&context.state);

        reconcile(&context.state).await?;

        let attempts: i32 =
            sqlx::query_scalar("SELECT reconciliation_attempts FROM settlements WHERE id = $1")
                .bind(second.record.id)
                .fetch_one(&database.pool)
                .await?;
        assert_eq!(attempts, 0);
        assert_eq!(
            context
                .state
                .store
                .settlement(first.record.id)
                .await?
                .map(|record| record.state),
            Some(SettlementState::Submitted)
        );
        assert_eq!(
            context
                .state
                .store
                .settlement(second.record.id)
                .await?
                .map(|record| record.state),
            Some(SettlementState::Prepared)
        );
        assert!(!context.state.readiness.snapshot().reconciliation);
        assert_eq!(context.rpc.sends(), 0);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn accepted_response_drop_recovers_without_second_transaction() -> TestResult {
    let Some(database) = TestDatabase::from_explicit_environment("accepted_response_drop").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_response_drop_0001").await?;
        let submitted = prepare_payment(&context, &reserved, 0, true).await?;

        context.rpc.set_send_plan(SendPlan::Unknown);
        assert!(matches!(
            context
                .state
                .provider
                .broadcast_exact(&submitted.transaction_bytes)
                .await?,
            TransactionLookup::Unknown
        ));
        let signed = x402_chain_near::decode_signed_transaction(&submitted.transaction_bytes)?;
        let outcome = final_outcome(signed, OutcomeShape::Successful)?;
        context.rpc.set_lookup(
            submitted.transaction_hash,
            LookupPlan::Final(Box::new(outcome)),
        );
        make_ready(&context.state);

        reconcile(&context.state).await?;

        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Succeeded);
        assert_eq!(terminal_json(&record)?["success"], true);
        assert_eq!(context.rpc.sends(), 1);
        assert_eq!(context.rpc.sent_bytes(), vec![submitted.transaction_bytes]);
        assert!(context.state.readiness.snapshot().reconciliation);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}

#[tokio::test]
async fn unknown_recovery_rebroadcasts_only_exact_stored_bytes() -> TestResult {
    let Some(database) =
        TestDatabase::from_explicit_environment("exact_stored_byte_rebroadcast").await?
    else {
        return Ok(());
    };
    let result = async {
        let context = build_context(&database).await?;
        let reserved = reserve_payment(&context, 1, 1_050, "recovery_exact_rebroadcast_01").await?;
        let submitted = prepare_payment(&context, &reserved, 0, true).await?;
        context.rpc.set_send_plan(SendPlan::FinalSuccess);
        make_ready(&context.state);

        reconcile(&context.state).await?;

        let record = context
            .state
            .store
            .settlement(reserved.id)
            .await?
            .ok_or_else(|| std::io::Error::other("settlement disappeared"))?;
        assert_eq!(record.state, SettlementState::Succeeded);
        assert_eq!(context.rpc.sends(), 1);
        assert_eq!(context.rpc.sent_bytes(), vec![submitted.transaction_bytes]);
        assert!(context.state.readiness.snapshot().reconciliation);
        Ok::<(), Box<dyn Error + Send + Sync>>(())
    }
    .await;
    database.cleanup().await?;
    result
}
