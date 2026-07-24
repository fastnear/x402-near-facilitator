//! Chain-neutral settlement vocabulary shared by the settlement engine.
//!
//! The engine in [`crate::service`] currently threads NEAR primitives directly.
//! To let a second chain (eip155/EVM) ride the same durable journal and
//! recovery, the engine must speak these neutral value types instead, and a
//! [`ChainProvider`] enum dispatches inward to the concrete per-chain provider.
//!
//! Phase 0 introduces this vocabulary and the NEAR wrapping without changing
//! NEAR behavior; the EVM variant is added in Phase 1. See
//! `docs/evm-v2-design.md`.

use std::fmt;

use near_primitives::hash::CryptoHash;
use near_primitives::types::AccountId;
use near_primitives::views::FinalExecutionOutcomeView;
use x402_chain_near::{
    NearChainProvider, NearRpcError, PreparedTransaction as NearPrepared, RelayerHead,
    TransactionLookup, VerificationFailure as NearVerificationFailure, VerificationPolicy,
    VerifiedPayment as NearVerified, interpret_final_outcome, validate_final_outcome_identity,
};
use x402_types::chain::ChainProviderOps as _;
use x402_types::proto;

/// The settlement provider for the environment's chain. A closed enum (rather
/// than `dyn`) so the engine can hold one `Arc<ChainProvider>` and dispatch
/// inward with neutral value types. Phase 0 wraps NEAR; Phase 1 adds `Evm`.
#[derive(Debug)]
pub enum ChainProvider {
    /// A NEAR delegate-settlement provider.
    Near(NearChainProvider),
}

impl ChainProvider {
    /// Borrow the inner NEAR provider.
    ///
    /// Transitional bridge for Phase 0: the settlement engine still calls
    /// `NearChainProvider`'s inherent methods through this accessor while the
    /// neutral method surface is migrated cluster by cluster. Each call site is
    /// replaced by a neutral [`ChainProvider`] method before the `Evm` variant
    /// lands, at which point this accessor is removed.
    #[must_use]
    pub fn as_near(&self) -> &NearChainProvider {
        let Self::Near(provider) = self;
        provider
    }

    /// The facilitator signer/relayer account identity (NEAR account id / EVM
    /// `0x` address).
    #[must_use]
    pub fn signer_account_id(&self) -> String {
        match self {
            Self::Near(provider) => provider.relayer_account_id().to_string(),
        }
    }

    /// The facilitator signer/relayer public key (NEAR ed25519 string; EVM
    /// address, or empty).
    #[must_use]
    pub fn signer_public_key(&self) -> String {
        match self {
            Self::Near(provider) => provider.relayer_public_key().to_string(),
        }
    }

    /// Probe that both configured RPC endpoints report the expected chain and a
    /// final block. This is the chain-liveness half of readiness.
    pub async fn readiness_probe(&self) -> bool {
        match self {
            Self::Near(provider) => {
                let expected = provider.chain_id().reference;
                matches!(provider.rpc_network_id().await, Ok(network) if network == expected)
                    && matches!(
                        provider.backup_rpc_network_id().await,
                        Ok(network) if network == expected
                    )
                    && provider.rpc_final_block().await.is_ok()
                    && provider.backup_rpc_final_block().await.is_ok()
            }
        }
    }

    /// A fresh snapshot of the signer and chain head, used to gate readiness and
    /// prepare a submission. For NEAR this also enforces that the relayer key is
    /// full-access (the underlying `relayer_status` errors otherwise).
    pub async fn signer_head(&self) -> Result<SignerHead, NearRpcError> {
        match self {
            Self::Near(provider) => {
                let status = provider.relayer_status().await?;
                Ok(SignerHead {
                    chain_block_height: status.block_height,
                    chain_block_ref: status.block_hash.to_string(),
                    signer_nonce: u128::from(status.access_key_nonce),
                    signer_id: provider.relayer_account_id().to_string(),
                    signer_public_key: provider.relayer_public_key().to_string(),
                    signer_balance_atomic: status.account.amount.as_yoctonear(),
                })
            }
        }
    }

    /// Verify a raw payment against policy, returning a neutral verified payment
    /// or a neutral [`VerifyRejection`] carrying the reason and its
    /// RPC-ambiguity flag (without exposing the per-chain failure enum).
    pub async fn verify(
        &self,
        request: &proto::VerifyRequest,
        policy: &VerificationPolicy,
    ) -> Result<VerifiedPayment, VerifyRejection> {
        match self {
            Self::Near(provider) => {
                let near = provider
                    .verify(request, policy)
                    .await
                    .map_err(VerifyRejection::from_near)?;
                Ok(VerifiedPayment {
                    payer: near.payer.to_string(),
                    payment_hash: *near.payment_hash(),
                    requirements: Requirements {
                        network: near.requirements.network.as_str().to_owned(),
                        asset: near.requirements.asset.to_string(),
                        pay_to: near.requirements.pay_to.to_string(),
                        amount: near.requirements.amount,
                        amount_decimal: near.requirements.amount_decimal.clone(),
                    },
                    detail: VerifiedDetail::Near(near),
                })
            }
        }
    }

    /// Build and sign a submission from a verified payment and a signer-head
    /// snapshot. The returned [`Prepared`] is durable: recovery rebroadcasts its
    /// exact bytes and must never re-sign.
    pub fn prepare(
        &self,
        payment: &VerifiedPayment,
        head: &SignerHead,
    ) -> Result<Prepared, PrepareError> {
        match self {
            Self::Near(provider) => {
                let VerifiedDetail::Near(near_payment) = &payment.detail;
                // The neutral head carries the block reference as a string; NEAR
                // round-trips it back to a `CryptoHash` (base58, lossless). A
                // parse failure is impossible for a well-formed head and is
                // treated as a safe preparation failure (no broadcast).
                let block_hash = head
                    .chain_block_ref
                    .parse::<CryptoHash>()
                    .map_err(|_| PrepareError::InvalidSignerHead)?;
                let access_key_nonce = u64::try_from(head.signer_nonce)
                    .map_err(|_| PrepareError::InvalidSignerHead)?;
                let relayer_head = RelayerHead {
                    block_height: head.chain_block_height,
                    block_hash,
                    access_key_nonce,
                };
                let prepared = provider
                    .prepare_outer_transaction(near_payment, relayer_head)
                    .map_err(PrepareError::Provider)?;
                Ok(Prepared {
                    submit_bytes: prepared.signed_transaction_bytes().to_vec(),
                    submit_hash: prepared.transaction_hash.to_string(),
                    signer_id: prepared.signer_id.to_string(),
                    signer_public_key: prepared.signer_public_key.to_string(),
                    signer_nonce: u128::from(prepared.relayer_nonce),
                    detail: PreparedDetail::Near(prepared),
                })
            }
        }
    }

    /// Broadcast a prepared submission and classify the outcome. NEAR resolves
    /// to [`BroadcastOutcome::Terminal`] on fast finality (after receipt-graph
    /// validation), [`BroadcastOutcome::Rejected`] on deterministic rejection,
    /// or [`BroadcastOutcome::Pending`] when the outcome is indeterminate and
    /// must be resolved by reconciliation. (EVM will always return `Pending`
    /// until its confirmation-depth policy is met.)
    pub async fn broadcast(&self, prepared: &Prepared, payment: &VerifiedPayment) -> BroadcastOutcome {
        match self {
            Self::Near(provider) => {
                let PreparedDetail::Near(near_prepared) = &prepared.detail;
                match provider
                    .broadcast_exact(near_prepared.signed_transaction_bytes())
                    .await
                {
                    Ok(TransactionLookup::Final(outcome)) => {
                        let VerifiedDetail::Near(near_payment) = &payment.detail;
                        interpret_near_terminal(
                            &outcome,
                            near_prepared.transaction_hash,
                            &provider.relayer_account_id(),
                            &near_payment.payer,
                            &near_payment.requirements.asset,
                        )
                    }
                    Err(NearRpcError::TransactionRejected) => {
                        BroadcastOutcome::Rejected("transaction_rejected".to_owned())
                    }
                    Ok(TransactionLookup::Pending(_) | TransactionLookup::Unknown) | Err(_) => {
                        BroadcastOutcome::Pending
                    }
                }
            }
        }
    }
}

/// The exact-scheme requirements a payment was verified against, in neutral
/// (string / atomic-unit) form for logging and cross-checks.
#[derive(Clone, Debug)]
pub struct Requirements {
    /// CAIP-2 network id (e.g. `near:mainnet`, `eip155:8453`).
    pub network: String,
    /// Asset identifier: NEAR account id or EVM `0x` token address.
    pub asset: String,
    /// Recipient: NEAR account id or EVM `0x` address.
    pub pay_to: String,
    /// Amount in the asset's atomic units.
    pub amount: u128,
    /// Amount as the canonical decimal string advertised in requirements.
    pub amount_decimal: String,
}

/// A verified payment ready to be prepared and submitted. Neutral fields drive
/// the engine; `detail` carries the chain-specific verified state the same
/// provider consumes in [`ChainProvider::prepare`].
#[derive(Clone)]
pub struct VerifiedPayment {
    /// Payer identity: NEAR account id or EVM `0x` address.
    pub payer: String,
    /// Canonical per-chain payload hash (idempotency + integrity anchor).
    pub payment_hash: [u8; 32],
    /// The requirements this payment satisfies.
    pub requirements: Requirements,
    /// Chain-specific verified state.
    pub detail: VerifiedDetail,
}

impl fmt::Debug for VerifiedPayment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPayment")
            .field("payer", &self.payer)
            .field("payment_hash", &"<redacted>")
            .field("requirements", &self.requirements)
            .field("detail", &self.detail)
            .finish()
    }
}

/// Chain-specific verified-payment state.
#[derive(Clone, Debug)]
pub enum VerifiedDetail {
    /// NEAR: the decoded, signature-checked delegate payment.
    Near(NearVerified),
    // Evm(...) added in Phase 1.
}

/// A snapshot of the facilitator's signer/relayer and chain head, used to
/// prepare a submission and to gate readiness.
#[derive(Clone, Debug)]
pub struct SignerHead {
    /// NEAR block height / EVM block number at snapshot time.
    pub chain_block_height: u64,
    /// NEAR block hash / EVM unused (empty).
    pub chain_block_ref: String,
    /// NEAR access-key nonce / EVM account (transaction) nonce.
    pub signer_nonce: u128,
    /// Signer identity: NEAR relayer account id / EVM signer `0x` address.
    pub signer_id: String,
    /// Signer public key (NEAR ed25519) / empty for EVM (address is `signer_id`).
    pub signer_public_key: String,
    /// Signer's native-gas balance in atomic units (yoctoNEAR / wei).
    pub signer_balance_atomic: u128,
}

/// A prepared, signed submission. `submit_bytes`/`submit_hash` are durable and
/// must never be re-signed once journaled; recovery rebroadcasts these exact
/// bytes.
#[derive(Clone)]
pub struct Prepared {
    /// Signed submission bytes: Borsh `SignedTransaction` (NEAR) / RLP tx (EVM).
    pub submit_bytes: Vec<u8>,
    /// Submission hash: NEAR `CryptoHash` string / EVM `0x` tx hash.
    pub submit_hash: String,
    /// Signer identity used for the submission.
    pub signer_id: String,
    /// Signer public key (NEAR ed25519) / empty for EVM (address is `signer_id`).
    pub signer_public_key: String,
    /// Signer nonce burned by this submission.
    pub signer_nonce: u128,
    /// Chain-specific durable extras.
    pub detail: PreparedDetail,
}

impl fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Prepared")
            .field("submit_bytes", &"<redacted>")
            .field("submit_hash", &self.submit_hash)
            .field("signer_id", &self.signer_id)
            .field("signer_public_key", &self.signer_public_key)
            .field("signer_nonce", &self.signer_nonce)
            .field("detail", &self.detail)
            .finish()
    }
}

/// Chain-specific prepared-transaction state.
#[derive(Clone, Debug)]
pub enum PreparedDetail {
    /// NEAR: the prepared outer meta-transaction.
    Near(NearPrepared),
    // Evm(...) added in Phase 1.
}

/// The result of broadcasting a prepared submission.
#[derive(Debug)]
pub enum BroadcastOutcome {
    /// The chain reached a terminal outcome in one shot (NEAR fast finality).
    Terminal(TerminalOutcome),
    /// Deterministic on-chain or relayer rejection (never retried).
    Rejected(String),
    /// Submitted but not yet terminal — recovery/confirmation resolves it.
    /// EVM always lands here until the confirmation-depth policy is met.
    Pending,
}

/// The lifecycle position of a submitted transaction, observed during
/// reconciliation.
#[derive(Debug)]
pub enum StatusState {
    /// The chain has no record of the submission.
    Unknown,
    /// Seen but not yet mined/final.
    Pending,
    /// Mined with a given confirmation depth (EVM), below the required depth.
    Mined {
        /// Confirmations observed so far.
        confirmations: u64,
        /// Block number the transaction was mined in.
        block_number: u64,
    },
    /// Terminal and trusted (NEAR final, or EVM at/after required confirmations).
    Final,
}

/// The outcome of a reconciliation status query.
#[derive(Debug)]
pub struct StatusOutcome {
    /// Where the submission sits in its lifecycle.
    pub state: StatusState,
    /// Present when `state` is terminal.
    pub terminal: Option<TerminalOutcome>,
}

/// A terminal settlement outcome with the cost and evidence needed for the
/// journal. The provider validates the chain-specific receipt/log locus before
/// constructing this, so the engine consumes only neutral fields.
#[derive(Clone, Debug)]
pub struct TerminalOutcome {
    /// Whether the transfer succeeded (inner receipt / log success).
    pub success: bool,
    /// The settled transaction hash.
    pub tx_hash: String,
    /// Recipient balance delta in atomic units, when observable (NEAR: `None`).
    pub recipient_delta_atomic: Option<u128>,
    /// Native-gas fee actually spent by the facilitator, in atomic units
    /// (yoctoNEAR / wei).
    pub fee_atomic: u128,
    /// Gas units consumed (NEAR gas / EVM gas used), for the cost metric.
    pub gas_units: u64,
    /// Present iff `!success`: the authoritative on-chain failure reason.
    pub failure_detail: Option<String>,
}

/// A neutral verification rejection: the machine reason plus the flag the engine
/// needs to choose an HTTP disposition, without exposing the per-chain failure
/// enum.
#[derive(Clone, Copy, Debug)]
pub struct VerifyRejection {
    /// Stable machine reason (e.g. `insufficient_funds`).
    pub reason: &'static str,
    /// Whether the failure reflects an unavailable/ambiguous RPC lookup rather
    /// than a definitive invalid payment (engine returns 503, not a rejection).
    pub rpc_ambiguous: bool,
}

impl VerifyRejection {
    fn from_near(failure: NearVerificationFailure) -> Self {
        Self {
            reason: failure.reason(),
            rpc_ambiguous: near_verification_is_rpc_ambiguous(failure),
        }
    }
}

/// NEAR verification failures that reflect an unavailable/ambiguous RPC lookup
/// rather than a definitive invalid payment.
const fn near_verification_is_rpc_ambiguous(failure: NearVerificationFailure) -> bool {
    matches!(
        failure,
        NearVerificationFailure::CurrentBlockHeightUnavailable
            | NearVerificationFailure::AccessKeyLookupFailed
            | NearVerificationFailure::AccountLookupFailed
            | NearVerificationFailure::TokenAccountLookupFailed
            | NearVerificationFailure::BalanceCheckFailed
            | NearVerificationFailure::StorageCheckFailed
    )
}

/// Why a submission could not be prepared from a verified payment + signer head.
#[derive(Debug)]
pub enum PrepareError {
    /// The neutral signer head did not carry a valid chain reference or nonce.
    InvalidSignerHead,
    /// The chain provider failed to build or sign the submission.
    Provider(NearRpcError),
}

/// Bind a NEAR final outcome to the prepared transaction and interpret its
/// receipt graph into a neutral terminal outcome. Identity mismatch or a
/// non-authoritative (indeterminate) receipt state yields
/// [`BroadcastOutcome::Pending`] so the submission stays for reconciliation;
/// only an authoritative success or definitive failure is terminal.
fn interpret_near_terminal(
    outcome: &FinalExecutionOutcomeView,
    transaction_hash: CryptoHash,
    signer: &AccountId,
    payer: &AccountId,
    asset: &AccountId,
) -> BroadcastOutcome {
    if validate_final_outcome_identity(outcome, transaction_hash, signer, payer).is_err() {
        return BroadcastOutcome::Pending;
    }
    let (gas_units, fee_atomic) = execution_cost_near(outcome);
    match interpret_final_outcome(outcome, payer, asset) {
        Ok(_) => BroadcastOutcome::Terminal(TerminalOutcome {
            success: true,
            tx_hash: transaction_hash.to_string(),
            recipient_delta_atomic: None,
            fee_atomic,
            gas_units,
            failure_detail: None,
        }),
        Err(error) if error.is_definitive_failure() => {
            BroadcastOutcome::Terminal(TerminalOutcome {
                success: false,
                tx_hash: transaction_hash.to_string(),
                recipient_delta_atomic: None,
                fee_atomic,
                gas_units,
                failure_detail: Some(error.to_string()),
            })
        }
        Err(_) => BroadcastOutcome::Pending,
    }
}

/// Sum gas and tokens burnt across the transaction and its receipts. (Mirrors
/// the reconcile path's `execution_cost` in `service`; that copy is removed when
/// reconcile moves behind this provider.)
fn execution_cost_near(outcome: &FinalExecutionOutcomeView) -> (u64, u128) {
    let mut gas = outcome.transaction_outcome.outcome.gas_burnt.as_gas();
    let mut tokens = outcome
        .transaction_outcome
        .outcome
        .tokens_burnt
        .as_yoctonear();
    for receipt in &outcome.receipts_outcome {
        gas = gas.saturating_add(receipt.outcome.gas_burnt.as_gas());
        tokens = tokens.saturating_add(receipt.outcome.tokens_burnt.as_yoctonear());
    }
    (gas, tokens)
}
