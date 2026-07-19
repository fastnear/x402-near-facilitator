use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use borsh::BorshDeserialize;
use near_crypto::{
    InMemorySigner, KeyType, ML_DSA_65_SIGNATURE_LENGTH, PublicKey, SecretKey, Signature, Signer,
};
use near_primitives::{
    action::{
        Action, FunctionCallAction, TransferAction,
        delegate::{DelegateAction, NonDelegateAction, SignedDelegateAction},
    },
    errors::{InvalidTxError, TxExecutionError},
    hash::CryptoHash,
    transaction::{SignedTransaction, Transaction, TransactionV0},
    types::{AccountId, Balance, Gas},
    views::{
        AccessKeyPermissionView, AccessKeyView, AccountView, ExecutionMetadataView,
        ExecutionOutcomeView, ExecutionOutcomeWithIdView, ExecutionStatusView,
        FinalExecutionOutcomeView, FinalExecutionStatus, SignedTransactionView,
    },
};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use x402_types::{proto, scheme::X402SchemeFacilitator};

use crate::{
    ExactNearPayload, NearChainProvider, NearExactFacilitator, NearExactFacilitatorConfig,
    NearNetwork, NearRelayerSigner, NearRpc, NearRpcError, ReceiptValidationError, RelayerHead,
    TransactionLookup, VerificationFailure, VerificationPolicy, decode_ft_transfer_args,
    decode_signed_delegate, interpret_final_outcome, rpc::FinalBlock, signed_transaction_hash,
    validate_final_outcome_identity,
};

const ED25519_TEST_SECRET_DO_NOT_FUND: &str = "ed25519:4m8u95BQAFnA3c593fnghrApJ9c4bufLydgdUwaHnmHvRs3r5ukT68H2punoN6Mg45MnRGnH5AEQcjQGnaNPJoQu";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    sender_id: String,
    receiver_id: String,
    pay_to: String,
    amount: String,
    nonce: String,
    max_block_height: String,
    gas: String,
    deposit: String,
    delegates: Vec<FixtureDelegate>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureDelegate {
    curve: String,
    public_key: String,
    signed_delegate_action: String,
}

fn fixture() -> Result<Fixture, Box<dyn Error>> {
    Ok(serde_json::from_str(include_str!(
        "../fixtures/signed-delegates.json"
    ))?)
}

fn test_signer(account_id: &str) -> Result<Signer, Box<dyn Error>> {
    let account_id = account_id.parse::<AccountId>()?;
    let secret_key = ED25519_TEST_SECRET_DO_NOT_FUND.parse::<SecretKey>()?;
    Ok(InMemorySigner::from_secret_key(account_id, secret_key))
}

#[derive(Clone, Copy, Debug)]
enum MockFailure {
    AccountNotFound,
    AccessKeyNotFound,
    MethodNotFound,
    InvalidResponse,
    Request,
    Timeout,
}

impl MockFailure {
    fn into_rpc_error(self) -> NearRpcError {
        match self {
            Self::AccountNotFound => NearRpcError::AccountNotFound,
            Self::AccessKeyNotFound => NearRpcError::AccessKeyNotFound,
            Self::MethodNotFound => NearRpcError::MethodNotFound,
            Self::InvalidResponse => NearRpcError::InvalidResponse("mock invalid response"),
            Self::Request => NearRpcError::Request("mock RPC unavailable".to_owned()),
            Self::Timeout => NearRpcError::Timeout,
        }
    }
}

#[derive(Debug, Default)]
struct RpcCalls {
    final_block: AtomicUsize,
    access_key: AtomicUsize,
    sender_account: AtomicUsize,
    token_account: AtomicUsize,
    balance: AtomicUsize,
    storage: AtomicUsize,
    send: AtomicUsize,
    transaction_status: AtomicUsize,
}

#[derive(Debug)]
struct MockRpc {
    network_id: String,
    network_id_error: Option<MockFailure>,
    block: FinalBlock,
    block_error: Option<MockFailure>,
    access_key: AccessKeyView,
    access_key_error: Option<MockFailure>,
    sender_account: AccountView,
    sender_account_error: Option<MockFailure>,
    token_account: AccountView,
    token_account_error: Option<MockFailure>,
    balance_response: Vec<u8>,
    balance_error: Option<MockFailure>,
    storage_response: Vec<u8>,
    storage_error: Option<MockFailure>,
    calls: RpcCalls,
}

impl MockRpc {
    fn new() -> Self {
        Self {
            network_id: "testnet".to_owned(),
            network_id_error: None,
            block: FinalBlock {
                height: 1_000,
                hash: CryptoHash::hash_bytes(b"final-block"),
            },
            block_error: None,
            access_key: AccessKeyView {
                nonce: 0,
                permission: AccessKeyPermissionView::FullAccess,
            },
            access_key_error: None,
            sender_account: Self::account(),
            sender_account_error: None,
            token_account: Self::account(),
            token_account_error: None,
            balance_response: br#""10000000""#.to_vec(),
            balance_error: None,
            storage_response: b"{}".to_vec(),
            storage_error: None,
            calls: RpcCalls::default(),
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
            Err(NearRpcError::InvalidResponse("query was not block-pinned"))
        }
    }
}

#[async_trait]
impl NearRpc for MockRpc {
    async fn network_id(&self) -> Result<String, NearRpcError> {
        if let Some(error) = self.network_id_error {
            return Err(error.into_rpc_error());
        }
        Ok(self.network_id.clone())
    }

    async fn final_block(&self) -> Result<FinalBlock, NearRpcError> {
        self.calls.final_block.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.block_error {
            return Err(error.into_rpc_error());
        }
        Ok(self.block)
    }

    async fn view_account(
        &self,
        block_hash: CryptoHash,
        account_id: AccountId,
    ) -> Result<AccountView, NearRpcError> {
        self.ensure_pinned(block_hash)?;
        if account_id.as_str() == "usdc.testnet" {
            self.calls.token_account.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.token_account_error {
                return Err(error.into_rpc_error());
            }
            Ok(self.token_account.clone())
        } else {
            self.calls.sender_account.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.sender_account_error {
                return Err(error.into_rpc_error());
            }
            Ok(self.sender_account.clone())
        }
    }

    async fn view_access_key(
        &self,
        block_hash: CryptoHash,
        _account_id: AccountId,
        _public_key: near_crypto::PublicKey,
    ) -> Result<AccessKeyView, NearRpcError> {
        self.ensure_pinned(block_hash)?;
        self.calls.access_key.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.access_key_error {
            return Err(error.into_rpc_error());
        }
        Ok(self.access_key.clone())
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
            "ft_balance_of" => {
                self.calls.balance.fetch_add(1, Ordering::SeqCst);
                if let Some(error) = self.balance_error {
                    return Err(error.into_rpc_error());
                }
                Ok(self.balance_response.clone())
            }
            "storage_balance_of" => {
                self.calls.storage.fetch_add(1, Ordering::SeqCst);
                if let Some(error) = self.storage_error {
                    return Err(error.into_rpc_error());
                }
                Ok(self.storage_response.clone())
            }
            _ => Err(NearRpcError::MethodNotFound),
        }
    }

    async fn send_transaction_final(
        &self,
        _signed_transaction: SignedTransaction,
    ) -> Result<TransactionLookup, NearRpcError> {
        self.calls.send.fetch_add(1, Ordering::SeqCst);
        Ok(TransactionLookup::Pending(
            near_primitives::views::TxExecutionStatus::IncludedFinal,
        ))
    }

    async fn transaction_status_final(
        &self,
        _transaction_hash: CryptoHash,
        _signer_id: AccountId,
    ) -> Result<TransactionLookup, NearRpcError> {
        self.calls.transaction_status.fetch_add(1, Ordering::SeqCst);
        Ok(TransactionLookup::Unknown)
    }
}

fn requirements_value() -> Value {
    json!({
        "scheme": "exact",
        "network": "near:testnet",
        "amount": "1000000",
        "asset": "usdc.testnet",
        "payTo": "merchant.testnet",
        "maxTimeoutSeconds": 60,
        "extra": {},
    })
}

fn request_value(encoded: &str) -> Value {
    let requirements = requirements_value();
    json!({
        "x402Version": 2,
        "paymentPayload": {
            "x402Version": 2,
            "accepted": requirements,
            "payload": { "signedDelegateAction": encoded },
        },
        "paymentRequirements": requirements,
    })
}

fn proto_request(value: &Value) -> Result<proto::VerifyRequest, Box<dyn Error>> {
    let raw = RawValue::from_string(serde_json::to_string(&value)?)?;
    Ok(proto::VerifyRequest::from(raw))
}

fn request(encoded: &str) -> Result<proto::VerifyRequest, Box<dyn Error>> {
    proto_request(&request_value(encoded))
}

fn provider(rpc: Arc<MockRpc>) -> Result<NearChainProvider, Box<dyn Error>> {
    let relayer: Arc<dyn NearRelayerSigner> = Arc::new(test_signer("relayer.testnet")?);
    Ok(NearChainProvider::new(NearNetwork::Testnet, rpc, relayer))
}

fn fixture_signed_delegate() -> Result<SignedDelegateAction, Box<dyn Error>> {
    let fixture = fixture()?;
    Ok(decode_signed_delegate(&fixture.delegates[0].signed_delegate_action)?.signed_delegate)
}

fn signed_delegate_with(
    mutate: impl FnOnce(&mut DelegateAction) -> Result<(), Box<dyn Error>>,
) -> Result<String, Box<dyn Error>> {
    let signer = test_signer("alice.testnet")?;
    let mut delegate = fixture_signed_delegate()?.delegate_action;
    mutate(&mut delegate)?;
    let signed_delegate = SignedDelegateAction::sign(&signer, delegate);
    Ok(STANDARD.encode(borsh::to_vec(&signed_delegate)?))
}

fn replace_actions(
    delegate: &mut DelegateAction,
    actions: Vec<Action>,
) -> Result<(), Box<dyn Error>> {
    delegate.actions = actions
        .into_iter()
        .map(NonDelegateAction::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn function_call(method_name: &str, args: Value, gas: u64, deposit: u128) -> Action {
    Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: method_name.to_owned(),
        args: serde_json::to_vec(&args).unwrap_or_default(),
        gas: Gas::from_gas(gas),
        deposit: Balance::from_yoctonear(deposit),
    }))
}

async fn failure_for(
    provider: &NearChainProvider,
    value: &Value,
) -> Result<VerificationFailure, Box<dyn Error>> {
    provider
        .verify(&proto_request(value)?, &VerificationPolicy::default())
        .await
        .err()
        .ok_or_else(|| "payment unexpectedly verified".into())
}

#[test]
fn decodes_typescript_ed25519_and_secp256k1_fixtures() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    assert_eq!(fixture.delegates.len(), 2);
    for vector in &fixture.delegates {
        let decoded = decode_signed_delegate(&vector.signed_delegate_action)?;
        let delegate = &decoded.signed_delegate.delegate_action;
        assert!(decoded.signed_delegate.verify());
        assert_eq!(delegate.sender_id.as_str(), fixture.sender_id);
        assert_eq!(delegate.receiver_id.as_str(), fixture.receiver_id);
        assert_eq!(delegate.nonce.to_string(), fixture.nonce);
        assert_eq!(
            delegate.max_block_height.to_string(),
            fixture.max_block_height
        );
        assert_eq!(delegate.public_key.to_string(), vector.public_key);
        match (vector.curve.as_str(), delegate.public_key.key_type()) {
            ("ed25519", KeyType::ED25519) | ("secp256k1", KeyType::SECP256K1) => {}
            _ => return Err("fixture curve does not match decoded key".into()),
        }
        let actions = delegate.get_actions();
        let Some(Action::FunctionCall(call)) = actions.first() else {
            return Err("fixture does not contain a function call".into());
        };
        assert_eq!(call.method_name, "ft_transfer");
        assert_eq!(call.gas.as_gas().to_string(), fixture.gas);
        assert_eq!(call.deposit.as_yoctonear().to_string(), fixture.deposit);
        let args: serde_json::Value = serde_json::from_slice(&call.args)?;
        assert_eq!(args["receiver_id"], fixture.pay_to);
        assert_eq!(args["amount"], fixture.amount);
    }
    Ok(())
}

#[test]
fn delegate_debug_output_redacts_payment_material() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let encoded = &fixture.delegates[0].signed_delegate_action;
    let decoded = decode_signed_delegate(encoded)?;
    let decoded_debug = format!("{decoded:?}");
    assert!(decoded_debug.contains("<redacted>"));
    assert!(!decoded_debug.contains(encoded));
    assert!(!decoded_debug.contains(&fixture.delegates[0].public_key));

    let payload = ExactNearPayload {
        signed_delegate_action: encoded.clone(),
    };
    let payload_debug = format!("{payload:?}");
    assert!(payload_debug.contains("<redacted>"));
    assert!(!payload_debug.contains(encoded));
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn verification_reason_strings_and_payer_attribution_are_stable() {
    let cases = [
        (
            VerificationFailure::InvalidX402Version,
            "invalid_x402_version",
            false,
        ),
        (
            VerificationFailure::UnsupportedScheme,
            "unsupported_scheme",
            false,
        ),
        (
            VerificationFailure::InvalidNetwork,
            "invalid_network",
            false,
        ),
        (
            VerificationFailure::NetworkMismatch,
            "invalid_exact_near_network_mismatch",
            false,
        ),
        (
            VerificationFailure::AssetMismatch,
            "invalid_exact_near_asset_mismatch",
            false,
        ),
        (
            VerificationFailure::PayToMismatch,
            "invalid_exact_near_pay_to_mismatch",
            false,
        ),
        (
            VerificationFailure::AmountMismatch,
            "invalid_exact_near_amount_mismatch",
            false,
        ),
        (
            VerificationFailure::InvalidMaxTimeout,
            "invalid_exact_near_max_timeout",
            false,
        ),
        (
            VerificationFailure::InvalidPayloadShape,
            "invalid_exact_near_payload_shape",
            false,
        ),
        (
            VerificationFailure::InvalidSignedDelegateAction,
            "invalid_exact_near_payload_signed_delegate_action",
            false,
        ),
        (
            VerificationFailure::InvalidSignature,
            "invalid_exact_near_payload_signature",
            false,
        ),
        (
            VerificationFailure::NoRelayerConfigured,
            "invalid_exact_near_no_relayer_configured",
            true,
        ),
        (
            VerificationFailure::RelayerCannotBePayer,
            "invalid_exact_near_relayer_cannot_be_payer",
            true,
        ),
        (
            VerificationFailure::InvalidActionCount,
            "invalid_exact_near_payload_action_count",
            true,
        ),
        (
            VerificationFailure::InvalidActionKind,
            "invalid_exact_near_payload_action_kind",
            true,
        ),
        (
            VerificationFailure::InvalidMethodName,
            "invalid_exact_near_payload_method_name",
            true,
        ),
        (
            VerificationFailure::TokenContractMismatch,
            "invalid_exact_near_payload_token_contract_mismatch",
            true,
        ),
        (
            VerificationFailure::InvalidFtTransferArgs,
            "invalid_exact_near_payload_ft_transfer_args",
            true,
        ),
        (
            VerificationFailure::RecipientMismatch,
            "invalid_exact_near_payload_recipient_mismatch",
            true,
        ),
        (
            VerificationFailure::TransferAmountMismatch,
            "invalid_exact_near_payload_amount_mismatch",
            true,
        ),
        (
            VerificationFailure::InvalidAttachedDeposit,
            "invalid_exact_near_payload_attached_deposit",
            true,
        ),
        (
            VerificationFailure::GasLimitExceeded,
            "invalid_exact_near_payload_gas_limit_exceeded",
            true,
        ),
        (
            VerificationFailure::CurrentBlockHeightUnavailable,
            "invalid_exact_near_current_block_height_unavailable",
            true,
        ),
        (
            VerificationFailure::DelegateActionExpired,
            "invalid_exact_near_payload_delegate_action_expired",
            true,
        ),
        (
            VerificationFailure::TimeoutWindowExceedsMaximum,
            "invalid_exact_near_payload_delegate_action_timeout_window_exceeds_max_timeout",
            true,
        ),
        (
            VerificationFailure::DelegateNonceOutOfRange,
            "invalid_exact_near_payload_delegate_action_nonce_out_of_range",
            true,
        ),
        (
            VerificationFailure::AccessKeyLookupFailed,
            "invalid_exact_near_access_key_lookup_failed",
            true,
        ),
        (
            VerificationFailure::AccessKeyNotFound,
            "invalid_exact_near_access_key_not_found",
            true,
        ),
        (
            VerificationFailure::DelegateNonceAlreadyUsed,
            "invalid_exact_near_payload_delegate_action_nonce_already_used",
            true,
        ),
        (
            VerificationFailure::FunctionCallKeyNotAllowed,
            "invalid_exact_near_function_call_key_not_allowed",
            true,
        ),
        (
            VerificationFailure::UnsupportedAccessKeyPermission,
            "invalid_exact_near_unsupported_access_key_permission",
            true,
        ),
        (
            VerificationFailure::AccountLookupFailed,
            "invalid_exact_near_account_lookup_failed",
            true,
        ),
        (
            VerificationFailure::SenderAccountNotFound,
            "invalid_exact_near_sender_account_not_found",
            true,
        ),
        (
            VerificationFailure::TokenAccountLookupFailed,
            "invalid_exact_near_token_account_lookup_failed",
            true,
        ),
        (
            VerificationFailure::TokenAccountNotFound,
            "invalid_exact_near_token_account_not_found",
            true,
        ),
        (
            VerificationFailure::TokenContractHasNoCode,
            "invalid_exact_near_token_contract_no_code",
            true,
        ),
        (
            VerificationFailure::BalanceCheckFailed,
            "invalid_exact_near_balance_check_failed",
            true,
        ),
        (
            VerificationFailure::InsufficientFunds,
            "insufficient_funds",
            true,
        ),
        (
            VerificationFailure::StorageCheckFailed,
            "invalid_exact_near_storage_check_failed",
            true,
        ),
        (
            VerificationFailure::RecipientNotRegisteredForStorage,
            "invalid_exact_near_recipient_not_registered_for_storage",
            true,
        ),
    ];

    for (failure, reason, attributable) in cases {
        assert_eq!(failure.reason(), reason);
        assert_eq!(failure.to_string(), reason);
        assert_eq!(failure.payer_is_attributable(), attributable);
    }
}

#[test]
fn rejects_noncanonical_base64_trailing_borsh_and_tampered_signature() -> Result<(), Box<dyn Error>>
{
    let fixture = fixture()?;
    let encoded = &fixture.delegates[0].signed_delegate_action;

    let unpadded = encoded.trim_end_matches('=');
    assert_eq!(
        decode_signed_delegate(unpadded).err(),
        Some(VerificationFailure::InvalidSignedDelegateAction)
    );

    let mut bytes = STANDARD.decode(encoded)?;
    bytes.push(0);
    let trailing = STANDARD.encode(bytes);
    assert_eq!(
        decode_signed_delegate(&trailing).err(),
        Some(VerificationFailure::InvalidSignedDelegateAction)
    );

    let mut signed_delegate = decode_signed_delegate(encoded)?.signed_delegate;
    signed_delegate.signature = test_signer("tamper.testnet")?.sign(&[0; 32]);
    let tampered = decode_signed_delegate(&STANDARD.encode(borsh::to_vec(&signed_delegate)?))?;
    assert!(!tampered.signed_delegate.verify());
    Ok(())
}

#[test]
fn rejects_truncated_mismatched_and_unsupported_signed_delegate_encodings()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let encoded = &fixture.delegates[0].signed_delegate_action;
    let bytes = STANDARD.decode(encoded)?;
    for truncated_length in [0, 1, bytes.len() / 2, bytes.len() - 1] {
        assert_eq!(
            decode_signed_delegate(&STANDARD.encode(&bytes[..truncated_length])).err(),
            Some(VerificationFailure::InvalidSignedDelegateAction)
        );
    }

    let mut mismatched = fixture_signed_delegate()?;
    mismatched.delegate_action.public_key = PublicKey::empty(KeyType::SECP256K1);
    assert_eq!(
        decode_signed_delegate(&STANDARD.encode(borsh::to_vec(&mismatched)?)).err(),
        Some(VerificationFailure::InvalidSignature)
    );

    let mut unsupported = fixture_signed_delegate()?;
    unsupported.delegate_action.public_key = PublicKey::empty(KeyType::MLDSA65);
    unsupported.signature =
        Signature::from_parts(KeyType::MLDSA65, &vec![0_u8; ML_DSA_65_SIGNATURE_LENGTH])?;
    assert_eq!(
        decode_signed_delegate(&STANDARD.encode(borsh::to_vec(&unsupported)?)).err(),
        Some(VerificationFailure::InvalidSignature)
    );
    Ok(())
}

#[tokio::test]
async fn validation_preserves_canonical_first_failure_order_before_chain_queries()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let encoded = &fixture.delegates[0].signed_delegate_action;
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(Arc::clone(&rpc))?;

    let malformed = proto::VerifyRequest::from(RawValue::from_string("[]".to_owned())?);
    assert_eq!(
        provider
            .verify(&malformed, &VerificationPolicy::default())
            .await
            .err(),
        Some(VerificationFailure::InvalidPayloadShape)
    );

    let mut value = request_value(encoded);
    value["x402Version"] = json!(1);
    value["paymentPayload"]["x402Version"] = json!(1);
    value["paymentPayload"]["accepted"]["scheme"] = json!("other");
    assert_eq!(
        failure_for(&provider, &value).await?,
        VerificationFailure::InvalidX402Version
    );

    let mut value = request_value(encoded);
    value["paymentPayload"]["x402Version"] = json!("2");
    assert_eq!(
        failure_for(&provider, &value).await?,
        VerificationFailure::InvalidX402Version
    );

    let mut value = request_value(encoded);
    value["paymentPayload"]["accepted"]["scheme"] = json!("upto");
    value["paymentRequirements"]["network"] = json!("eip155:8453");
    assert_eq!(
        failure_for(&provider, &value).await?,
        VerificationFailure::UnsupportedScheme
    );

    let mut value = request_value(encoded);
    value["paymentPayload"]["accepted"]["network"] = json!("near:mainnet");
    value["paymentRequirements"]["network"] = json!("eip155:8453");
    assert_eq!(
        failure_for(&provider, &value).await?,
        VerificationFailure::InvalidNetwork
    );

    let mismatch_cases = [
        (
            "network",
            json!("near:mainnet"),
            VerificationFailure::NetworkMismatch,
        ),
        (
            "asset",
            json!("other.testnet"),
            VerificationFailure::AssetMismatch,
        ),
        (
            "payTo",
            json!("other.testnet"),
            VerificationFailure::PayToMismatch,
        ),
        ("amount", json!("2"), VerificationFailure::AmountMismatch),
    ];
    for (field, accepted_value, expected) in mismatch_cases {
        let mut value = request_value(encoded);
        value["paymentPayload"]["accepted"][field] = accepted_value;
        assert_eq!(failure_for(&provider, &value).await?, expected);
    }

    for timeout in [json!(0), json!(-1), json!(1.5), json!("60"), Value::Null] {
        let mut value = request_value(encoded);
        value["paymentRequirements"]["maxTimeoutSeconds"] = timeout;
        assert_eq!(
            failure_for(&provider, &value).await?,
            VerificationFailure::InvalidMaxTimeout
        );
    }

    for payload in [
        json!({}),
        json!({"signedDelegateAction": 7}),
        json!({"signedDelegateAction": null}),
        json!("not-an-object"),
    ] {
        let mut value = request_value(encoded);
        value["paymentPayload"]["payload"] = payload;
        assert_eq!(
            failure_for(&provider, &value).await?,
            VerificationFailure::InvalidPayloadShape
        );
    }

    let mut value = request_value(encoded);
    value["paymentPayload"]["payload"]["signedDelegateAction"] = json!("@@not-base64@@");
    assert_eq!(
        failure_for(&provider, &value).await?,
        VerificationFailure::InvalidSignedDelegateAction
    );

    assert_eq!(rpc.calls.final_block.load(Ordering::SeqCst), 0);
    assert_eq!(rpc.calls.access_key.load(Ordering::SeqCst), 0);
    assert_eq!(rpc.calls.sender_account.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn configured_provider_network_must_match_canonical_request_network()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let mut value = request_value(&fixture.delegates[0].signed_delegate_action);
    value["paymentPayload"]["accepted"]["network"] = json!("near:mainnet");
    value["paymentRequirements"]["network"] = json!("near:mainnet");
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(Arc::clone(&rpc))?;
    assert_eq!(
        failure_for(&provider, &value).await?,
        VerificationFailure::InvalidNetwork
    );
    assert_eq!(rpc.calls.final_block.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn rejects_unsafe_action_shapes_and_exact_transfer_violations_before_rpc()
-> Result<(), Box<dyn Error>> {
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(Arc::clone(&rpc))?;

    let relayer_payer = signed_delegate_with(|delegate| {
        delegate.sender_id = "relayer.testnet".parse()?;
        Ok(())
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&relayer_payer)).await?,
        VerificationFailure::RelayerCannotBePayer
    );

    let no_actions = signed_delegate_with(|delegate| {
        delegate.actions.clear();
        Ok(())
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&no_actions)).await?,
        VerificationFailure::InvalidActionCount
    );

    let two_actions = signed_delegate_with(|delegate| {
        let first = delegate
            .actions
            .first()
            .cloned()
            .ok_or("fixture has no action")?;
        delegate.actions.push(first);
        Ok(())
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&two_actions)).await?,
        VerificationFailure::InvalidActionCount
    );

    let transfer = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![Action::Transfer(TransferAction {
                deposit: Balance::from_yoctonear(1),
            })],
        )
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&transfer)).await?,
        VerificationFailure::InvalidActionKind
    );

    let wrong_method = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "storage_deposit",
                json!({"receiver_id": "merchant.testnet", "amount": "1000000"}),
                30_000_000_000_000,
                1,
            )],
        )
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&wrong_method)).await?,
        VerificationFailure::InvalidMethodName
    );

    let wrong_token = signed_delegate_with(|delegate| {
        delegate.receiver_id = "other.testnet".parse()?;
        Ok(())
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&wrong_token)).await?,
        VerificationFailure::TokenContractMismatch
    );

    let wrong_recipient = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({"receiver_id": "attacker.testnet", "amount": "1000000"}),
                30_000_000_000_000,
                1,
            )],
        )
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&wrong_recipient)).await?,
        VerificationFailure::RecipientMismatch
    );

    let wrong_amount = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({"receiver_id": "merchant.testnet", "amount": "999999"}),
                30_000_000_000_000,
                1,
            )],
        )
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&wrong_amount)).await?,
        VerificationFailure::TransferAmountMismatch
    );

    let wrong_deposit = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({"receiver_id": "merchant.testnet", "amount": "1000000"}),
                30_000_000_000_000,
                0,
            )],
        )
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&wrong_deposit)).await?,
        VerificationFailure::InvalidAttachedDeposit
    );

    let excess_gas = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({"receiver_id": "merchant.testnet", "amount": "1000000"}),
                30_000_000_000_001,
                1,
            )],
        )
    })?;
    assert_eq!(
        failure_for(&provider, &request_value(&excess_gas)).await?,
        VerificationFailure::GasLimitExceeded
    );

    assert_eq!(rpc.calls.final_block.load(Ordering::SeqCst), 0);
    assert_eq!(rpc.calls.access_key.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn pure_ft_transfer_parser_covers_the_fuzz_entrypoint() -> Result<(), Box<dyn Error>> {
    let (receiver_id, amount) = decode_ft_transfer_args(
        br#"{"receiver_id":"merchant.testnet","amount":"1000","memo":"invoice-7"}"#,
    )?;
    assert_eq!(receiver_id, "merchant.testnet");
    assert_eq!(amount, "1000");

    for malformed in [
        br#"{"receiver_id":"merchant.testnet","amount":1000}"#.as_slice(),
        br#"{"receiver_id":"merchant.testnet","amount":"-1"}"#,
        br#"{"receiver_id":"","amount":"1000"}"#,
    ] {
        assert_eq!(
            decode_ft_transfer_args(malformed),
            Err(VerificationFailure::InvalidFtTransferArgs)
        );
    }
    Ok(())
}

#[tokio::test]
async fn ft_transfer_argument_parser_rejects_malformed_and_non_decimal_values()
-> Result<(), Box<dyn Error>> {
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(Arc::clone(&rpc))?;
    let malformed_args = [
        b"not-json".to_vec(),
        b"null".to_vec(),
        b"[]".to_vec(),
        b"{}".to_vec(),
        br#"{"receiver_id":"","amount":"1000000"}"#.to_vec(),
        br#"{"receiver_id":"merchant.testnet","amount":1000000}"#.to_vec(),
        br#"{"receiver_id":"merchant.testnet","amount":""}"#.to_vec(),
        br#"{"receiver_id":"merchant.testnet","amount":"-1"}"#.to_vec(),
        br#"{"receiver_id":"merchant.testnet","amount":"1.0"}"#.to_vec(),
        br#"{"receiver_id":"merchant.testnet","amount":"1e6"}"#.to_vec(),
    ];

    for args in malformed_args {
        let encoded = signed_delegate_with(|delegate| {
            replace_actions(
                delegate,
                vec![Action::FunctionCall(Box::new(FunctionCallAction {
                    method_name: "ft_transfer".to_owned(),
                    args,
                    gas: Gas::from_gas(30_000_000_000_000),
                    deposit: Balance::from_yoctonear(1),
                }))],
            )
        })?;
        assert_eq!(
            failure_for(&provider, &request_value(&encoded)).await?,
            VerificationFailure::InvalidFtTransferArgs
        );
    }
    assert_eq!(rpc.calls.final_block.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn gas_at_policy_cap_and_unknown_extra_fields_are_accepted() -> Result<(), Box<dyn Error>> {
    let encoded = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({
                    "receiver_id": "merchant.testnet",
                    "amount": "1000000",
                    "memo": "invoice-7",
                    "ignored": {"recipient": "attacker.testnet"}
                }),
                30_000_000_000_000,
                1,
            )],
        )
    })?;
    let mut value = request_value(&encoded);
    value["paymentRequirements"]["extra"] =
        json!({"recipient": "attacker.testnet", "amount": "999999"});
    value["paymentPayload"]["accepted"]["extra"] =
        json!({"recipient": "elsewhere.testnet", "amount": "1"});
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(rpc)?;
    let verified = provider
        .verify(&proto_request(&value)?, &VerificationPolicy::default())
        .await?;
    assert_eq!(verified.requirements.amount, 1_000_000);
    assert_eq!(verified.requirements.pay_to.as_str(), "merchant.testnet");
    Ok(())
}

#[tokio::test]
async fn verifies_fixture_against_one_pinned_final_block() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(rpc)?;
    let verified = provider
        .verify(
            &request(&fixture.delegates[0].signed_delegate_action)?,
            &VerificationPolicy::default(),
        )
        .await?;

    assert_eq!(verified.payer.as_str(), fixture.sender_id);
    assert_eq!(verified.requirements.asset.as_str(), fixture.receiver_id);
    assert_eq!(verified.requirements.pay_to.as_str(), fixture.pay_to);
    assert_eq!(verified.requirements.amount_decimal, fixture.amount);
    assert_eq!(verified.delegate_nonce.to_string(), fixture.nonce);
    assert_ne!(verified.payment_hash(), &[0; 32]);
    Ok(())
}

#[tokio::test]
async fn tampered_signature_is_not_attributable_and_never_queries_chain()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let mut signed_delegate =
        decode_signed_delegate(&fixture.delegates[0].signed_delegate_action)?.signed_delegate;
    signed_delegate.signature = test_signer("tamper.testnet")?.sign(&[0; 32]);

    let rpc = Arc::new(MockRpc::new());
    let provider = provider(Arc::clone(&rpc))?;
    let failure = provider
        .verify(
            &request(&STANDARD.encode(borsh::to_vec(&signed_delegate)?))?,
            &VerificationPolicy::default(),
        )
        .await
        .err()
        .ok_or("tampered payment unexpectedly verified")?;
    assert_eq!(failure, VerificationFailure::InvalidSignature);
    assert!(!failure.payer_is_attributable());
    assert_eq!(rpc.calls.send.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn facilitator_responses_attribute_only_cryptographically_verified_payers()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let encoded = &fixture.delegates[0].signed_delegate_action;
    let rpc = Arc::new(MockRpc::new());
    let facilitator = NearExactFacilitator::new(
        provider(Arc::clone(&rpc))?,
        NearExactFacilitatorConfig::default(),
    );

    let mut invalid_version = request_value(encoded);
    invalid_version["paymentPayload"]["x402Version"] = json!(1);
    let response = facilitator
        .verify(&proto_request(&invalid_version)?)
        .await?;
    assert_eq!(response.0["isValid"], false);
    assert_eq!(response.0["invalidReason"], "invalid_x402_version");
    assert!(response.0.get("payer").is_none());

    let mut tampered = fixture_signed_delegate()?;
    tampered.signature = test_signer("tamper.testnet")?.sign(&[0; 32]);
    let response = facilitator
        .verify(&request(&STANDARD.encode(borsh::to_vec(&tampered)?))?)
        .await?;
    assert_eq!(
        response.0["invalidReason"],
        "invalid_exact_near_payload_signature"
    );
    assert!(response.0.get("payer").is_none());

    let relayer_payer = signed_delegate_with(|delegate| {
        delegate.sender_id = "relayer.testnet".parse()?;
        Ok(())
    })?;
    let response = facilitator.verify(&request(&relayer_payer)?).await?;
    assert_eq!(
        response.0["invalidReason"],
        "invalid_exact_near_relayer_cannot_be_payer"
    );
    assert_eq!(response.0["payer"], "relayer.testnet");

    assert_eq!(rpc.calls.final_block.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn expiry_timeout_nonce_and_block_lookup_boundaries_fail_closed() -> Result<(), Box<dyn Error>>
{
    let fixture = fixture()?;
    let valid_request = request_value(&fixture.delegates[0].signed_delegate_action);

    let mut rpc = MockRpc::new();
    rpc.block_error = Some(MockFailure::Request);
    let rpc = Arc::new(rpc);
    assert_eq!(
        failure_for(&provider(Arc::clone(&rpc))?, &valid_request).await?,
        VerificationFailure::CurrentBlockHeightUnavailable
    );
    assert_eq!(rpc.calls.access_key.load(Ordering::SeqCst), 0);

    for max_block_height in [999, 1_000] {
        let encoded = signed_delegate_with(|delegate| {
            delegate.max_block_height = max_block_height;
            Ok(())
        })?;
        let rpc = Arc::new(MockRpc::new());
        assert_eq!(
            failure_for(&provider(rpc)?, &request_value(&encoded)).await?,
            VerificationFailure::DelegateActionExpired
        );
    }

    let too_far = signed_delegate_with(|delegate| {
        delegate.max_block_height = 1_061;
        Ok(())
    })?;
    let rpc = Arc::new(MockRpc::new());
    assert_eq!(
        failure_for(&provider(rpc)?, &request_value(&too_far)).await?,
        VerificationFailure::TimeoutWindowExceedsMaximum
    );

    let nonce_at_upper_bound = signed_delegate_with(|delegate| {
        delegate.nonce = 1_000_000_000;
        Ok(())
    })?;
    let rpc = Arc::new(MockRpc::new());
    assert_eq!(
        failure_for(&provider(rpc)?, &request_value(&nonce_at_upper_bound)).await?,
        VerificationFailure::DelegateNonceOutOfRange
    );

    let nonce_below_upper_bound = signed_delegate_with(|delegate| {
        delegate.nonce = 999_999_999;
        Ok(())
    })?;
    let rpc = Arc::new(MockRpc::new());
    provider(rpc)?
        .verify(
            &request(&nonce_below_upper_bound)?,
            &VerificationPolicy::default(),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn typed_access_key_failures_nonce_replay_and_permissions_are_distinct()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let value = request_value(&fixture.delegates[0].signed_delegate_action);

    for (rpc_failure, expected) in [
        (
            MockFailure::AccessKeyNotFound,
            VerificationFailure::AccessKeyNotFound,
        ),
        (
            MockFailure::AccountNotFound,
            VerificationFailure::AccessKeyLookupFailed,
        ),
        (
            MockFailure::InvalidResponse,
            VerificationFailure::AccessKeyLookupFailed,
        ),
        (
            MockFailure::Request,
            VerificationFailure::AccessKeyLookupFailed,
        ),
        (
            MockFailure::Timeout,
            VerificationFailure::AccessKeyLookupFailed,
        ),
    ] {
        let mut rpc = MockRpc::new();
        rpc.access_key_error = Some(rpc_failure);
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            expected
        );
        assert_eq!(rpc.calls.sender_account.load(Ordering::SeqCst), 0);
    }

    for nonce in [5, 6] {
        let mut rpc = MockRpc::new();
        rpc.access_key.nonce = nonce;
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            VerificationFailure::DelegateNonceAlreadyUsed
        );
        assert_eq!(rpc.calls.sender_account.load(Ordering::SeqCst), 0);
    }

    let permissions = [
        (
            AccessKeyPermissionView::FunctionCall {
                allowance: None,
                receiver_id: "usdc.testnet".to_owned(),
                method_names: vec!["ft_transfer".to_owned()],
            },
            VerificationFailure::FunctionCallKeyNotAllowed,
        ),
        (
            AccessKeyPermissionView::GasKeyFunctionCall {
                balance: Balance::from_yoctonear(1),
                num_nonces: 1,
                allowance: None,
                receiver_id: "usdc.testnet".to_owned(),
                method_names: vec!["ft_transfer".to_owned()],
            },
            VerificationFailure::UnsupportedAccessKeyPermission,
        ),
        (
            AccessKeyPermissionView::GasKeyFullAccess {
                balance: Balance::from_yoctonear(1),
                num_nonces: 1,
            },
            VerificationFailure::UnsupportedAccessKeyPermission,
        ),
    ];
    for (permission, expected) in permissions {
        let mut rpc = MockRpc::new();
        rpc.access_key.permission = permission;
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            expected
        );
        assert_eq!(rpc.calls.sender_account.load(Ordering::SeqCst), 0);
    }
    Ok(())
}

#[tokio::test]
async fn typed_account_failures_and_missing_token_code_are_distinct() -> Result<(), Box<dyn Error>>
{
    let fixture = fixture()?;
    let value = request_value(&fixture.delegates[0].signed_delegate_action);

    for (rpc_failure, expected) in [
        (
            MockFailure::AccountNotFound,
            VerificationFailure::SenderAccountNotFound,
        ),
        (
            MockFailure::Request,
            VerificationFailure::AccountLookupFailed,
        ),
        (
            MockFailure::InvalidResponse,
            VerificationFailure::AccountLookupFailed,
        ),
    ] {
        let mut rpc = MockRpc::new();
        rpc.sender_account_error = Some(rpc_failure);
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            expected
        );
        assert_eq!(rpc.calls.token_account.load(Ordering::SeqCst), 0);
    }

    for (rpc_failure, expected) in [
        (
            MockFailure::AccountNotFound,
            VerificationFailure::TokenAccountNotFound,
        ),
        (
            MockFailure::Request,
            VerificationFailure::TokenAccountLookupFailed,
        ),
        (
            MockFailure::InvalidResponse,
            VerificationFailure::TokenAccountLookupFailed,
        ),
    ] {
        let mut rpc = MockRpc::new();
        rpc.token_account_error = Some(rpc_failure);
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            expected
        );
        assert_eq!(rpc.calls.balance.load(Ordering::SeqCst), 0);
    }

    let mut rpc = MockRpc::new();
    rpc.token_account.code_hash = CryptoHash::default();
    let rpc = Arc::new(rpc);
    assert_eq!(
        failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
        VerificationFailure::TokenContractHasNoCode
    );
    assert_eq!(rpc.calls.balance.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn balance_preflight_parses_u128_numerically_and_fails_closed() -> Result<(), Box<dyn Error>>
{
    let fixture = fixture()?;
    let value = request_value(&fixture.delegates[0].signed_delegate_action);

    for response in [
        b"not-json".to_vec(),
        b"10000000".to_vec(),
        br#""1.0""#.to_vec(),
        br#""-1""#.to_vec(),
        br#""""#.to_vec(),
        br#""340282366920938463463374607431768211456""#.to_vec(),
    ] {
        let mut rpc = MockRpc::new();
        rpc.balance_response = response;
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            VerificationFailure::BalanceCheckFailed
        );
        assert_eq!(rpc.calls.storage.load(Ordering::SeqCst), 0);
    }

    for rpc_failure in [
        MockFailure::MethodNotFound,
        MockFailure::InvalidResponse,
        MockFailure::Request,
    ] {
        let mut rpc = MockRpc::new();
        rpc.balance_error = Some(rpc_failure);
        let rpc = Arc::new(rpc);
        assert_eq!(
            failure_for(&provider(Arc::clone(&rpc))?, &value).await?,
            VerificationFailure::BalanceCheckFailed
        );
        assert_eq!(rpc.calls.storage.load(Ordering::SeqCst), 0);
    }

    let mut rpc = MockRpc::new();
    rpc.balance_response = br#""999999""#.to_vec();
    assert_eq!(
        failure_for(&provider(Arc::new(rpc))?, &value).await?,
        VerificationFailure::InsufficientFunds
    );

    let mut rpc = MockRpc::new();
    rpc.balance_response = br#""1000000""#.to_vec();
    provider(Arc::new(rpc))?
        .verify(&proto_request(&value)?, &VerificationPolicy::default())
        .await?;

    let huge_amount = "340282366920938463463374607431768211456";
    let huge_delegate = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({"receiver_id": "merchant.testnet", "amount": huge_amount}),
                30_000_000_000_000,
                1,
            )],
        )
    })?;
    let mut huge_value = request_value(&huge_delegate);
    huge_value["paymentRequirements"]["amount"] = json!(huge_amount);
    huge_value["paymentPayload"]["accepted"]["amount"] = json!(huge_amount);
    let mut rpc = MockRpc::new();
    rpc.balance_response = format!("\"{huge_amount}\"").into_bytes();
    assert_eq!(
        failure_for(&provider(Arc::new(rpc))?, &huge_value).await?,
        VerificationFailure::BalanceCheckFailed
    );
    Ok(())
}

#[tokio::test]
async fn storage_preflight_distinguishes_unregistered_unsupported_and_rpc_failure()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let value = request_value(&fixture.delegates[0].signed_delegate_action);

    let mut rpc = MockRpc::new();
    rpc.storage_response = b"null".to_vec();
    assert_eq!(
        failure_for(&provider(Arc::new(rpc))?, &value).await?,
        VerificationFailure::RecipientNotRegisteredForStorage
    );

    let mut rpc = MockRpc::new();
    rpc.storage_response = b"not-json".to_vec();
    assert_eq!(
        failure_for(&provider(Arc::new(rpc))?, &value).await?,
        VerificationFailure::StorageCheckFailed
    );

    for rpc_failure in [
        MockFailure::AccountNotFound,
        MockFailure::InvalidResponse,
        MockFailure::Request,
    ] {
        let mut rpc = MockRpc::new();
        rpc.storage_error = Some(rpc_failure);
        assert_eq!(
            failure_for(&provider(Arc::new(rpc))?, &value).await?,
            VerificationFailure::StorageCheckFailed
        );
    }

    let mut unsupported = MockRpc::new();
    unsupported.storage_error = Some(MockFailure::MethodNotFound);
    provider(Arc::new(unsupported))?
        .verify(&proto_request(&value)?, &VerificationPolicy::default())
        .await?;

    for registered in [
        b"{}".to_vec(),
        b"true".to_vec(),
        br#""registered""#.to_vec(),
    ] {
        let mut rpc = MockRpc::new();
        rpc.storage_response = registered;
        provider(Arc::new(rpc))?
            .verify(&proto_request(&value)?, &VerificationPolicy::default())
            .await?;
    }

    let invalid_pay_to = "not a near account";
    let encoded = signed_delegate_with(|delegate| {
        replace_actions(
            delegate,
            vec![function_call(
                "ft_transfer",
                json!({"receiver_id": invalid_pay_to, "amount": "1000000"}),
                30_000_000_000_000,
                1,
            )],
        )
    })?;
    let mut invalid_value = request_value(&encoded);
    invalid_value["paymentRequirements"]["payTo"] = json!(invalid_pay_to);
    invalid_value["paymentPayload"]["accepted"]["payTo"] = json!(invalid_pay_to);
    assert_eq!(
        failure_for(&provider(Arc::new(MockRpc::new()))?, &invalid_value).await?,
        VerificationFailure::StorageCheckFailed
    );
    Ok(())
}

#[tokio::test]
async fn prepare_is_deterministic_and_has_no_network_side_effect() -> Result<(), Box<dyn Error>> {
    let fixture = fixture()?;
    let rpc = Arc::new(MockRpc::new());
    let provider = provider(Arc::clone(&rpc))?;
    let verified = provider
        .verify(
            &request(&fixture.delegates[0].signed_delegate_action)?,
            &VerificationPolicy::default(),
        )
        .await?;
    let head = provider.relayer_head().await?;
    let first = provider.prepare_outer_transaction(&verified, head)?;
    let second = provider.prepare_outer_transaction(&verified, head)?;

    assert_eq!(first.transaction_hash, second.transaction_hash);
    assert_eq!(
        first.signed_transaction_bytes(),
        second.signed_transaction_bytes()
    );
    assert_eq!(first.relayer_nonce, 1);
    assert_eq!(rpc.calls.send.load(Ordering::SeqCst), 0);
    assert_eq!(
        signed_transaction_hash(first.signed_transaction_bytes())?,
        first.transaction_hash
    );

    let signed = SignedTransaction::try_from_slice(first.signed_transaction_bytes())?;
    let Transaction::V0(transaction) = signed.transaction else {
        return Err("prepared transaction was not Transaction::V0".into());
    };
    assert_eq!(transaction.receiver_id, verified.payer);
    assert_eq!(transaction.actions.len(), 1);
    assert!(matches!(transaction.actions[0], Action::Delegate(_)));

    let mut trailing = first.signed_transaction_bytes().to_vec();
    trailing.push(0);
    assert!(matches!(
        signed_transaction_hash(&trailing),
        Err(NearRpcError::InvalidSignedTransaction)
    ));

    let mut corrupt_signature = first.signed_transaction_bytes().to_vec();
    let Some(last) = corrupt_signature.last_mut() else {
        return Err("prepared transaction bytes are empty".into());
    };
    *last ^= 0xff;
    assert!(matches!(
        signed_transaction_hash(&corrupt_signature),
        Err(NearRpcError::InvalidSignedTransaction)
    ));
    assert!(matches!(
        provider.broadcast_exact(&corrupt_signature).await,
        Err(NearRpcError::InvalidSignedTransaction)
    ));
    assert_eq!(rpc.calls.send.load(Ordering::SeqCst), 0);

    assert!(matches!(
        provider.prepare_outer_transaction(
            &verified,
            RelayerHead {
                block_height: head.block_height,
                block_hash: head.block_hash,
                access_key_nonce: u64::MAX,
            },
        ),
        Err(NearRpcError::InvalidResponse("relayer nonce overflow"))
    ));
    Ok(())
}

#[tokio::test]
async fn readiness_apis_report_rpc_identity_finality_and_pinned_relayer_state()
-> Result<(), Box<dyn Error>> {
    let primary = Arc::new(MockRpc::new());
    let mut backup_rpc = MockRpc::new();
    backup_rpc.network_id = "testnet".to_owned();
    backup_rpc.block = FinalBlock {
        height: 999,
        hash: CryptoHash::hash_bytes(b"backup-final-block"),
    };
    let backup = Arc::new(backup_rpc);
    let chain_provider = provider(Arc::clone(&primary))?.with_backup_rpc(backup);

    assert_eq!(chain_provider.rpc_network_id().await?, "testnet");
    assert_eq!(chain_provider.backup_rpc_network_id().await?, "testnet");
    assert_eq!(chain_provider.rpc_final_block().await?, primary.block);
    assert_eq!(chain_provider.backup_rpc_final_block().await?.height, 999);

    let status = chain_provider.relayer_status().await?;
    assert_eq!(status.block_height, primary.block.height);
    assert_eq!(status.block_hash, primary.block.hash);
    assert_eq!(status.access_key_nonce, 0);
    assert_eq!(status.account.amount.as_yoctonear(), 10_u128.pow(24));

    let provider_without_backup = provider(Arc::new(MockRpc::new()))?;
    assert!(matches!(
        provider_without_backup.backup_rpc_network_id().await,
        Err(NearRpcError::InvalidResponse(
            "backup RPC is not configured"
        ))
    ));
    assert!(matches!(
        provider_without_backup.backup_rpc_final_block().await,
        Err(NearRpcError::InvalidResponse(
            "backup RPC is not configured"
        ))
    ));
    Ok(())
}

#[tokio::test]
async fn relayer_readiness_rejects_non_full_access_key_before_account_lookup()
-> Result<(), Box<dyn Error>> {
    let mut rpc = MockRpc::new();
    rpc.access_key.permission = AccessKeyPermissionView::FunctionCall {
        allowance: None,
        receiver_id: "relayer.testnet".to_owned(),
        method_names: Vec::new(),
    };
    let rpc = Arc::new(rpc);
    assert!(matches!(
        provider(Arc::clone(&rpc))?.relayer_status().await,
        Err(NearRpcError::InvalidResponse(
            "relayer key is not full access"
        ))
    ));
    assert_eq!(rpc.calls.sender_account.load(Ordering::SeqCst), 0);
    Ok(())
}

fn outcome(
    id: CryptoHash,
    executor_id: AccountId,
    receipt_ids: Vec<CryptoHash>,
    status: ExecutionStatusView,
) -> ExecutionOutcomeWithIdView {
    ExecutionOutcomeWithIdView {
        proof: Vec::new(),
        block_hash: CryptoHash::hash_bytes(b"outcome-block"),
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

fn transaction_view() -> Result<SignedTransactionView, Box<dyn Error>> {
    let signer = test_signer("relayer.testnet")?;
    let transaction = Transaction::V0(TransactionV0 {
        signer_id: signer.get_account_id(),
        public_key: signer.public_key(),
        nonce: 1,
        receiver_id: "alice.testnet".parse()?,
        block_hash: CryptoHash::hash_bytes(b"transaction-block"),
        actions: Vec::new(),
    });
    let (hash, _) = transaction.get_hash_and_size();
    Ok(SignedTransaction::new(signer.sign(hash.as_ref()), transaction).into())
}

fn successful_outcome() -> Result<FinalExecutionOutcomeView, Box<dyn Error>> {
    let relayer = "relayer.testnet".parse()?;
    let payer = "alice.testnet".parse()?;
    let token = "usdc.testnet".parse()?;
    let delegate_id = CryptoHash::hash_bytes(b"delegate-receipt");
    let token_id = CryptoHash::hash_bytes(b"token-receipt");
    let transaction = transaction_view()?;
    let transaction_hash = transaction.hash;
    Ok(FinalExecutionOutcomeView {
        status: FinalExecutionStatus::SuccessValue(Vec::new()),
        transaction,
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
                token,
                Vec::new(),
                ExecutionStatusView::SuccessValue(Vec::new()),
            ),
        ],
    })
}

#[test]
fn final_outcome_identity_is_bound_to_prepared_transaction() -> Result<(), Box<dyn Error>> {
    let relayer: AccountId = "relayer.testnet".parse()?;
    let payer: AccountId = "alice.testnet".parse()?;
    let outcome = successful_outcome()?;
    let transaction_hash = outcome.transaction.hash;
    validate_final_outcome_identity(&outcome, transaction_hash, &relayer, &payer)?;

    let mut wrong_hash = outcome.clone();
    wrong_hash.transaction.hash = CryptoHash::hash_bytes(b"wrong-transaction");
    assert_eq!(
        validate_final_outcome_identity(&wrong_hash, transaction_hash, &relayer, &payer).err(),
        Some(ReceiptValidationError::TransactionHashMismatch)
    );

    let mut wrong_outcome_id = outcome.clone();
    wrong_outcome_id.transaction_outcome.id = CryptoHash::hash_bytes(b"wrong-outcome");
    assert_eq!(
        validate_final_outcome_identity(&wrong_outcome_id, transaction_hash, &relayer, &payer)
            .err(),
        Some(ReceiptValidationError::TransactionOutcomeIdMismatch)
    );

    let mut wrong_executor = outcome.clone();
    wrong_executor.transaction_outcome.outcome.executor_id = "attacker.testnet".parse()?;
    assert_eq!(
        validate_final_outcome_identity(&wrong_executor, transaction_hash, &relayer, &payer).err(),
        Some(ReceiptValidationError::InvalidTransactionExecutor)
    );
    Ok(())
}

fn execution_failure() -> TxExecutionError {
    TxExecutionError::InvalidTxError(InvalidTxError::InvalidSignature)
}

#[test]
fn receipt_graph_requires_direct_inner_token_success() -> Result<(), Box<dyn Error>> {
    let payer = "alice.testnet".parse()?;
    let token = "usdc.testnet".parse()?;
    let outcome = successful_outcome()?;
    let success = interpret_final_outcome(&outcome, &payer, &token)?;
    assert_eq!(success.receipt_id, CryptoHash::hash_bytes(b"token-receipt"));

    let mut outer_only = successful_outcome()?;
    outer_only.receipts_outcome.truncate(1);
    assert_eq!(
        interpret_final_outcome(&outer_only, &payer, &token).err(),
        Some(ReceiptValidationError::IncompleteReceiptGraph)
    );

    let mut pending_inner = successful_outcome()?;
    let Some(token_outcome) = pending_inner.receipts_outcome.get_mut(1) else {
        return Err("missing token fixture outcome".into());
    };
    token_outcome.outcome.status = ExecutionStatusView::Unknown;
    assert_eq!(
        interpret_final_outcome(&pending_inner, &payer, &token).err(),
        Some(ReceiptValidationError::TokenReceiptNotSuccessful)
    );
    Ok(())
}

#[test]
fn receipt_graph_rejects_nonfinal_and_failed_outer_states_first() -> Result<(), Box<dyn Error>> {
    let payer = "alice.testnet".parse()?;
    let token = "usdc.testnet".parse()?;

    let mut not_started = successful_outcome()?;
    not_started.status = FinalExecutionStatus::NotStarted;
    assert_eq!(
        interpret_final_outcome(&not_started, &payer, &token).err(),
        Some(ReceiptValidationError::NotStarted)
    );

    let mut pending = successful_outcome()?;
    pending.status = FinalExecutionStatus::Started;
    assert_eq!(
        interpret_final_outcome(&pending, &payer, &token).err(),
        Some(ReceiptValidationError::Pending)
    );

    let mut final_failure = successful_outcome()?;
    final_failure.status = FinalExecutionStatus::Failure(execution_failure());
    assert!(matches!(
        interpret_final_outcome(&final_failure, &payer, &token),
        Err(ReceiptValidationError::FinalFailure(_))
    ));

    let mut transaction_failure = successful_outcome()?;
    transaction_failure.transaction_outcome.outcome.status =
        ExecutionStatusView::Failure(execution_failure());
    assert!(matches!(
        interpret_final_outcome(&transaction_failure, &payer, &token),
        Err(ReceiptValidationError::TransactionFailure(_))
    ));

    let mut transaction_unknown = successful_outcome()?;
    transaction_unknown.transaction_outcome.outcome.status = ExecutionStatusView::Unknown;
    assert_eq!(
        interpret_final_outcome(&transaction_unknown, &payer, &token).err(),
        Some(ReceiptValidationError::IncompleteReceiptGraph)
    );
    Ok(())
}

#[test]
fn receipt_graph_rejects_invalid_delegate_edges_and_outcomes() -> Result<(), Box<dyn Error>> {
    let payer = "alice.testnet".parse()?;
    let token = "usdc.testnet".parse()?;

    let mut no_delegate = successful_outcome()?;
    no_delegate.transaction_outcome.outcome.receipt_ids.clear();
    assert_eq!(
        interpret_final_outcome(&no_delegate, &payer, &token).err(),
        Some(ReceiptValidationError::InvalidDelegateReceiptCount)
    );

    let mut two_delegates = successful_outcome()?;
    two_delegates
        .transaction_outcome
        .outcome
        .receipt_ids
        .push(CryptoHash::hash_bytes(b"second-delegate"));
    assert_eq!(
        interpret_final_outcome(&two_delegates, &payer, &token).err(),
        Some(ReceiptValidationError::InvalidDelegateReceiptCount)
    );

    let mut missing_delegate = successful_outcome()?;
    missing_delegate.transaction_outcome.outcome.receipt_ids[0] =
        CryptoHash::hash_bytes(b"missing-delegate");
    assert_eq!(
        interpret_final_outcome(&missing_delegate, &payer, &token).err(),
        Some(ReceiptValidationError::MissingDelegateReceipt)
    );

    let mut wrong_executor = successful_outcome()?;
    let Some(delegate) = wrong_executor.receipts_outcome.first_mut() else {
        return Err("missing delegate outcome".into());
    };
    delegate.outcome.executor_id = "attacker.testnet".parse()?;
    assert_eq!(
        interpret_final_outcome(&wrong_executor, &payer, &token).err(),
        Some(ReceiptValidationError::InvalidDelegateExecutor)
    );

    let mut failed_delegate = successful_outcome()?;
    let Some(delegate) = failed_delegate.receipts_outcome.first_mut() else {
        return Err("missing delegate outcome".into());
    };
    delegate.outcome.status = ExecutionStatusView::Failure(execution_failure());
    assert!(matches!(
        interpret_final_outcome(&failed_delegate, &payer, &token),
        Err(ReceiptValidationError::DelegateFailure(_))
    ));

    let mut unknown_delegate = successful_outcome()?;
    let Some(delegate) = unknown_delegate.receipts_outcome.first_mut() else {
        return Err("missing delegate outcome".into());
    };
    delegate.outcome.status = ExecutionStatusView::Unknown;
    assert_eq!(
        interpret_final_outcome(&unknown_delegate, &payer, &token).err(),
        Some(ReceiptValidationError::IncompleteReceiptGraph)
    );
    Ok(())
}

#[test]
fn receipt_graph_rejects_missing_duplicate_cyclic_and_failed_reachable_nodes()
-> Result<(), Box<dyn Error>> {
    let payer = "alice.testnet".parse()?;
    let token = "usdc.testnet".parse()?;

    let mut duplicate = successful_outcome()?;
    let Some(token_outcome) = duplicate.receipts_outcome.get(1).cloned() else {
        return Err("missing token outcome".into());
    };
    duplicate.receipts_outcome.push(token_outcome);
    assert_eq!(
        interpret_final_outcome(&duplicate, &payer, &token).err(),
        Some(ReceiptValidationError::DuplicateReceiptOutcome)
    );

    let mut missing_descendant = successful_outcome()?;
    let Some(token_outcome) = missing_descendant.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome
        .outcome
        .receipt_ids
        .push(CryptoHash::hash_bytes(b"missing-descendant"));
    assert_eq!(
        interpret_final_outcome(&missing_descendant, &payer, &token).err(),
        Some(ReceiptValidationError::IncompleteReceiptGraph)
    );

    let mut failed_descendant = successful_outcome()?;
    let failed_id = CryptoHash::hash_bytes(b"failed-descendant");
    let Some(token_outcome) = failed_descendant.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome.outcome.receipt_ids.push(failed_id);
    failed_descendant.receipts_outcome.push(outcome(
        failed_id,
        "other.testnet".parse()?,
        Vec::new(),
        ExecutionStatusView::Failure(execution_failure()),
    ));
    assert!(matches!(
        interpret_final_outcome(&failed_descendant, &payer, &token),
        Err(ReceiptValidationError::ReachableReceiptFailure(_))
    ));

    let mut cyclic = successful_outcome()?;
    let delegate_id = CryptoHash::hash_bytes(b"delegate-receipt");
    let Some(token_outcome) = cyclic.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome.outcome.receipt_ids.push(delegate_id);
    assert_eq!(
        interpret_final_outcome(&cyclic, &payer, &token).err(),
        Some(ReceiptValidationError::CyclicReceiptGraph)
    );
    Ok(())
}

#[test]
fn receipt_graph_requires_exactly_one_direct_token_success_value() -> Result<(), Box<dyn Error>> {
    let payer = "alice.testnet".parse()?;
    let token = "usdc.testnet".parse()?;

    let mut no_token = successful_outcome()?;
    let Some(token_outcome) = no_token.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome.outcome.executor_id = "other.testnet".parse()?;
    assert_eq!(
        interpret_final_outcome(&no_token, &payer, &token).err(),
        Some(ReceiptValidationError::InvalidTokenReceiptCount)
    );

    let mut unreachable_token = no_token;
    unreachable_token.receipts_outcome.push(outcome(
        CryptoHash::hash_bytes(b"unreachable-token"),
        token.clone(),
        Vec::new(),
        ExecutionStatusView::SuccessValue(Vec::new()),
    ));
    assert_eq!(
        interpret_final_outcome(&unreachable_token, &payer, &token).err(),
        Some(ReceiptValidationError::InvalidTokenReceiptCount)
    );

    let mut two_tokens = successful_outcome()?;
    let second_token_id = CryptoHash::hash_bytes(b"second-token");
    let Some(delegate) = two_tokens.receipts_outcome.first_mut() else {
        return Err("missing delegate outcome".into());
    };
    delegate.outcome.receipt_ids.push(second_token_id);
    two_tokens.receipts_outcome.push(outcome(
        second_token_id,
        token.clone(),
        Vec::new(),
        ExecutionStatusView::SuccessValue(Vec::new()),
    ));
    assert_eq!(
        interpret_final_outcome(&two_tokens, &payer, &token).err(),
        Some(ReceiptValidationError::InvalidTokenReceiptCount)
    );

    let mut success_receipt_id = successful_outcome()?;
    let Some(token_outcome) = success_receipt_id.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome.outcome.status =
        ExecutionStatusView::SuccessReceiptId(CryptoHash::hash_bytes(b"promise-result"));
    assert_eq!(
        interpret_final_outcome(&success_receipt_id, &payer, &token).err(),
        Some(ReceiptValidationError::TokenReceiptNotSuccessful)
    );

    let mut failed_token = successful_outcome()?;
    let Some(token_outcome) = failed_token.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome.outcome.status = ExecutionStatusView::Failure(execution_failure());
    assert!(matches!(
        interpret_final_outcome(&failed_token, &payer, &token),
        Err(ReceiptValidationError::ReachableReceiptFailure(_))
    ));

    let mut value = successful_outcome()?;
    let Some(token_outcome) = value.receipts_outcome.get_mut(1) else {
        return Err("missing token outcome".into());
    };
    token_outcome.outcome.status = ExecutionStatusView::SuccessValue(vec![1, 2, 3]);
    assert_eq!(
        interpret_final_outcome(&value, &payer, &token)?.value,
        vec![1, 2, 3]
    );
    Ok(())
}
