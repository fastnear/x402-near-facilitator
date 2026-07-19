use std::{collections::HashMap, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use borsh::BorshDeserialize;
use near_crypto::KeyType;
use near_primitives::{
    action::Action,
    action::delegate::SignedDelegateAction,
    hash::CryptoHash,
    types::{AccountId, Balance},
    views::AccessKeyPermissionView,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use x402_types::{
    chain::{ChainId, ChainProviderOps},
    proto,
    scheme::{
        X402SchemeFacilitator, X402SchemeFacilitatorBuilder, X402SchemeFacilitatorError,
        X402SchemeId,
    },
};

use crate::{
    provider::{NearChainProvider, SettlementDisposition},
    rpc::NearRpcError,
    types::{
        ExactScheme, NONCE_RANGE_MULTIPLIER, NearNetwork, ONE_YOCTO, PAYMENT_HASH_DOMAIN,
        VerificationFailure, VerificationPolicy, VerifiedPayment, VerifiedRequirements,
    },
};

const FT_TRANSFER: &str = "ft_transfer";
const FT_BALANCE_OF: &str = "ft_balance_of";
const STORAGE_BALANCE_OF: &str = "storage_balance_of";

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NearExactFacilitatorConfig {
    pub max_sponsored_gas: u64,
}

impl Default for NearExactFacilitatorConfig {
    fn default() -> Self {
        Self {
            max_sponsored_gas: crate::types::DEFAULT_MAX_SPONSORED_GAS,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct V2NearExact;

impl X402SchemeId for V2NearExact {
    #[allow(clippy::unnecessary_literal_bound)]
    fn namespace(&self) -> &str {
        "near"
    }

    fn scheme(&self) -> &str {
        ExactScheme.as_ref()
    }
}

impl X402SchemeFacilitatorBuilder<&NearChainProvider> for V2NearExact {
    fn build(
        &self,
        provider: &NearChainProvider,
        config: Option<Value>,
    ) -> Result<Box<dyn X402SchemeFacilitator>, Box<dyn std::error::Error>> {
        let config = config
            .map(serde_json::from_value::<NearExactFacilitatorConfig>)
            .transpose()?
            .unwrap_or_default();
        Ok(Box::new(NearExactFacilitator::new(
            provider.clone(),
            config,
        )))
    }
}

#[derive(Clone, Debug)]
pub struct NearExactFacilitator {
    provider: NearChainProvider,
    policy: VerificationPolicy,
}

impl NearExactFacilitator {
    #[must_use]
    pub fn new(provider: NearChainProvider, config: NearExactFacilitatorConfig) -> Self {
        Self {
            provider,
            policy: VerificationPolicy {
                max_sponsored_gas: config.max_sponsored_gas,
            },
        }
    }

    #[must_use]
    pub fn provider(&self) -> &NearChainProvider {
        &self.provider
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRequest {
    x402_version: Value,
    payment_payload: WirePaymentPayload,
    payment_requirements: WireRequirements,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePaymentPayload {
    x402_version: Value,
    accepted: WireRequirements,
    payload: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRequirements {
    scheme: String,
    network: String,
    amount: String,
    pay_to: String,
    max_timeout_seconds: Value,
    asset: String,
    #[serde(default)]
    #[allow(dead_code)]
    extra: Option<Value>,
}

#[derive(Deserialize)]
struct FtTransferArgs {
    receiver_id: String,
    amount: String,
    #[serde(default)]
    #[allow(dead_code)]
    memo: Option<String>,
}

/// Decodes the strict string fields required by NEP-141 `ft_transfer`.
///
/// This pure parser is exposed for protocol fuzzing. Requirements-specific
/// recipient and amount comparisons remain part of full payment verification.
///
/// # Errors
///
/// Returns [`VerificationFailure::InvalidFtTransferArgs`] when the input is
/// not JSON with non-empty string `receiver_id` and decimal-string `amount`
/// fields.
#[doc(hidden)]
pub fn decode_ft_transfer_args(input: &[u8]) -> Result<(String, String), VerificationFailure> {
    let transfer: FtTransferArgs =
        serde_json::from_slice(input).map_err(|_| VerificationFailure::InvalidFtTransferArgs)?;
    if transfer.receiver_id.is_empty()
        || transfer.amount.is_empty()
        || !transfer.amount.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(VerificationFailure::InvalidFtTransferArgs);
    }
    Ok((transfer.receiver_id, transfer.amount))
}

pub struct DecodedSignedDelegate {
    pub bytes: Vec<u8>,
    pub signed_delegate: SignedDelegateAction,
    pub payment_hash: [u8; 32],
}

impl fmt::Debug for DecodedSignedDelegate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedSignedDelegate")
            .field("bytes", &"<redacted>")
            .field("signed_delegate", &"<redacted>")
            .field("payment_hash", &"<redacted>")
            .finish()
    }
}

/// Decodes and validates the envelope shape of a signed NEP-366 delegate action.
///
/// # Errors
///
/// Returns a protocol verification failure when base64 or Borsh decoding is
/// non-canonical, or when the public-key and signature curves do not match the
/// supported ED25519 and SECP256K1 combinations.
pub fn decode_signed_delegate(encoded: &str) -> Result<DecodedSignedDelegate, VerificationFailure> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| VerificationFailure::InvalidSignedDelegateAction)?;
    let signed_delegate = SignedDelegateAction::try_from_slice(&bytes)
        .map_err(|_| VerificationFailure::InvalidSignedDelegateAction)?;

    let key_type = signed_delegate.delegate_action.public_key.key_type();
    let signature_type = signed_delegate.signature.key_type();
    if !matches!(
        (key_type, signature_type),
        (KeyType::ED25519, KeyType::ED25519) | (KeyType::SECP256K1, KeyType::SECP256K1)
    ) {
        return Err(VerificationFailure::InvalidSignature);
    }

    let payment_hash = hash_signed_delegate_bytes(&bytes);
    Ok(DecodedSignedDelegate {
        bytes,
        signed_delegate,
        payment_hash,
    })
}

/// Computes the globally unique x402 NEAR delegate hash from a typed classic
/// NEP-366 action.
///
/// # Errors
///
/// Returns [`VerificationFailure::InvalidSignedDelegateAction`] if the typed
/// action cannot be serialized canonically.
pub fn signed_delegate_hash(
    signed_delegate: &SignedDelegateAction,
) -> Result<[u8; 32], VerificationFailure> {
    let bytes = borsh::to_vec(signed_delegate)
        .map_err(|_| VerificationFailure::InvalidSignedDelegateAction)?;
    Ok(hash_signed_delegate_bytes(&bytes))
}

fn hash_signed_delegate_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PAYMENT_HASH_DOMAIN);
    hasher.update(bytes);
    hasher.finalize().into()
}

fn invalid_response(
    failure: VerificationFailure,
    payer: Option<&AccountId>,
) -> proto::VerifyResponse {
    let mut response = json!({
        "isValid": false,
        "invalidReason": failure.reason(),
    });
    if failure.payer_is_attributable()
        && let Some(payer) = payer
    {
        response["payer"] = Value::String(payer.to_string());
    }
    proto::VerifyResponse(response)
}

fn success_response(payer: &AccountId) -> proto::VerifyResponse {
    proto::VerifyResponse(json!({
        "isValid": true,
        "payer": payer,
    }))
}

fn settlement_response(
    success: bool,
    transaction: &str,
    network: NearNetwork,
    payer: Option<&AccountId>,
    error_reason: Option<&str>,
    error_message: Option<&str>,
) -> proto::SettleResponse {
    let mut response = json!({
        "success": success,
        "transaction": transaction,
        "network": network.as_str(),
    });
    if let Some(payer) = payer {
        response["payer"] = Value::String(payer.to_string());
    }
    if let Some(reason) = error_reason {
        response["errorReason"] = Value::String(reason.to_owned());
    }
    if let Some(message) = error_message {
        response["errorMessage"] = Value::String(message.to_owned());
    }
    proto::SettleResponse(response)
}

fn value_is_two(value: &Value) -> bool {
    value.as_u64() == Some(2)
}

fn timeout_seconds(requirements: &WireRequirements) -> Result<u64, VerificationFailure> {
    requirements
        .max_timeout_seconds
        .as_u64()
        .filter(|seconds| *seconds > 0)
        .ok_or(VerificationFailure::InvalidMaxTimeout)
}

fn parse_decimal(value: &str) -> Option<u128> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

// Keeping the security checks in one ordered function makes parity with the
// reference implementation auditable and preserves its first-failure semantics.
#[allow(clippy::too_many_lines)]
pub(crate) async fn verify_proto_request(
    provider: &NearChainProvider,
    request: &proto::VerifyRequest,
    policy: &VerificationPolicy,
) -> Result<VerifiedPayment, VerificationFailure> {
    let wire: WireRequest = serde_json::from_str(request.as_str())
        .map_err(|_| VerificationFailure::InvalidPayloadShape)?;
    if !value_is_two(&wire.x402_version) || !value_is_two(&wire.payment_payload.x402_version) {
        return Err(VerificationFailure::InvalidX402Version);
    }

    let accepted = &wire.payment_payload.accepted;
    let requirements = &wire.payment_requirements;
    if accepted.scheme != "exact" || requirements.scheme != "exact" {
        return Err(VerificationFailure::UnsupportedScheme);
    }
    let network = NearNetwork::try_from(requirements.network.as_str())?;
    if network != provider.network() {
        return Err(VerificationFailure::InvalidNetwork);
    }
    if accepted.network != requirements.network {
        return Err(VerificationFailure::NetworkMismatch);
    }
    if accepted.asset != requirements.asset {
        return Err(VerificationFailure::AssetMismatch);
    }
    if accepted.pay_to != requirements.pay_to {
        return Err(VerificationFailure::PayToMismatch);
    }
    if accepted.amount != requirements.amount {
        return Err(VerificationFailure::AmountMismatch);
    }
    let max_timeout_seconds = timeout_seconds(requirements)?;

    let encoded = wire
        .payment_payload
        .payload
        .as_object()
        .and_then(|payload| payload.get("signedDelegateAction"))
        .and_then(Value::as_str)
        .ok_or(VerificationFailure::InvalidPayloadShape)?;
    let decoded = decode_signed_delegate(encoded)?;
    if !decoded.signed_delegate.verify() {
        return Err(VerificationFailure::InvalidSignature);
    }

    let delegate = &decoded.signed_delegate.delegate_action;
    let payer = &delegate.sender_id;
    let relayer = provider.relayer_account_id();
    if payer == &relayer {
        return Err(VerificationFailure::RelayerCannotBePayer);
    }

    if delegate.actions.len() != 1 {
        return Err(VerificationFailure::InvalidActionCount);
    }
    let actions = delegate.get_actions();
    let Some(Action::FunctionCall(function_call)) = actions.first() else {
        return Err(VerificationFailure::InvalidActionKind);
    };
    if function_call.method_name != FT_TRANSFER {
        return Err(VerificationFailure::InvalidMethodName);
    }
    if delegate.receiver_id.as_str() != requirements.asset {
        return Err(VerificationFailure::TokenContractMismatch);
    }
    let (receiver_id, amount) = decode_ft_transfer_args(&function_call.args)?;
    if receiver_id != requirements.pay_to {
        return Err(VerificationFailure::RecipientMismatch);
    }
    if amount != requirements.amount {
        return Err(VerificationFailure::TransferAmountMismatch);
    }
    if function_call.deposit.as_yoctonear() != ONE_YOCTO {
        return Err(VerificationFailure::InvalidAttachedDeposit);
    }
    if function_call.gas.as_gas() > policy.max_sponsored_gas {
        return Err(VerificationFailure::GasLimitExceeded);
    }

    let final_block = provider
        .rpc()
        .final_block()
        .await
        .map_err(|_| VerificationFailure::CurrentBlockHeightUnavailable)?;
    let remaining_blocks = delegate
        .max_block_height
        .checked_sub(final_block.height)
        .ok_or(VerificationFailure::DelegateActionExpired)?;
    if remaining_blocks == 0 {
        return Err(VerificationFailure::DelegateActionExpired);
    }
    let timeout_blocks = max_timeout_seconds.max(1);
    if remaining_blocks > timeout_blocks {
        return Err(VerificationFailure::TimeoutWindowExceedsMaximum);
    }
    if delegate.nonce >= final_block.height.saturating_mul(NONCE_RANGE_MULTIPLIER) {
        return Err(VerificationFailure::DelegateNonceOutOfRange);
    }

    let access_key = match provider
        .rpc()
        .view_access_key(final_block.hash, payer.clone(), delegate.public_key.clone())
        .await
    {
        Ok(access_key) => access_key,
        Err(NearRpcError::AccessKeyNotFound) => {
            return Err(VerificationFailure::AccessKeyNotFound);
        }
        Err(_) => return Err(VerificationFailure::AccessKeyLookupFailed),
    };
    if delegate.nonce <= access_key.nonce {
        return Err(VerificationFailure::DelegateNonceAlreadyUsed);
    }
    match access_key.permission {
        AccessKeyPermissionView::FullAccess => {}
        AccessKeyPermissionView::FunctionCall { .. } => {
            return Err(VerificationFailure::FunctionCallKeyNotAllowed);
        }
        AccessKeyPermissionView::GasKeyFunctionCall { .. }
        | AccessKeyPermissionView::GasKeyFullAccess { .. } => {
            return Err(VerificationFailure::UnsupportedAccessKeyPermission);
        }
    }

    match provider
        .rpc()
        .view_account(final_block.hash, payer.clone())
        .await
    {
        Ok(_) => {}
        Err(NearRpcError::AccountNotFound) => {
            return Err(VerificationFailure::SenderAccountNotFound);
        }
        Err(_) => return Err(VerificationFailure::AccountLookupFailed),
    }

    let asset = requirements
        .asset
        .parse::<AccountId>()
        .map_err(|_| VerificationFailure::TokenAccountLookupFailed)?;
    let token_account = match provider
        .rpc()
        .view_account(final_block.hash, asset.clone())
        .await
    {
        Ok(account) => account,
        Err(NearRpcError::AccountNotFound) => {
            return Err(VerificationFailure::TokenAccountNotFound);
        }
        Err(_) => return Err(VerificationFailure::TokenAccountLookupFailed),
    };
    if token_account.code_hash == CryptoHash::default() {
        return Err(VerificationFailure::TokenContractHasNoCode);
    }

    let balance_args = serde_json::to_vec(&json!({ "account_id": payer }))
        .map_err(|_| VerificationFailure::BalanceCheckFailed)?;
    let balance_bytes = provider
        .rpc()
        .call_function(
            final_block.hash,
            asset.clone(),
            FT_BALANCE_OF.to_owned(),
            balance_args,
        )
        .await
        .map_err(|_| VerificationFailure::BalanceCheckFailed)?;
    let balance_decimal: String = serde_json::from_slice(&balance_bytes)
        .map_err(|_| VerificationFailure::BalanceCheckFailed)?;
    let balance = parse_decimal(&balance_decimal).ok_or(VerificationFailure::BalanceCheckFailed)?;
    let amount =
        parse_decimal(&requirements.amount).ok_or(VerificationFailure::BalanceCheckFailed)?;
    if balance < amount {
        return Err(VerificationFailure::InsufficientFunds);
    }

    let pay_to = requirements
        .pay_to
        .parse::<AccountId>()
        .map_err(|_| VerificationFailure::StorageCheckFailed)?;
    let storage_args = serde_json::to_vec(&json!({ "account_id": pay_to }))
        .map_err(|_| VerificationFailure::StorageCheckFailed)?;
    match provider
        .rpc()
        .call_function(
            final_block.hash,
            asset.clone(),
            STORAGE_BALANCE_OF.to_owned(),
            storage_args,
        )
        .await
    {
        Ok(storage_bytes) => {
            let storage: Value = serde_json::from_slice(&storage_bytes)
                .map_err(|_| VerificationFailure::StorageCheckFailed)?;
            if storage.is_null() {
                return Err(VerificationFailure::RecipientNotRegisteredForStorage);
            }
        }
        Err(NearRpcError::MethodNotFound) => {}
        Err(_) => return Err(VerificationFailure::StorageCheckFailed),
    }

    Ok(VerifiedPayment::new(
        VerifiedRequirements {
            network,
            asset,
            pay_to,
            amount,
            amount_decimal: requirements.amount.clone(),
            max_timeout_seconds,
        },
        decoded.signed_delegate,
        decoded.bytes,
        decoded.payment_hash,
    ))
}

#[async_trait::async_trait]
impl X402SchemeFacilitator for NearExactFacilitator {
    async fn verify(
        &self,
        request: &proto::VerifyRequest,
    ) -> Result<proto::VerifyResponse, X402SchemeFacilitatorError> {
        match self.provider.verify(request, &self.policy).await {
            Ok(payment) => Ok(success_response(&payment.payer)),
            Err(failure) => {
                let payer = attributable_payer(request, failure);
                Ok(invalid_response(failure, payer.as_ref()))
            }
        }
    }

    async fn settle(
        &self,
        request: &proto::SettleRequest,
    ) -> Result<proto::SettleResponse, X402SchemeFacilitatorError> {
        let payment = match self.provider.verify(request, &self.policy).await {
            Ok(payment) => payment,
            Err(failure) => {
                let payer = attributable_payer(request, failure);
                return Ok(settlement_response(
                    false,
                    "",
                    self.provider.network(),
                    payer.as_ref(),
                    Some(failure.reason()),
                    None,
                ));
            }
        };
        let payer = payment.payer.clone();
        match self
            .provider
            .coordinate_settlement(payment)
            .await
            .map_err(|error| X402SchemeFacilitatorError::OnchainFailure(error.to_string()))?
        {
            SettlementDisposition::Succeeded { transaction } => Ok(settlement_response(
                true,
                &transaction.to_string(),
                self.provider.network(),
                Some(&payer),
                None,
                None,
            )),
            SettlementDisposition::Failed {
                transaction,
                reason,
                message,
            } => Ok(settlement_response(
                false,
                &transaction.map_or_else(String::new, |hash| hash.to_string()),
                self.provider.network(),
                Some(&payer),
                Some(&reason),
                message.as_deref(),
            )),
        }
    }

    async fn supported(&self) -> Result<proto::SupportedResponse, X402SchemeFacilitatorError> {
        let chain_id = self.provider.chain_id();
        let kinds = vec![proto::SupportedPaymentKind {
            x402_version: proto::v2::X402Version2.into(),
            scheme: "exact".to_owned(),
            network: chain_id.to_string(),
            extra: None,
        }];
        let mut signers = HashMap::with_capacity(1);
        signers.insert(chain_id, self.provider.signer_addresses());
        Ok(proto::SupportedResponse {
            kinds,
            extensions: vec!["payment-identifier".to_owned()],
            signers,
        })
    }
}

fn attributable_payer(
    request: &proto::VerifyRequest,
    failure: VerificationFailure,
) -> Option<AccountId> {
    if !failure.payer_is_attributable() {
        return None;
    }
    let wire: WireRequest = serde_json::from_str(request.as_str()).ok()?;
    let encoded = wire
        .payment_payload
        .payload
        .as_object()?
        .get("signedDelegateAction")?
        .as_str()?;
    let decoded = decode_signed_delegate(encoded).ok()?;
    decoded
        .signed_delegate
        .verify()
        .then_some(decoded.signed_delegate.delegate_action.sender_id)
}

#[allow(dead_code)]
fn _typed_aliases_compile(
    _request: crate::types::NearVerifyRequest,
    _requirements: crate::types::NearPaymentRequirements,
    _chain_id: ChainId,
    _balance: Balance,
) {
}
