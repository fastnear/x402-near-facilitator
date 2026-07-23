# Testnet go-live and readiness evidence — 2026-07-23

Owner: Mike Purvis

This record covers the testnet facilitator reaching public readiness on the
launch host and the pre-funding validation of the payment path. It does not
by itself claim a completed funded settlement; the funded acceptance
transactions and their receipts are recorded separately when broadcast.

## Release and install

- Release `v0.1.2` (published, attested) installed under
  `/opt/x402-near-facilitator/releases/v0.1.2`; archive checksum and SLSA
  provenance verified on the operator workstation before staging. Both asset
  manifests verified on-host; both binaries pass `--version`.
- `v0.1.2` fixed two deployment-blocking defects found on this host: the
  installer left the release directory mode `0700` (unprivileged service
  could not traverse it), and the service rejected systemd's ACL-guarded
  `LoadCredential` secrets (`0440 root:root` reported the ACL mask in the
  group bits). Both fixed with tests; `v0.1.1` remains installed as the
  rollback target.

## Host readiness

- `current-testnet` -> `releases/v0.1.2`; `x402-near-facilitator@testnet`
  active and enabled at boot, version `0.1.2`, bound to `127.0.0.1:8403`.
- Local `/readyz` returned `ready: true` with all checks `ready`
  (database, leadership, reconciliation, rpc, relayer).
- Public through nginx/TLS at `https://test.x402.mikedotexe.com`:
  `/healthz` and `/readyz` return 200, `/supported` advertises exactly x402
  v2 / `exact` / `near:testnet` with the `payment-identifier` extension and
  the `x402-relayer.mike.testnet` signer, unauthenticated `/verify` returns
  401, and the mainnet host returns 502 (its service is intentionally
  stopped).
- `scripts/verify-deployment.sh` gates pass against the public URL.

## Readiness fail-closed characteristic

Readiness requires both the primary (`rpc.testnet.fastnear.com`) and backup
(`rpc.testnet.near.org`) RPCs to agree on network identity and finality.
During transient testnet finality lag, `/readyz` briefly returns 503
(`rpc: not_ready`) and recovers within seconds. This is the documented
fail-closed behavior, not a defect.

## Payment path validation (no broadcast)

A payer harness signing as `mike.testnet` built a classic NEP-366
`SignedDelegateAction` (`ft_transfer` of 1,000 atomic USDC to
`merchant.mike.testnet` on the configured Circle test contract) and posted a
`/verify` request with the issued API key. The facilitator returned HTTP 200
with `payer: "mike.testnet"` and `invalidReason: "insufficient_funds"`,
confirming API-key authentication, delegate signature verification, and
payer attribution before any funds were moved. The invalid result reflects
`mike.testnet` holding zero USDC prior to funding.

## Funded acceptance (completed 2026-07-23)

Each funded broadcast below was previewed and explicitly confirmed
immediately before submission. Asset is the configured Circle test USDC
contract; amounts are 1,000 atomic units.

- Direct transfer `merchant.mike.testnet` → `mike.testnet`, transaction
  `GjUHrMfYm2UUXaqQaLED1KSuwjKr7P35XCFhkXFQMzkF`. Balance delta exactly
  +1,000 to the recipient.
- Post-funding `/verify` returned `isValid: true` with `payer:
  mike.testnet`.
- Facilitated settlement `mike.testnet` → `merchant.mike.testnet` through
  `x402-relayer.mike.testnet`, transaction
  `FHuswy7QNXc1T1nHHR5jT55f8UML8rNW2iwmDnsrzgdP`. Outer result
  `SuccessValue`, all four receipts succeeded (the inner NEP-141
  `ft_transfer` receipt succeeded), sponsorship cost `0.000331` NEAR
  against a `0.01` NEAR maximum reservation.
- Fail-closed behavior confirmed: a `/settle` submitted during a transient
  RPC readiness dip returned HTTP 503 `settlement_unavailable` with no
  broadcast — the relayer balance and nonce were unchanged — and succeeded
  on retry once readiness was stable.
- Replay safety confirmed: after a second funded round trip (funding
  transaction `9YjGMn37w2UGqqtSwVJyfWsNbhseLRNcR7z18hobcqQV`, settlement
  `G9d4cNKeYZY5ysfDRXFgEbpN9ABUThHCy5Th9LBSVVPp`), resubmitting the exact
  same request returned `duplicate_settlement` with an empty transaction and
  an unchanged relayer balance, proving no second transfer was created.

The validation API key was rotated after acceptance; the prior key is
revoked and returns 401.

## Readiness backup-RPC note

The readiness dips observed during acceptance trace to the configured
backup RPC `rpc.testnet.near.org` responding in roughly 3.3–3.8 seconds,
while the primary `rpc.testnet.fastnear.com` responds in well under one
second. Adopting a faster testnet backup RPC would reduce readiness
flapping; the current fail-closed behavior is correct in the meantime.
