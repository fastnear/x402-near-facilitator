# Distribution: registry submissions (prepared, not sent)

Status: the three PR-based submissions are **open** (foundation #2941,
awesome-agentic-commerce #510, x402facilitators #15) and the x402scan
feature request is filed (#1040). Remaining operator actions: the NEAR
Catalog form and watching the open PRs for maintainer feedback. The NEAR
integrations coordination hub entry (launch-checklist gate 3) is parked
separately.

## Readiness facts registries key off

- `/supported` on both facilitator hostnames is canonical x402 v2:
  `kinds` (`x402Version: 2`, `exact`, `near:mainnet` / `near:testnet`),
  `extensions: ["payment-identifier"]`, and per-network `signers`.
- The public demo resource server returns a valid 402 with a base64
  `PAYMENT-REQUIRED` requirements header at
  `https://x402-demo.mikedotexe.com/work` (mainnet) and
  `https://x402-demo-test.mikedotexe.com/work` (testnet), and settles
  real payments end to end (see the real-traffic evidence entry).

## Targets

### 1. x402scan — resource registration (web form, no PR)

- Submit at <https://www.x402scan.com/resources/register>.
- URL to submit: `https://x402-demo.mikedotexe.com/work`.
- Per their discovery spec (`Merit-Systems/x402scan`
  `docs/DISCOVERY.md`, OpenAPI-first), both demo hostnames serve
  `/openapi.json` declaring the paid `POST /work` operation with
  `x-payment-info` (protocol `x402`, fixed `$0.001`), a `402` response,
  and the request-body input schema, plus `/.well-known/x402` for
  compatibility fan-out (sources in `deploy/demo/discovery/`). Runtime
  402 behavior remains authoritative and was validated by the live paid
  flows.
- Optionally also register the testnet resource URL if the form accepts
  non-mainnet resources (their spec notes testnets are not indexed).
- **Status 2026-07-23: blocked upstream.** The probe now parses our
  discovery document (title, paid operation, input schema all accepted)
  but rejects registration with `No supported networks. Got:
  [near:mainnet]. Supported: [base, solana]`. Their
  `apps/scan/src/types/chain.ts` `Chain` enum and payment tooling are
  EVM/Solana-only, so NEAR indexing is an upstream feature, not a config
  change. Both warnings from the probe (root `favicon.ico`,
  `info.contact.email`) are fixed on our side so registration is
  turnkey once they add NEAR.
- **Feature request filed 2026-07-23**:
  <https://github.com/Merit-Systems/x402scan/issues/1040> (Support NEAR
  `near:mainnet` resources). Registration is turnkey once it lands.

### 2. x402 Foundation repo — facilitators table (PR)

- Upstream: `x402-foundation/x402`, file `docs/dev-tools/facilitators.md`.
- Staged branch (based on clean upstream `main`):
  <https://github.com/mikedotexe/x402/tree/x402-near-facilitator-listing>
- **PR opened 2026-07-23:** <https://github.com/x402-foundation/x402/pull/2941>
- Entry added (alphabetical position):
  `| [NEAR x402 Facilitator](https://x402.mikedotexe.com/supported) |
  Independent facilitator for NEAR mainnet and testnet; NEP-141 USDC
  settled through NEP-366 signed delegates with relayer-sponsored gas |`

### 3. x402.org ecosystem page — partner entry (same branch)

- The x402.org site source lives in the same foundation repo
  (`typescript/site`); the **same staged branch** adds
  `app/ecosystem/partners-data/near-x402-facilitator/metadata.json`
  (category `Facilitators`, baseUrl `https://x402.mikedotexe.com`,
  networks near/near-testnet, scheme `exact`, assets NEP-141,
  supports verify/settle/supported, no `list` endpoint) plus an original
  minimal SVG logo at `public/logos/near-x402-facilitator.svg`.
- One PR to the foundation repo covers both this and the table above.

### 4. awesome-agentic-commerce (formerly awesome-x402) — list entry (PR)

- Upstream: `Merit-Systems/awesome-agentic-commerce`, README
  "Facilitators & Networks" section.
- Staged branch:
  <https://github.com/mikedotexe/awesome-agentic-commerce/tree/x402-near-facilitator-listing>
- **PR opened 2026-07-23:** <https://github.com/Merit-Systems/awesome-agentic-commerce/pull/510>

### 5. x402.watch / @swader/x402facilitators — facilitator directory (PR)

- Chain-neutral community directory (<https://facilitators.x402.watch>,
  npm `@swader/x402facilitators`) that other tools consume as a
  facilitator metadata source. Upstream: `Swader/x402facilitators`.
- Staged branch (adds `Network.NEAR`, the NEP-141 USDC token constant,
  explorer/icon wiring, and our entry; `tsc --noEmit` clean):
  <https://github.com/mikedotexe/x402facilitators/tree/x402-near-facilitator-listing>
- **PR opened 2026-07-23:** <https://github.com/Swader/x402facilitators/pull/15>
- The entry's logo references
  `docs/assets/near-x402-facilitator.svg` in this repository (must be on
  `main` before the PR is opened).

### 6. NEAR Catalog — ecosystem directory (web form)

- Submit at <https://submit.nearcatalog.xyz/> (requires NEAR-account
  login; editorial review). The NEAR-native directory — strongest
  audience fit. Suggested category: infrastructure/payments; link the
  repo, both facilitator endpoints, and the demo workload.

### 7. near/awesome-near — official curated list (PR)

- Staged branch (entry in "AI and Cloud Services"):
  <https://github.com/mikedotexe/awesome-near/tree/x402-near-facilitator-listing>
- Open the PR from
  <https://github.com/near/awesome-near/compare/main...mikedotexe:awesome-near:x402-near-facilitator-listing>

### 8. x402-list.com — facilitator registry (blocked: address format)

- Submission API (`POST /api/v1/submit`, `"type": "facilitator"`)
  accepts arbitrary network names, but `settler_addresses` validation
  only allows EVM `0x` or Solana base58 — NEAR named accounts
  (`x402-relayer2.mike.near`) cannot pass. Feature request needed
  before we can submit; their on-chain probe is EVM-only anyway
  (non-EVM claims are manually reviewed).

### 9. Bazaar — reference only

- Coinbase's Bazaar discovery layer
  (<https://docs.cdp.coinbase.com/x402/bazaar>) indexes resources behind
  the CDP facilitator; as a self-hosted facilitator we are out of scope.
  x402scan (target 1) is the discovery surface that applies.

## Client integration note (learned from the live paid flows)

Clients talking to this facilitator through the official middleware must
send the `payment-identifier` extension in its full canonical envelope —
`{"payment-identifier": {"info": {"required": true, "id": "…"}, "schema":
{…}}}` — echoing the `schema` object from the 402 requirements. An
`info`-only entry is rejected as non-canonical (the facilitator validates
extension entries against `additionalProperties: false`). Replays must
resend the byte-identical signed payload: the reference workload binds
each payment identifier to the exact payload fingerprint, so a re-signed
payment with a reused identifier is a `409` conflict by design.

## Housekeeping

- Mike's GitHub fork of the foundation repo was temporarily renamed by
  tooling during fork setup and has been restored to `mikedotexe/x402`.
  A leftover empty duplicate may exist as `mikedotexe/x402-foundation`;
  delete it if present (it is not referenced by anything).
