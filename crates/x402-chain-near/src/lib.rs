//! NEAR support for x402-rs.
//!
//! This crate implements the x402 v2 `exact` mechanism for classic NEP-366
//! signed delegate actions carrying one NEP-141 `ft_transfer`. Settlement is
//! intentionally split into verify, prepare, broadcast, reconcile, and receipt
//! interpretation phases so callers can durably journal a transaction before
//! any network side effect.

#![forbid(unsafe_code)]

mod mechanism;
mod provider;
mod receipt;
mod rpc;
mod types;

pub use mechanism::{
    DecodedSignedDelegate, NearExactFacilitator, NearExactFacilitatorConfig, V2NearExact,
    decode_ft_transfer_args, decode_signed_delegate, signed_delegate_hash,
};
pub use provider::{
    NearChainProvider, NearRelayerSigner, NearSettlementCoordinator, RelayerHead, RelayerStatus,
    SettlementDisposition,
};
pub use receipt::{
    ReceiptValidationError, SuccessfulTransferReceipt, interpret_final_outcome,
    validate_final_outcome_identity,
};
pub use rpc::{
    FinalBlock, JsonRpcNearRpc, NearRpc, NearRpcError, decode_signed_transaction,
    signed_transaction_hash,
};
pub use types::{
    DEFAULT_MAX_SPONSORED_GAS, ExactNearPayload, ExactScheme, NearNetwork, NearPaymentPayload,
    NearPaymentRequirements, NearSettleRequest, NearVerifyRequest, PreparedTransaction,
    TransactionLookup, VerificationFailure, VerificationPolicy, VerifiedPayment,
    VerifiedRequirements,
};

pub const NEAR_MAINNET: &str = "near:mainnet";
pub const NEAR_TESTNET: &str = "near:testnet";

#[cfg(test)]
mod tests;
