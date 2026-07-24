# NEAR Intents sibling service — design sketch (DRAFT)

> **Status: draft, pending gate G1.** This sketches the mainnet-only
> sibling service that will carry the x402 `exact` `intents` asset transfer
> method (decision in
> [near-intents-adoption-gates.md](near-intents-adoption-gates.md)). It is
> written now, while upstream spec PR
> [x402-foundation/x402#2948](https://github.com/x402-foundation/x402/pull/2948)
> is under review, to make the engineering shape concrete. **Nothing here
> is built, and field names / binding rules are not frozen until #2948
> merges (G1).** Background and evidence:
> [near-intents-x402-progress.md](near-intents-x402-progress.md).

## Why a sibling, not a mode of this service

This facilitator's trust story is that **every mainnet code path was
rehearsed identically on testnet first** — the promote, rollback, and
indeterminate-recovery drills all ran on testnet before mainnet. The
`intents` method cannot honor that: there is no testnet Verifier
deployment and none is planned, so the method is `near:mainnet`-only.
Folding it into this service would put a permanent asterisk on the
"testnet-first, no exceptions" claim. A separate service owns the
mainnet-only caveat explicitly, and a Verifier fault (custody bug,
availability) is blast-radius-isolated from the delegate facilitator that
serves NEAR-native payers today. The delegate service's launch invariants
(FunctionCall-key rejection, single-action `ft_transfer`, 1-yocto rule)
are **not** relaxed.

## Service boundary

- **Serves**: x402 v2 `exact` on `near:mainnet` with
  `extra.assetTransferMethod: "intents"`, delivery `wallet` (default,
  `ft_withdraw` to `payTo`) and optionally `internal`. Signature standards
  `nep413` and `erc191` at minimum.
- **Does not serve**: `near:testnet` (no Verifier); the `delegate` method
  (that is this repository's service); `token_diff` / cross-asset (excluded
  from `exact`; belongs to the `upfront` / #2102 direction upstream);
  `exact-agent` (separate track).
- **Custody**: the service submits `execute_intents` from **its own funded
  NEAR account** and pays gas; it never holds payer funds. Payer funds live
  in the `intents.near` Verifier ledger (deposited out of band). This is the
  same "facilitator never custodies" posture as the delegate service, with
  a different settlement contract.

## Shared modules (extract, do not fork)

The sibling reuses this repository's hardened pieces as **extracted crates
/ templates**, never copy-paste:

- **Durable settlement journal + idempotency** (reserved→prepared→
  submitted→terminal, advisory-lock leadership, cache key = hash of the
  exact signed payload). The state machine is settlement-mechanism-agnostic;
  the cache key becomes the `signedIntent` bytes.
- **Fail-closed verification helpers** and typed RPC error handling (every
  undeterminable state → fail closed).
- **Deploy / hardening templates**: the systemd unit hardening (exposure
  1.5), `LoadCredential` mode-0600 credential handling, Nginx deny-by-default
  vhost, CloudWatch metrics + alarm + OnFailure alerting, nightly backup +
  restore drill. These are configuration lineages, parameterized per service.

## Verification pipeline (per #2948)

Replaces the delegate-shaped `verify_proto_request` wholesale:

1. Version / scheme / network / method discriminator match.
2. Standard allowlist; reject unsupported deterministically.
3. Per-standard signature recompute — NEP-413 prehash (`sha256(u32_le(2^31+413)
   || borsh{message, nonce, recipient, callback_url:None})`) for `nep413`;
   EIP-191 digest + `v`-normalized recovery for `erc191`.
4. Signer authorization via the **Verifier's** registry — `has_public_key`
   for named accounts, implicit derivation (ed25519→64-hex,
   secp256k1→implicit-Eth) otherwise — not NEAR access keys.
5. `is_nonce_used(signer, nonce)` false (on-chain single-use; no local nonce
   store needed).
6. Two-sided deadline bound with a stated clock-skew margin.
7. Intent binding: exactly one intent; for wallet delivery
   `ft_withdraw{token==asset, receiver_id==payTo, amount==amount}` exactly.
8. `mt_balance_of(signer, "nep141:<asset>")` ≥ amount.
9. `storage_balance_of(payTo)` non-null (wallet delivery).
10. `simulate_intents` preflight — which the deployed contract validates
    signatures for (stronger than documented), but MUST NOT replace step 3.
11. Fail-closed on any simulated delivery shortfall (fee in pips).

## Settlement and receipts

- Submit `execute_intents({signed:[signedIntent]})` from the service account;
  no attached deposit.
- Wallet delivery succeeds **only** when the spawned `ft_withdraw` token
  receipt is `SuccessValue` — outer `execute_intents` success is not
  sufficient. Internal delivery: the `execute_intents` outcome is
  authoritative.
- `payer` in the settlement response is the payload's `signer_id`.

## Reconciliation and recovery (the hard part — gate G3)

- **Authoritative, submitter-independent oracle**: because any holder of the
  signed bytes can submit `execute_intents`, reconciliation identity is
  **(signer, nonce consumed, intent effect)**, never the service's own tx
  hash. On an indeterminate submit, read `is_nonce_used(signer, nonce)` — if
  consumed, the payment happened; terminalize without rebroadcast. This is a
  cleaner primitive than the delegate path's tx-hash matching.
- **Novel failure mode to measure on dust before production**:
  `execute_intents` succeeds but the spawned `ft_withdraw` token receipt
  fails (e.g. `payTo` storage-deregistered between verify and settle) — the
  Verifier debits the internal ledger but tokens do not reach the wallet.
  Does the Verifier refund the internal balance? Settlement must report
  failure regardless. The `storage_balance_of` preflight covers the common
  cause; the verify→settle race is the drill.
- **Deadline**: once past `deadline`, no rebroadcast is possible; the failure
  window mirrors the delegate path's `max_block_height`.

## Operational posture

- **Mainnet-only, dust-scale canary drills** stand in for the testnet
  rehearsal that cannot exist: a recurring small-value settle + the G3 drill
  matrix, gated behind explicit funded-broadcast confirmation.
- **Blast-radius**: keep the service account's standing Verifier balance near
  a single payment; just-in-time deposits; per-client budget caps as in the
  delegate service.
- **Monitoring parity**: relayer/settlement metrics, cert + backup alarms,
  OnFailure alerting, dead-man readiness — same standard as the delegate
  service before its go-live.

## Dependencies / open questions

- **G1** freezes the discriminator, payload shapes, and binding rules; this
  doc finalizes against merged spec text, not the branch.
- **G2** measures fee (pips) semantics across amounts and delivery modes.
- Clock-skew margin value; `internal`-delivery policy (only for merchants who
  operate a Verifier balance); whether to charge a facilitator fee at all
  (the delegate service is currently free).
