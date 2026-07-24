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

use x402_chain_near::{PreparedTransaction as NearPrepared, VerifiedPayment as NearVerified};

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

/// A terminal settlement outcome with the evidence needed for the journal.
#[derive(Debug)]
pub struct TerminalOutcome {
    /// Whether the transfer succeeded (inner receipt / log success).
    pub success: bool,
    /// The settled transaction hash.
    pub tx_hash: String,
    /// Recipient balance delta in atomic units, when observable.
    pub recipient_delta_atomic: Option<u128>,
    /// Fee/gas actually spent by the facilitator, in atomic units.
    pub fee_atomic: u128,
    /// Chain-specific receipt/log locus proving the transfer.
    pub evidence: TerminalEvidence,
}

/// Chain-specific terminal evidence.
#[derive(Debug)]
pub enum TerminalEvidence {
    /// NEAR: the final execution outcome (receipt graph validated by the
    /// provider before this is constructed).
    Near {
        /// Gas burnt across the transaction and its receipts.
        gas_burnt: u64,
        /// Tokens burnt (yoctoNEAR) across the transaction and its receipts.
        tokens_burnt: u128,
    },
    // Evm { log_index, mined_block_number, mined_block_hash } added in Phase 1.
}
