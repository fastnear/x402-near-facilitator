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

## Remaining testnet acceptance gates

The funded direct transfer, the facilitated return settlement, the receipt
and balance-delta evidence, and the replay-safety check remain to be
recorded. Each funded broadcast requires a fresh preview and explicit human
confirmation. The API key used for validation is scheduled for rotation
after acceptance.
