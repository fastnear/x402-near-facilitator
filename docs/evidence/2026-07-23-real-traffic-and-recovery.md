# Real traffic, mainnet v0.1.3, and settlement-recovery drill — 2026-07-23

Owner: Mike Purvis

Every funded broadcast below was part of a batch previewed field-by-field
(network, asset, amount, payer, recipient, relayer, maximum sponsorship)
and explicitly confirmed immediately before execution; the testnet
funding deviation (ping-pong of the available 1,000 atomic units instead
of one 6,000-unit transfer) was separately previewed and confirmed.

## Demo workload public go-live

- Route 53: A and AAAA records for `x402-demo.mikedotexe.com` (mainnet)
  and `x402-demo-test.mikedotexe.com` (testnet) to the facilitator host;
  change INSYNC and publicly resolving.
- Let's Encrypt issued a dedicated `x402-demo.mikedotexe.com` lineage
  covering both names (webroot bootstrap per runbook, bootstrap removed);
  the packaged deny-by-default vhosts from `deploy/demo/` serve only
  `POST /work` (GET `/` 404, GET `/work` 403, HTTP 308 to HTTPS, HSTS).
- The per-lineage `CertDaysRemaining` metric picked the new lineage up
  automatically and the `x402-demo-cert-expiry-soon` alarm reports OK.
- Both public endpoints return canonical 402s with base64
  `PAYMENT-REQUIRED` requirements headers; both `x402-demo@` systemd
  instances run the example from the v0.1.3 source tag as dedicated
  unprivileged users.

## Mainnet promotion to v0.1.3

Stop → packaged promote tool → start: **5 seconds** stop-to-ready.
`/healthz` version 0.1.3, `/readyz` 200 with all five checks ready
locally and publicly, deployment smoke checks passed. Testnet had soaked
on v0.1.3 through the evening, including all testnet traffic below. Both
environments now run v0.1.3 with the drill-validated v0.1.2 rollback
path behind them.

## Real paid traffic (reference workload, both networks)

Full x402 client flow against the public demo endpoints: unpaid 402 →
NEP-366 signed payment with the `payment-identifier` extension →
`PAYMENT-SIGNATURE` resubmit → facilitator verify + settle →
deterministic work result with settlement receipt header.

Testnet (payer `mike.testnet` → `merchant.mike.testnet`, 1,000 atomic
test-USDC per settle, relayer `x402-relayer.mike.testnet`; funding
ping-pong transfers `merchant → mike` of 1,000 each:
`CedjfaxzsZzeuT1mhxWL3dSi5ZLU7Vgcy4dpHhWW7CKR`,
`8EXzdSCRQfEX2VaT88CxxQEpN2NLXdTAGdh3GNrrgnRg`,
`7oLXewh995cpqcNNEjsD2TD8pGRD5BPnhEjXFpf1mp1t`,
`GJk8EZF5AWXL9wyMTkCUmVS1aLDMqBG9w4TXPhBUuzwF`):

- Settlements `HFyKqyKrz7BT2zV4QyiywdXeHoHf6FCCHgSogLty8UX3`,
  `BGP6KQxMsGY8zMeCzhF8zhb5TCsZxqFeHyknuxbRcuT5`,
  `DHZfnF4dD11t1uKMy8VT5GY6ShHtEkiuLaxUuSyLkgu9` — each delivered the
  deterministic result with a success receipt.
- Replay of the byte-identical signed request returned the cached result
  (`replayed: true`) with no new settlement; the same identifier with a
  tampered payload returned `409`.

Mainnet (payer `mike.near` → `count.mike.near`, 1,000 atomic Circle USDC
per settle, relayer `x402-relayer2.mike.near`):

- Settlements `DR76XNQcR1NNkDk8RZ62NXM3a7KDv1hbjYrPCGhf8S2P` and
  `6VCZNjomBRzW8WahjLyDaUHDT3SN2Qa73MWrKjGdktyi`; replay-cached and
  tampered-payload `409` behaviors confirmed identically.

No customer-identifying data is recorded; all parties are operator
accounts. A client-side interop note from these flows (canonical
extension envelope; byte-identical replays) is recorded in
`docs/distribution.md`.

## Indeterminate-settlement recovery drill (testnet)

Performed with the scoped short-lived `recovery-drill` API client,
following the runbook's indeterminate-settlement procedure exactly.

1. Two induced crash-before-broadcast cases: the facilitator was
   SIGKILLed while drill settlements were still `reserved`. With the
   service stopped, `x402-near-admin reconcile` acquired exclusive
   leadership and terminalized each as `failed /
   recovered_before_prepare`; no broadcast had occurred and no funds
   moved.
2. The true indeterminate case: a database-triggered watcher SIGKILLed
   the service the instant the drill settlement reached `submitted`
   (21:02:38 UTC) — the transaction was broadcast but the outcome was
   unobserved and the client received a 502. With the service kept
   stopped, `x402-near-admin reconcile` queried the exact stored hash,
   identity-matched the on-chain result, and terminalized the row as
   `succeeded` **without any rebroadcast**
   (transaction `GoHZCwDtNv6c1mzoZb6f38Bnzfy28z2zMjziFmDLyaWC`).
3. After restart the service returned `/readyz` 200 with zero
   nonterminal settlements. Final balances prove exactly one transfer
   for the drill payment: `mike.testnet` 1,000 → 0,
   `merchant.mike.testnet` 0 → 1,000.

The `recovery-drill` client was revoked after the drill and its local
key material destroyed.
