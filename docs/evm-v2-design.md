# EVM v2 engineering design — the provider seam

Executable design for Phase 0 of the EVM-support plan (see the approved plan
and `docs/near-intents-adoption-gates.md` for scope). This turns "introduce a
provider seam and route EVM through the hardened journal" into concrete types
and a safe increment sequence. **Phase 0 changes NEAR behavior in no way; it is
a pure refactor plus additive schema, shipped as v0.2.0 and drilled before any
EVM code (Phase 1) lands.**

## Why an enum, not `dyn`

The settlement engine (`service.rs` `run_new_settlement`, `finalize_outcome`,
`reconcile_prepared`, `validate_stored_transaction`) currently calls inherent
methods on a concrete `Arc<NearChainProvider>` and threads NEAR primitives
(`AccountId`, `PublicKey`, `CryptoHash`, `SignedDelegateAction`,
`FinalExecutionOutcomeView`, `TransactionLookup`). To make EVM ride the same
engine, the engine must speak **neutral value types**. A single erased provider
would need those types to be concrete (object safety rules out associated
types), and the chain set is closed (NEAR, eip155). Upstream x402-rs uses the
same shape (a `ChainProvider` enum in its `facilitator/chain.rs`). So:

```rust
// crates/x402-near-facilitator/src/chain.rs (new)
pub enum ChainProvider {
    Near(x402_chain_near::NearChainProvider),
    // Evm(x402_chain_eip155_provider::Eip155Provider)  // added in Phase 1
}
```

`AppState.provider: Arc<ChainProvider>`. Every engine call goes through inherent
`ChainProvider` methods that return the neutral types below and `match` inward.

## Neutral value types (engine boundary)

Replacing the NEAR-specific returns. Fields chosen to satisfy both the journal
(`PreparedJournalEntry`, `SettlementRecord`) and the reconcile logic.

```rust
pub struct VerifiedPayment {
    pub payer: String,                 // NEAR account id / EVM 0x from
    pub payment_hash: [u8; 32],        // canonical per-chain payload hash
    pub requirements: VerifiedRequirements, // asset, pay_to, amount, network (neutral strings + u128)
    pub detail: VerifiedDetail,        // enum { Near(near::VerifiedPayment), Evm(evm::Verified) }
}

pub struct SignerHead {
    pub chain_block_height: u64,       // NEAR block height / EVM block number
    pub chain_block_ref: String,       // NEAR block hash / EVM "" (unused)
    pub signer_nonce: u128,            // NEAR access-key nonce / EVM account nonce
    pub signer_id: String,             // relayer account / EVM signer 0x
    pub signer_balance_atomic: u128,   // yoctoNEAR / wei (gas balance)
}

pub struct Prepared {
    pub submit_bytes: Vec<u8>,         // Borsh SignedTransaction / RLP signed tx
    pub submit_hash: String,           // NEAR CryptoHash / EVM 0x tx hash
    pub signer_id: String,
    pub signer_public_key: String,     // NEAR ed25519 pubkey / EVM "" (address is signer_id)
    pub signer_nonce: u128,
    pub detail: PreparedDetail,        // chain-specific durable extras
}

pub enum BroadcastOutcome {
    Terminal(TerminalOutcome),         // NEAR fast-finality success in one shot
    Rejected(String),                  // deterministic on-chain/relayer rejection
    Pending,                           // EVM always lands here → reconcile confirms
}

pub struct StatusOutcome {             // from query_status during reconcile
    pub state: StatusState,            // Unknown | Pending | Mined{confs,block} | Final
    pub terminal: Option<TerminalOutcome>,
}

pub struct TerminalOutcome {
    pub success: bool,
    pub tx_hash: String,
    pub recipient_delta_atomic: Option<u128>, // for evidence
    pub fee_atomic: u128,                      // gas_burnt/tokens_burnt (NEAR) / gas*price (EVM)
    pub evidence: TerminalEvidence,            // chain-specific receipt/log locus
}
```

The `*Detail` enums carry chain-specific state the same provider reconstructs
(e.g. NEAR's `SignedDelegateAction`, EVM's EIP-3009 authorization + `log_index`)
without leaking primitives to the engine.

## ChainProvider method surface (what the engine needs)

Derived from `run_new_settlement` (`service.rs:1144-1294`) and reconcile
(`1620-1868`):

- `verify(&VerifyRequest, &policy) -> Result<VerifiedPayment, VerifyFailure>`
  where `VerifyFailure` exposes `reason() -> &str`, `payer_attributable()`, and
  an `is_rpc_ambiguous()` classifier (today `verification_is_rpc_ambiguous`).
- `signer_head() -> Result<SignerHead, ChainError>` (replaces `relayer_status`
  + `RelayerHead`; carries balance for readiness).
- `prepare(&VerifiedPayment, &SignerHead) -> Result<Prepared, ChainError>`.
- `broadcast(&Prepared) -> Result<BroadcastOutcome, ChainError>`.
- `query_status(submit_hash, signer_id, signer_nonce, min_confs) -> Result<StatusOutcome, ChainError>`
  (primary) and a `_backup` variant (NEAR dual-RPC; EVM may reuse primary).
- `validate_stored_submit(bytes, expected_hash) -> Result<(), ChainError>`
  (the "exact bytes, deterministic hash" reconcile guard).
- `readiness_probe() -> RpcReadiness` (chain-id match + liveness; NEAR checks
  both RPCs, EVM checks `eth_chainId`/`eth_blockNumber`).
- `signer_addresses()`, `chain_id()` (already the generic `ChainProviderOps`).

The NEAR-specific **nonce-quarantine** (`service.rs:1222-1243`) and
**block-height-expiry** reconcile branches become methods that the NEAR impl
implements and the EVM impl no-ops (EVM exactly-once is the on-chain EIP-3009
nonce; reorg is handled by confirmation depth, below).

### As built (Phase 0, increments 1–2)

The neutral seam shipped as a `ChainProvider` enum (not `dyn`), and the engine in
`service.rs` now speaks only these methods — the `as_near()` bridge is gone from
the engine (it remains solely as a test accessor for staging journal fixtures):

- `verify(&VerifyRequest, &VerificationPolicy) -> Result<VerifiedPayment, VerifyRejection>`;
  `VerifyRejection { reason: &'static str, rpc_ambiguous: bool }`.
- `signer_head()` / `backup_signer_head() -> Result<SignerHead, NearRpcError>`
  (backup carries height + nonce only; balance is unused there).
- `prepare(&VerifiedPayment, &SignerHead) -> Result<Prepared, PrepareError>`.
- `broadcast(&Prepared, &VerifiedPayment) -> BroadcastOutcome`.
- `reconcile_status(submit_hash, signer, payer, asset) -> ReconcileStatus`
  and `rebroadcast(bytes, submit_hash, payer, asset) -> BroadcastOutcome`.
- `readiness_probe() -> bool`, `signer_account_id()`, `signer_public_key()`.

**Decision — dual-RPC + conflict live in the provider.** Rather than the engine
calling `query_status` twice and comparing, `reconcile_status` performs both RPC
queries, compares the two *raw* final outcomes for integrity, and interprets the
receipt graph, returning one neutral `ReconcileStatus { verdict, rpc_failover }`
(`verdict`: Terminal | Indeterminate | Pending | Unknown | Conflict | Ambiguous).
Rationale: NEAR's byte-for-byte raw-outcome equality is the integrity check we
must preserve exactly in Phase 0, and comparing *interpreted* neutral outcomes
would subtly weaken it; EVM's cross-check is confirmation-depth, also provider
-specific. `validate_stored_transaction` stays in the engine for now (it is a
pure function over the record + bytes, not a bridge user); it moves behind the
provider when the EVM RLP/EIP-3009 validator lands in Phase 1.

The identity-mismatch and receipt-indeterminate tracing events collapse to a
single `*_indeterminate` event carrying the reason; the settlement stays
submitted and the outer reconcile loop recomputes readiness from the remaining
nonterminal set, so the final readiness state is unchanged.

## EVM settlement specifics (Phase 1)

- **verify**: reuse `x402-chain-eip155`'s `V2Eip155Exact` handler wholesale
  (EIP-712 domain, EIP-1271/6492, balance). It gives us verify; we own submit.
- **prepare/broadcast**: build + sign the `transferWithAuthorization` tx via
  `alloy` from our funded EOA at a pinned account nonce; `submit_bytes` = RLP,
  `submit_hash` = tx hash. `broadcast` submits and returns `Pending` (never
  Terminal — we do not trust 1 confirmation).
- **confirmation depth / reorg**: reconcile calls `query_status`; a terminal
  `succeeded` is written **only at ≥ N confirmations** (config `confirmations`,
  Base default e.g. 5). `Mined{confs<N}` stays `submitted`. A tx that was
  `Mined` then returns `Unknown` (reorged out) stays `submitted` and is
  re-broadcast-safe only via the **same** signed bytes (nonce unchanged) — never
  re-signed. Revert → `Rejected`.
- **exactly-once**: the EIP-3009 authorization nonce is single-use on-chain, so
  a re-submit of the same bytes is idempotent at the token contract.

## Journal / migration 0002

One superset schema (one binary → one migration checksum across all instances;
each instance uses its chain's subset). Additive, nullable:

- `settlements`: add `evm_authorization JSONB`, `signer_address TEXT`,
  `signer_account_nonce NUMERIC(20,0)`, `submitted_tx_rlp BYTEA`,
  `submitted_tx_hash TEXT`, `mined_block_number NUMERIC(20,0)`,
  `mined_block_hash TEXT`, `confirmations INTEGER`, `required_confirmations INTEGER`.
- Relax the non-terminal CHECK (`0001` lines 124-133) to be **chain-conditional**:
  NEAR rows require the `delegate_*`/`relayer_*`/`outer_transaction_*` set; EVM
  rows require `signer_address`/`signer_account_nonce`/`submitted_tx_*`.
- Widen `api_clients.environment` CHECK beyond `('mainnet','testnet')`.
- Keep `*_yocto_near` budget column names (documented as chain-native atomic
  units; `NUMERIC(40,0)` already fits wei). Rename deferred.
- Migration applied only by `x402-near-admin migrate` (checksum-gated; the
  service refuses to start on mismatch). Test against the restore drill.

## Config generalization

`ServiceConfig` gains `chain_kind: ChainKind` (`near`|`eip155`) and a
chain-specific block; `validate()` branches. `Environment` (today `{Mainnet,
Testnet}`) generalizes to a `(chain_kind, caip2_network)` identity used for the
api-key label, DB name, and readiness. Relayer-key parse is chain-conditional
(ed25519 vs secp256k1). `deny_unknown_fields` → the struct changes, not just JSON.

## Safe increment sequence (each: `cargo check` + relevant tests green)

1. Add `chain.rs` with neutral types + `ChainProvider::Near` wrapping
   `NearChainProvider` (conversions NEAR→neutral). `#[allow(dead_code)]` until
   step 2 wires it (single transient allow, removed in step 2).
2. Flip `AppState.provider` to `Arc<ChainProvider>`; migrate the engine
   functions to neutral types **in one compilable move** (`run_new_settlement`,
   `finalize_outcome`, `reconcile_prepared`, `validate_stored_transaction`,
   `refresh_chain_readiness`, `fresh_relayer_status`). NEAR behavior identical.
3. Update the NEAR test doubles (`service_recovery_tests.rs` uses `MockRpc`;
   keep the NEAR provider path, adapt call sites to neutral types).
4. Config generalization + migration 0002 + widen 2-env scripts.
5. Full `cargo test` (needs Postgres for store/recovery/leadership suites) +
   CI; testnet drills; v0.2.0; mainnet promote with drilled rollback.

## Regression gate

The NEAR path must stay byte-identical. Gates: `crates/x402-chain-near`
unit/oracle tests, `service_recovery_tests.rs` (crash/recovery matrix),
`store_postgres_tests.rs` (idempotency/budget), `leadership_postgres_tests.rs`,
and the on-host testnet drills (promote/rollback/recovery) before any mainnet
promote. Rollback target stays v0.1.3.
