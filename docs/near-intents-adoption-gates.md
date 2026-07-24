# NEAR Intents adoption: decisions and engineering gates

Living record of whether and how the `intents` asset transfer method
(x402 `exact` on NEAR settled through the `intents.near` Verifier) is
adopted by this operation. The frozen background record with all
evidence is [near-intents-x402-progress.md](near-intents-x402-progress.md);
the launch boundaries in [AGENTS.md](../AGENTS.md) remain authoritative
for this service and are not changed by anything here.

## Decisions (2026-07-24)

1. **Container: sibling service.** This facilitator stays delegate-only.
   A separate, minimal, mainnet-only service will carry
   `assetTransferMethod: "intents"` once the gates below pass. Rationale:
   this service's trust story is that every mainnet code path was
   rehearsed identically on testnet first, and no testnet Verifier
   exists or is planned — carrying intents here would put a permanent
   asterisk on that claim. The sibling owns the mainnet-only caveat
   openly, and a Verifier fault is blast-radius-isolated from the
   delegate service. The sibling shares this repository's proven
   patterns (durable settlement journal, fail-closed verification,
   deploy/monitoring/backup templates) as extracted modules — never
   copy-paste, and never by relaxing this service's invariants
   (FunctionCall-key rejection, single-action `ft_transfer`, 1-yocto
   check).
2. **Upstream spec first.** The method is specified as an amendment to
   the merged `scheme_exact_near.md`, opened 2026-07-24 as
   [x402-foundation/x402#2948](https://github.com/x402-foundation/x402/pull/2948).
   No intents implementation is written anywhere until it merges — code
   builds against merged spec text, never a local branch. Complementary
   upstream context:
   [#2102](https://github.com/x402-foundation/x402/pull/2102) (NEAR
   Intents via 1Click, client-settled cross-chain; per its author now
   oriented toward the `upfront` scheme pending
   [#2520](https://github.com/x402-foundation/x402/pull/2520)) — cross-
   referenced from #2948 and in a comment on #2102 inviting NEAR-core
   review.
3. **`/supported` stays silent.** Neither facilitator advertises
   `assetTransferMethod` vocabulary until the spec is merged and an
   implementation exists. Advertising unfrozen vocabulary is a claim
   without evidence.

## Gates before any intents code ships (in order)

- [ ] **G1 — Spec frozen.** #2948 merged (or maintainer-redirected and
  the successor merged); discriminator name, payload shapes, and
  verification MUSTs are upstream-stable.
- [ ] **G2 — Fee semantics measured.** `state.fee` (pips) behavior
  measured across amounts and both delivery modes on dust settles.
  Until characterized, verification fails closed on any simulated
  delivery shortfall (spec rule 11).
- [ ] **G3 — Drill matrix designed and passed on mainnet dust.** The
  stand-in for the testnet rehearsal that cannot exist:
  - Storage-deregistration race: `payTo` storage-unregisters on the
    token contract between verify and settle; service must land in a
    terminal, operator-explainable state with the payer's Verifier
    balance accounted for.
  - `execute_intents` succeeds but the spawned `ft_withdraw` token
    receipt fails: measure whether the Verifier refunds the internal
    balance; settlement must report failure (wallet delivery is only
    successful on the token receipt's `SuccessValue`).
  - Indeterminate recovery: process killed after broadcast; reconcile
    via `is_nonce_used(signer, nonce)` — submitter-independent, so a
    third-party submission that consumed the nonce is recognized as the
    payment having happened. No rebroadcast on reconcile.
  - Replay: identical payload resubmitted → on-chain `nonce already
    used` rejection surfaces as a deterministic client error.
- [ ] **G4 — Sibling design doc.** Service boundary, shared-module
  extraction plan (journal, fail-closed verify helpers, deploy
  templates), key/custody model (facilitator submits `execute_intents`
  from its own funded account; it never holds payer funds), and
  monitoring/alerting parity with this service's standard.
- [ ] **G5 — Operational parity.** The sibling passes the same classes
  of drills this service passed before its go-live (promote, rollback,
  recovery, alerting delivery), adapted to mainnet-dust scale.

## Non-goals

- **`exact-agent`** (least-privilege payment agent): separate track,
  testnet-proven only; out of scope here.
- **`token_diff` / cross-asset**: excluded from `exact` semantics;
  belongs to the #2102 → `upfront` (#2520) direction upstream.
- **Testnet intents**: impossible — no testnet Verifier deployment
  exists; this is a hard constraint, not a deferral.
