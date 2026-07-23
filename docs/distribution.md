# Distribution: registry submissions (prepared, not sent)

Everything below is staged and ready to fire; **nothing has been
submitted**. Each submission is a deliberate operator action. The NEAR
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
- URL to submit: `https://x402-demo.mikedotexe.com/work` — the indexer
  validates the live 402 schema and lists automatically.
- Optionally also register the testnet resource URL if the form accepts
  non-mainnet resources.

### 2. x402 Foundation repo — facilitators table (PR)

- Upstream: `x402-foundation/x402`, file `docs/dev-tools/facilitators.md`.
- Staged branch (based on clean upstream `main`):
  <https://github.com/mikedotexe/x402/tree/x402-near-facilitator-listing>
- Open the PR from
  <https://github.com/x402-foundation/x402/compare/main...mikedotexe:x402:x402-near-facilitator-listing>
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
- Open the PR from
  <https://github.com/Merit-Systems/awesome-agentic-commerce/compare/main...mikedotexe:awesome-agentic-commerce:x402-near-facilitator-listing>

### 5. Bazaar — reference only

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
