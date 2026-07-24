# NEAR Intents × x402 — progress record and position (2026-07-24)

> This file is the frozen 2026-07-24 record. The **living** adoption
> decisions and engineering gates are in
> [near-intents-adoption-gates.md](near-intents-adoption-gates.md).

This document records, in one place, the NEAR Intents × x402 work completed on
2026-07-23 and what it means for this repository. It is a **progress record and
decision aid, not policy**: the launch boundaries in [AGENTS.md](../AGENTS.md)
remain authoritative for this service, and nothing below changes them. All
claims carry their evidence — transaction links, repository pins, or file:line
citations into this codebase.

Why it lives here: this repository is the production x402 facilitator for NEAR.
The work below defines a second settlement route for the same scheme this
service already serves, proves that route on mainnet with an independent
reference implementation, and leaves this repository with a concrete decision
about whether — and how — to carry it.

---

## 1. The landscape: three settlement shapes, all now grounded

x402 `exact` on NEAR now has three proven ways to move the same payment
(exact amount of a NEP-141 token to a fixed payee, gas sponsored):

| | `delegate` (this service) | `intents` (new) | `exact-agent` (adjacent) |
| :--- | :--- | :--- | :--- |
| Mechanism | NEP-366 `SignedDelegateAction` wrapping one `ft_transfer`; relayer submits | NEAR Intents Verifier (`intents.near`): payer signs an intent payload; anyone submits `execute_intents` | Agent contract holds the token and attaches the 1 yocto itself; a `pay()`-scoped function-call key authorizes |
| Payer needs | A NEAR account holding the token + a **full-access** key | A balance inside the Verifier + any of 7 signature standards — an `erc191` EVM key pays with **no NEAR account at all** | No full-access key anywhere: a method-scoped fc-key on the agent |
| Custody before settlement | Payer's own account | Shared Verifier ledger (blast radius = deposited balance) | The agent contract's balance, policy-capped |
| Status | **Mainnet-live 2026-07-23** (`evidence/2026-07-23-mainnet-golive.md`) | Spec amendment drafted + **$0.10 settled on mainnet** through a reference builder (§3) | Proven on testnet: direct, relayed, and against real tokens incl. Circle test USDC (§4) |
| Networks | `near:testnet` + `near:mainnet` | `near:mainnet` only — **no testnet Verifier exists** | testnet (mainnet unexercised) |
| Circle Developer-Controlled Wallets | NEAR wallet (`signDelegateAction`, NEP-461) | EVM wallet (`signMessage` emits exact `erc191` bytes — the live-proven route) | Any key holder; Circle NEAR wallet works as the scoped signer |

The three are complements, not competitors: `delegate` is the
no-custody-transfer route for NEAR-native payers; `intents` is the
no-NEAR-account route for everyone else; `exact-agent` is the least-privilege
route for custodians who refuse to hold full-access keys.

## 2. The constraint that shaped all of this

NEP-141 `ft_transfer` requires exactly 1 yoctoNEAR attached; NEAR
function-call access keys cannot attach any deposit. Every account-based payer
is therefore forced to a full-access key — which is precisely what this
service's verifier enforces and documents
([AGENTS.md:29-31](../AGENTS.md); FunctionCall-key rejection and the 1-yocto
check in `verify_proto_request`,
[crates/x402-chain-near/src/mechanism.rs:362](../crates/x402-chain-near/src/mechanism.rs),
[:381](../crates/x402-chain-near/src/mechanism.rs)). The `intents` and
`exact-agent` shapes are the two structural answers: move the authorization
into a contract's signature check (`intents.near`), or move the token into a
contract that attaches the yocto itself (the payment agent).

## 3. The `intents` asset transfer method

### 3.1 What was specified

An amendment to the **merged** upstream NEAR scheme spec
(`specs/schemes/exact/scheme_exact_near.md` in the x402-foundation repository)
adds a second asset transfer method under the same scheme and network,
selected by `extra.assetTransferMethod: "intents"` — the exact discriminator
pattern `scheme_exact_evm.md` already uses for eip3009/permit2/erc7710, and
what the fork's own authoring guide mandates when a scheme offers multiple
payload formats. No new scheme name, no second mechanism package (the upstream
TS registry resolves one mechanism per (scheme, network) pair; a second
package claiming `("exact", "near:mainnet")` cannot coexist).

- Where: branch `feat/near-exact-intents` @ `29622f58`, +131 lines, branched
  off x402-foundation `main` (fetched 2026-07-23, `61349dea`), in the worktree
  `~/other/x402-wt-near-exact-intents`. **Upstream PR not yet opened.**
- Delivery binding: `extra.delivery: "wallet"` (default) requires the single
  intent to be `ft_withdraw{token==asset, receiver_id==payTo, amount==amount}`
  — real tokens leave the Verifier for the merchant's wallet, the semantic
  equivalent of this service's inner `ft_transfer`. `"internal"` (server
  opt-in only) is a Verifier-ledger `transfer` credit. `token_diff` is
  excluded: quote-dependent fill amounts break `exact` semantics.
- Deadline: `deadline = signing_time + maxTimeoutSeconds` (ISO-8601), with the
  same two-sided facilitator check this service applies to
  `max_block_height`.
- Payload: `{"signedIntent": <Verifier multi-standard payload verbatim>}`.
  MUST-support standards: `nep413` and `erc191`.

### 3.2 The two payload shapes, pinned empirically

**`nep413` (envelope form).** `nonce` (base64, 32 bytes) and `recipient`
(`"intents.near"`) live in the envelope; the signed message is the **minimal**
JSON string. This exact message settled on mainnet (§3.4):

```json
{"signer_id":"ea4854a9a1151a602bc21bd61c068a748cfc4556e735ad97647b00c631b7a1e6",
 "deadline":"2026-07-24T01:34:18.388Z",
 "intents":[{"intent":"ft_withdraw",
   "token":"17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1",
   "receiver_id":"count.mike.near","amount":"100000"}]}
```

Signature: ed25519 over `sha256(u32_le(2^31+413) || borsh{message, nonce,
recipient, callback_url: None})`.

**`erc191` (flat form).** No envelope — `verifying_contract` and `nonce` sit
**inside** the signed JSON string; the payer key is recovered from the 65-byte
secp256k1 signature (`v` 27/28 normalized to 0/1). This is the 347-byte
payload the deployed Verifier accepted from a Circle EVM wallet on 2026-07-16
([`ALqQDmeT…`](https://nearblocks.io/txns/ALqQDmeTUouVv6vqG8bqbwBo7RzxBuB7gufPwtcLyw6E)),
mined verbatim from archival RPC as the reference vector:

```json
{"signer_id":"0x020c42fb71eb59f06d3a36e10b52072c673d8abf",
 "verifying_contract":"intents.near",
 "deadline":"2026-07-17T00:51:55.236Z",
 "nonce":"Q7eU5DwdaYk8WwX8s8ehpUKj0vFzkPrwNLyPGZgOFss=",
 "intents":[{"intent":"ft_withdraw",
   "token":"17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1",
   "receiver_id":"count.mike.near","amount":"1000"}]}
```

The split resolves what looked like a documentation contradiction: the
Intents docs' "expanded message" example (with `verifying_contract` and an
embedded nonce) is the erc191-family shape; nep413 accepts the minimal
message. Both are now pinned against the deployed contract, not the docs.

### 3.3 Empirical findings the docs do not tell you

Found while grounding the spec against deployed `intents.near`
(defuse v0.4.2), all reproducible from `~/other/x402-near-intents`:

1. **`simulate_intents` validates signatures.** The docs say simulation "does
   not require signatures to be validated"; the deployed contract panics
   `invalid signature` on a garbage signature. Consequence: a facilitator's
   simulation preflight is stronger than documented, and a *successfully
   simulated* payload has already passed deserialization → signature → signer
   authorization.
2. **Verifier view surface confirmed live:** `is_nonce_used(account_id,
   nonce)` (the live tx's consumed nonce reads `true`, fresh nonces `false`),
   `has_public_key(account_id, public_key)`, `mt_balance_of(account_id,
   "nep141:<token>")`. Named accounts must `add_public_key` first; implicit
   signers (ed25519 → 64-hex, secp256k1/erc191 → implicit-Eth) need no
   registration — the Verifier's own key registry, not NEAR access keys, is
   the authority.
3. **Fee: `simulate_intents` reports `state.fee = 1` (pips), and delivery was
   still exact** — the merchant received +100000 of a 100000 `ft_withdraw`.
   The spec text still requires fail-closed verification of simulated
   delivery equality rather than assuming fee exemption.
4. **Deadlines are ISO-8601 with milliseconds** on the accepted payloads;
   `raw_ed25519` (unused here) is the one standard with a unix-timestamp
   deadline per docs.
5. **No testnet Verifier exists.** The docs are explicit. Everything short of
   funds must ground on `simulate_intents` + mined vectors; the only
   settlement target is mainnet.

### 3.4 The verification ladder, and what each rung proved

Reference implementation: `~/other/x402-near-intents` @ `d1ad52f`
(dependency-light TypeScript: hand-encoded NEP-413 borsh, `@noble` crypto,
plain JSON-RPC — deliberately no near-api-js).

| Rung | Evidence | Proves |
| :--- | :--- | :--- |
| Byte vectors (offline) | 8 vitest tests; the builder **rebuilds the accepted 347-byte erc191 payload byte-for-byte** and recovers its signer address from the chain-accepted signature | Our EIP-191 digest, address recovery, and message construction match what the deployed contract accepted |
| `simulate_intents` (read-only mainnet) | A fresh throwaway-key nep413 payload passed signature validation and failed only at `insufficient balance or overflow` | Our NEP-413 prehash/borsh/signing bytes are correct per the deployed contract; minimal message accepted; implicit-signer auth works — all without funds |
| View preflights (read-only) | `is_nonce_used` true/false on consumed/fresh nonces; `has_public_key`; `mt_balance_of` | The verify-side preflight suite a facilitator needs exists and behaves as specified |
| **Funded settle (mainnet, once, explicitly confirmed)** | Deposit [`3A7sfuWy…`](https://www.nearblocks.io/txns/3A7sfuWyNbEy3MRT3knE6FEGUsj9rziYcjvwNBAhGNqh): `mike.near` credited $0.10 USDC to throwaway implicit signer `ea4854a9…` (`mt_mint`). Settle [`2R2ANbXf…`](https://www.nearblocks.io/txns/2R2ANbXfhhTC1XaVfYLWqu3but8FKdsimAEEex1tK1dp): `bob.mike.near` — an unrelated account — submitted `execute_intents`; the Verifier verified the hand-built nep413 signature, executed `ft_withdraw`, and the USDC contract emitted a real `ft_transfer`; `count.mike.near` 335001 → 435001, **+100000 exactly** | End-to-end settlement of the new method through independently written bytes; the submitter needs no relationship to the payer (the signature alone carries authority) — the facilitator role in one transaction |
| Replay (mainnet) | Resubmitting the identical payload: rejected, `nonce was already used` | On-chain, per-signer single-use nonces; a facilitator needs **no nonce storage** |

The run followed this repository's own funded-broadcast rule
([AGENTS.md, Network and funds safety](../AGENTS.md)): explicit human
confirmation of network, asset, amount, payer, recipient, and refund address
before broadcast. The throwaway signer's seed lives in
`~/.near-credentials/mainnet/`, outside every repository.

## 4. The `exact-agent` shape, and why this service structurally rejects it

The least-privilege payment agent
([github.com/mikedotexe/near-payment-agent](https://github.com/mikedotexe/near-payment-agent),
`main` @ `666d5ef`) holds the NEP-141 and attaches the 1 yocto itself, so a
custodian authorizes `pay(recipient, amount, payment_id)` with a
function-call key scoped to that one method. On testnet it settled **direct**
([`A3C234V1…`](https://testnet.nearblocks.io/txns/A3C234V1yBV2hAYmDrcH9gkKqEtBN6wLeD1ukXLedejV)),
**relayed** via NEP-366 meta-tx
([`C5KaUwv8…`](https://testnet.nearblocks.io/txns/C5KaUwv8E7tuPegHZQFPLpQsU5WHnuJvhG27cyFgZadb)),
and — closing the mock-only caveat — against **real tokens**: Circle test USDC
direct ([`2BaCYpkz…`](https://testnet.nearblocks.io/txns/2BaCYpkzcmmTwc4jDL9yXveL9FrMnpuBQjF1A36JmBCC))
and relayed ([`1E1QCSnx…`](https://testnet.nearblocks.io/txns/1E1QCSnxc7kW8dFVejUXZuyvCoeQ1oCr9riVL2kVD1p)),
plus wNEAR ([`9Wq5AEum…`](https://testnet.nearblocks.io/txns/9Wq5AEumEi2N4giYKxR13RriPBzTLt4Y9hq6Hd7x9MF6)).
The same key was rejected on-chain for everything else (`MethodNameMismatch`,
`ak_receiver`≠`tx_receiver`, `DepositWithFunctionCall`).

This facilitator **cannot and should not** serve that shape under its launch
boundaries: it rejects FunctionCall payer permissions and requires the 1-yocto
deposit by documented invariant ([AGENTS.md:29-31](../AGENTS.md);
[mechanism.rs:362](../crates/x402-chain-near/src/mechanism.rs),
[:381](../crates/x402-chain-near/src/mechanism.rs)). That is by design — the
relayed demo used a minimal standalone relay instead, and any first-class
`@x402/near-agent` scheme remains a separate, deferred track.

## 5. What serving `intents` in this repository would take

Assessed against the code as of `main` @ `32f815a`. The settlement seam that
looks made for this — `NearSettlementCoordinator` + `with_settlement_coordinator`
([crates/x402-chain-near/src/provider.rs:72-131](../crates/x402-chain-near/src/provider.rs))
— is **unused in production**: no caller in the service crate; the HTTP
`/settle` path drives verify → `prepare_outer_transaction`
([provider.rs:223](../crates/x402-chain-near/src/provider.rs)) →
`broadcast_exact` ([provider.rs:259](../crates/x402-chain-near/src/provider.rs))
directly. An `intents` backend is therefore not a drop-in coordinator; it
touches every layer:

- **Verification** — `verify_proto_request` is delegate-shaped end to end
  (decode borsh `SignedDelegateAction`, single-action/`ft_transfer`/1-yocto,
  access-key nonce window). The intents path replaces it wholesale: standard
  allowlist, per-standard signature recomputation (NEP-413 prehash /
  EIP-191 digest), Verifier-registry auth (`has_public_key` or implicit
  derivation), `is_nonce_used`, two-sided ISO deadline, intent-binding
  equality, `mt_balance_of` ≥ amount, `storage_balance_of(payTo)` for wallet
  delivery, and a `simulate_intents` preflight (which, per §3.3, is stronger
  than documented — but must not replace signature verification).
- **Types** — `VerifiedPayment` holds a decoded delegate; an enum or parallel
  type is needed.
- **Submission** — no delegate, no outer `Action::Delegate`: the facilitator's
  own account signs a plain `FunctionCall` to `intents.near::execute_intents`.
  The relayer-nonce serialization machinery still applies (it is the
  facilitator's account nonce now); the relayer≠payer rule becomes moot
  (the payer never appears as a NEAR transaction signer).
- **Receipt validation** — `interpret_final_outcome`
  ([receipt.rs:109](../crates/x402-chain-near/src/receipt.rs)) enforces
  tx → delegate receipt → token receipt. The intents tree is
  tx → `execute_intents` receipt → (for wallet delivery) a spawned token
  `ft_transfer` receipt that must end `SuccessValue`; for `internal`
  delivery the `execute_intents` outcome itself is authoritative.
- **Idempotency/journal** — the durable-settlement journal pattern carries
  over with the cache key = hash of the exact `signedIntent` bytes; the
  Verifier's on-chain nonce independently guarantees at-most-once execution.
- **Config/policy** — the launch gates pin the asset to the canonical Circle
  USDC per environment ([config.rs:174-176](../crates/x402-near-facilitator/src/config.rs)),
  which matches; but `near:testnet` support is impossible for this method
  (no Verifier), payer policy rows would key on Verifier signer ids
  (including `0x…` implicit-Eth), and the deposit precondition changes what
  "payer balance" means (`mt_balance_of`, not `ft_balance_of`).

**Position (recommendation, not policy):** do not relax this service's launch
invariants in place. The two honest paths are (a) a deliberate v-next that
adds `assetTransferMethod: "intents"` behind its own policy review, using the
now-proven reference builder as the conformance oracle, advertised via
`/supported` `kinds[].extra`; or (b) a minimal sibling service that shares the
journal/fail-closed patterns but none of the delegate code. Either way, the
spec amendment and the reference vectors mean the protocol surface is no
longer the unknown — the remaining work is this repository's own engineering
standards.

## 6. Artifact and evidence map

| Artifact | Where | State (2026-07-24) |
| :--- | :--- | :--- |
| Spec amendment (`intents` method) | `~/other/x402-wt-near-exact-intents` (worktree of the x402 fork) | `feat/near-exact-intents` @ `29622f58`, clean; off x402-foundation `main` `61349dea`; **PR not opened** |
| Reference builder + vectors + gated settle | `~/other/x402-near-intents` | `main` @ `d1ad52f`, clean; no remote |
| Least-privilege payment agent | `~/other/near-payment-agent` · [GitHub](https://github.com/mikedotexe/near-payment-agent) | `main` @ `666d5ef`, pushed |
| Live erc191 oracle | mainnet | [`ALqQDmeT…`](https://nearblocks.io/txns/ALqQDmeTUouVv6vqG8bqbwBo7RzxBuB7gufPwtcLyw6E) (2026-07-16) |
| `intents` settle + replay proof | mainnet | [`3A7sfuWy…`](https://www.nearblocks.io/txns/3A7sfuWyNbEy3MRT3knE6FEGUsj9rziYcjvwNBAhGNqh) · [`2R2ANbXf…`](https://www.nearblocks.io/txns/2R2ANbXfhhTC1XaVfYLWqu3but8FKdsimAEEex1tK1dp) (2026-07-23) |
| Program-level record | `~/near/fn/near-integrations` (hub), Program 04 | recorded through `1c14eec`; `check.py` 15 pins green |

Adjacent but out of scope here: the speculative CCTP V2 track
(`~/other/near-cctp`: design spec + compiling skeletons + executable
7-scenario fixture harness; activation Circle-gated).

## 7. Honest scope — what is NOT yet true

- One $0.10 settle is not volume: no soak, no concurrency, no failure-mode
  matrix (RPC flap mid-`execute_intents`, deadline expiry races, partial
  `ft_withdraw` failure refunds) has been exercised for the intents route.
- The settle ran through the reference builder, **not through this service's
  code path**. Nothing in `x402-chain-near` or `x402-near-facilitator`
  implements the method today.
- Fee behavior was observed at one amount for `ft_withdraw` (`fee = 1` pip
  reported, delivery exact). The spec requires fail-closed verification, not
  an assumption of exemption; broader amounts/intent types are unmeasured.
- The spec amendment is a local branch. Upstream review may change field
  names, binding rules, or the discriminator itself; the amendment is
  grounded, not merged.
- `exact-agent` is testnet-proven only, and Circle NEAR DCWs still cannot
  sign NEP-413 — Intents-route Circle payers are EVM-keyed by construction.
- There is no testnet story for the intents method at all, and cannot be
  until a testnet Verifier exists.

## 8. Open decisions for this repository

All four were resolved on 2026-07-24; the living record is
[near-intents-adoption-gates.md](near-intents-adoption-gates.md).

1. Whether to carry `assetTransferMethod: "intents"` at all — and if so,
   v-next here vs sibling service (§5). — **Resolved: sibling service**;
   this facilitator's launch invariants and testnet-first drill story
   stay exception-free.
2. Timing of the upstream PR for the spec amendment (the artifact this
   service would be implementing against). — **Resolved: opened
   2026-07-24 as
   [x402-foundation/x402#2948](https://github.com/x402-foundation/x402/pull/2948)**.
3. Whether `/supported` should advertise transfer-method capability now
   (as `kinds[].extra`) even before any implementation, or stay silent.
   — **Resolved: stay silent** until the spec merges and an
   implementation exists; advertising unfrozen vocabulary would be a
   claim without evidence.
4. Housekeeping: the README status block still read "pre-launch" while
   `evidence/2026-07-23-*-golive.md` record both go-lives. — **Resolved
   in the same change that relocated this file** (README and SECURITY.md
   now state go-live status with evidence links).
