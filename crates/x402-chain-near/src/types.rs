use std::fmt;

use near_crypto::PublicKey;
use near_primitives::{
    action::delegate::SignedDelegateAction,
    hash::CryptoHash,
    types::{AccountId, BlockHeight, Nonce},
    views::{FinalExecutionOutcomeView, TxExecutionStatus},
};
use serde::{Deserialize, Serialize};
use x402_types::{chain::ChainId, lit_str, proto};

use crate::{NEAR_MAINNET, NEAR_TESTNET};

pub const DEFAULT_MAX_SPONSORED_GAS: u64 = 30_000_000_000_000;
pub(crate) const ONE_YOCTO: u128 = 1;
pub(crate) const NONCE_RANGE_MULTIPLIER: u64 = 1_000_000;
pub(crate) const PAYMENT_HASH_DOMAIN: &[u8] = b"x402-near/signed-delegate/v1";

lit_str!(ExactScheme, "exact");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NearNetwork {
    Mainnet,
    Testnet,
}

impl NearNetwork {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mainnet => NEAR_MAINNET,
            Self::Testnet => NEAR_TESTNET,
        }
    }

    #[must_use]
    pub fn chain_id(self) -> ChainId {
        match self {
            Self::Mainnet => ChainId::new("near", "mainnet"),
            Self::Testnet => ChainId::new("near", "testnet"),
        }
    }
}

impl fmt::Display for NearNetwork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for NearNetwork {
    type Error = VerificationFailure;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            NEAR_MAINNET => Ok(Self::Mainnet),
            NEAR_TESTNET => Ok(Self::Testnet),
            _ => Err(VerificationFailure::InvalidNetwork),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactNearPayload {
    pub signed_delegate_action: String,
}

impl fmt::Debug for ExactNearPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactNearPayload")
            .field("signed_delegate_action", &"<redacted>")
            .finish()
    }
}

pub type NearPaymentRequirements =
    proto::v2::PaymentRequirements<ExactScheme, String, String, Option<serde_json::Value>>;
pub type NearPaymentPayload = proto::v2::PaymentPayload<NearPaymentRequirements, ExactNearPayload>;
pub type NearVerifyRequest = proto::v2::VerifyRequest<NearPaymentPayload, NearPaymentRequirements>;
pub type NearSettleRequest = NearVerifyRequest;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationPolicy {
    pub max_sponsored_gas: u64,
}

impl Default for VerificationPolicy {
    fn default() -> Self {
        Self {
            max_sponsored_gas: DEFAULT_MAX_SPONSORED_GAS,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRequirements {
    pub network: NearNetwork,
    pub asset: AccountId,
    pub pay_to: AccountId,
    pub amount: u128,
    pub amount_decimal: String,
    pub max_timeout_seconds: u64,
}

#[derive(Clone)]
pub struct VerifiedPayment {
    pub payer: AccountId,
    pub payer_public_key: PublicKey,
    pub delegate_nonce: Nonce,
    pub max_block_height: BlockHeight,
    pub requirements: VerifiedRequirements,
    signed_delegate: SignedDelegateAction,
    signed_delegate_bytes: Vec<u8>,
    payment_hash: [u8; 32],
}

impl VerifiedPayment {
    #[must_use]
    pub fn signed_delegate(&self) -> &SignedDelegateAction {
        &self.signed_delegate
    }

    #[must_use]
    pub fn signed_delegate_bytes(&self) -> &[u8] {
        &self.signed_delegate_bytes
    }

    #[must_use]
    pub const fn payment_hash(&self) -> &[u8; 32] {
        &self.payment_hash
    }

    pub(crate) fn new(
        requirements: VerifiedRequirements,
        signed_delegate: SignedDelegateAction,
        signed_delegate_bytes: Vec<u8>,
        payment_hash: [u8; 32],
    ) -> Self {
        let delegate = &signed_delegate.delegate_action;
        Self {
            payer: delegate.sender_id.clone(),
            payer_public_key: delegate.public_key.clone(),
            delegate_nonce: delegate.nonce,
            max_block_height: delegate.max_block_height,
            requirements,
            signed_delegate,
            signed_delegate_bytes,
            payment_hash,
        }
    }
}

impl fmt::Debug for VerifiedPayment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPayment")
            .field("payer", &self.payer)
            .field("payer_public_key", &self.payer_public_key)
            .field("delegate_nonce", &self.delegate_nonce)
            .field("max_block_height", &self.max_block_height)
            .field("requirements", &self.requirements)
            .field("signed_delegate", &"<redacted>")
            .field("signed_delegate_bytes", &"<redacted>")
            .field("payment_hash", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct PreparedTransaction {
    pub transaction_hash: CryptoHash,
    pub relayer_nonce: Nonce,
    pub signer_id: AccountId,
    pub signer_public_key: PublicKey,
    signed_transaction_bytes: Vec<u8>,
}

impl PreparedTransaction {
    #[must_use]
    pub fn signed_transaction_bytes(&self) -> &[u8] {
        &self.signed_transaction_bytes
    }

    pub(crate) fn new(
        transaction_hash: CryptoHash,
        relayer_nonce: Nonce,
        signer_id: AccountId,
        signer_public_key: PublicKey,
        signed_transaction_bytes: Vec<u8>,
    ) -> Self {
        Self {
            transaction_hash,
            relayer_nonce,
            signer_id,
            signer_public_key,
            signed_transaction_bytes,
        }
    }
}

impl fmt::Debug for PreparedTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedTransaction")
            .field("transaction_hash", &self.transaction_hash)
            .field("relayer_nonce", &self.relayer_nonce)
            .field("signer_id", &self.signer_id)
            .field("signer_public_key", &self.signer_public_key)
            .field("signed_transaction_bytes", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub enum TransactionLookup {
    Unknown,
    Pending(TxExecutionStatus),
    Final(Box<FinalExecutionOutcomeView>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationFailure {
    InvalidX402Version,
    UnsupportedScheme,
    InvalidNetwork,
    NetworkMismatch,
    AssetMismatch,
    PayToMismatch,
    AmountMismatch,
    InvalidMaxTimeout,
    InvalidPayloadShape,
    InvalidSignedDelegateAction,
    InvalidSignature,
    NoRelayerConfigured,
    RelayerCannotBePayer,
    InvalidActionCount,
    InvalidActionKind,
    InvalidMethodName,
    TokenContractMismatch,
    InvalidFtTransferArgs,
    RecipientMismatch,
    TransferAmountMismatch,
    InvalidAttachedDeposit,
    GasLimitExceeded,
    CurrentBlockHeightUnavailable,
    DelegateActionExpired,
    TimeoutWindowExceedsMaximum,
    DelegateNonceOutOfRange,
    AccessKeyLookupFailed,
    AccessKeyNotFound,
    DelegateNonceAlreadyUsed,
    FunctionCallKeyNotAllowed,
    UnsupportedAccessKeyPermission,
    AccountLookupFailed,
    SenderAccountNotFound,
    TokenAccountLookupFailed,
    TokenAccountNotFound,
    TokenContractHasNoCode,
    BalanceCheckFailed,
    InsufficientFunds,
    StorageCheckFailed,
    RecipientNotRegisteredForStorage,
}

impl VerificationFailure {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::InvalidX402Version => "invalid_x402_version",
            Self::UnsupportedScheme => "unsupported_scheme",
            Self::InvalidNetwork => "invalid_network",
            Self::NetworkMismatch => "invalid_exact_near_network_mismatch",
            Self::AssetMismatch => "invalid_exact_near_asset_mismatch",
            Self::PayToMismatch => "invalid_exact_near_pay_to_mismatch",
            Self::AmountMismatch => "invalid_exact_near_amount_mismatch",
            Self::InvalidMaxTimeout => "invalid_exact_near_max_timeout",
            Self::InvalidPayloadShape => "invalid_exact_near_payload_shape",
            Self::InvalidSignedDelegateAction => {
                "invalid_exact_near_payload_signed_delegate_action"
            }
            Self::InvalidSignature => "invalid_exact_near_payload_signature",
            Self::NoRelayerConfigured => "invalid_exact_near_no_relayer_configured",
            Self::RelayerCannotBePayer => "invalid_exact_near_relayer_cannot_be_payer",
            Self::InvalidActionCount => "invalid_exact_near_payload_action_count",
            Self::InvalidActionKind => "invalid_exact_near_payload_action_kind",
            Self::InvalidMethodName => "invalid_exact_near_payload_method_name",
            Self::TokenContractMismatch => "invalid_exact_near_payload_token_contract_mismatch",
            Self::InvalidFtTransferArgs => "invalid_exact_near_payload_ft_transfer_args",
            Self::RecipientMismatch => "invalid_exact_near_payload_recipient_mismatch",
            Self::TransferAmountMismatch => "invalid_exact_near_payload_amount_mismatch",
            Self::InvalidAttachedDeposit => "invalid_exact_near_payload_attached_deposit",
            Self::GasLimitExceeded => "invalid_exact_near_payload_gas_limit_exceeded",
            Self::CurrentBlockHeightUnavailable => {
                "invalid_exact_near_current_block_height_unavailable"
            }
            Self::DelegateActionExpired => "invalid_exact_near_payload_delegate_action_expired",
            Self::TimeoutWindowExceedsMaximum => {
                "invalid_exact_near_payload_delegate_action_timeout_window_exceeds_max_timeout"
            }
            Self::DelegateNonceOutOfRange => {
                "invalid_exact_near_payload_delegate_action_nonce_out_of_range"
            }
            Self::AccessKeyLookupFailed => "invalid_exact_near_access_key_lookup_failed",
            Self::AccessKeyNotFound => "invalid_exact_near_access_key_not_found",
            Self::DelegateNonceAlreadyUsed => {
                "invalid_exact_near_payload_delegate_action_nonce_already_used"
            }
            Self::FunctionCallKeyNotAllowed => "invalid_exact_near_function_call_key_not_allowed",
            Self::UnsupportedAccessKeyPermission => {
                "invalid_exact_near_unsupported_access_key_permission"
            }
            Self::AccountLookupFailed => "invalid_exact_near_account_lookup_failed",
            Self::SenderAccountNotFound => "invalid_exact_near_sender_account_not_found",
            Self::TokenAccountLookupFailed => "invalid_exact_near_token_account_lookup_failed",
            Self::TokenAccountNotFound => "invalid_exact_near_token_account_not_found",
            Self::TokenContractHasNoCode => "invalid_exact_near_token_contract_no_code",
            Self::BalanceCheckFailed => "invalid_exact_near_balance_check_failed",
            Self::InsufficientFunds => "insufficient_funds",
            Self::StorageCheckFailed => "invalid_exact_near_storage_check_failed",
            Self::RecipientNotRegisteredForStorage => {
                "invalid_exact_near_recipient_not_registered_for_storage"
            }
        }
    }

    #[must_use]
    pub const fn payer_is_attributable(self) -> bool {
        !matches!(
            self,
            Self::InvalidX402Version
                | Self::UnsupportedScheme
                | Self::InvalidNetwork
                | Self::NetworkMismatch
                | Self::AssetMismatch
                | Self::PayToMismatch
                | Self::AmountMismatch
                | Self::InvalidMaxTimeout
                | Self::InvalidPayloadShape
                | Self::InvalidSignedDelegateAction
                | Self::InvalidSignature
        )
    }
}

impl fmt::Display for VerificationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for VerificationFailure {}
