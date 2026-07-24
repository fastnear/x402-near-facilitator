# Changelog

All notable changes to the x402 NEAR facilitator are recorded here. The format
follows [Keep a Changelog](https://keepachangelog.com/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.2.0] — unreleased

Internal refactor toward multi-chain settlement. **NEAR behavior is unchanged**;
this release is a structural milestone validated by the full DB-backed suite
(recovery, store, leadership, HTTP) at every step.

### Changed

- The durable settlement engine (`service.rs`) now speaks a chain-neutral
  `ChainProvider` surface (`verify` / `prepare` / `broadcast` / `reconcile_status`
  / `rebroadcast` / `signer_head`) over neutral value types, instead of threading
  NEAR primitives directly. The concrete NEAR logic — receipt-graph
  interpretation, dual-RPC raw-outcome conflict detection, and exact-byte replay —
  now lives inside the provider. The engine no longer references a NEAR primitive
  on the settlement path.
- Configuration gained a `chain_kind` discriminator (`near` | `eip155`), defaulting
  to `near` so existing host configs parse unchanged. NEAR-specific validation is
  isolated behind it; an `eip155` config is recognized and rejected with a clear
  message until the EVM provider lands.

### Operational

- Monitoring/backup scripts no longer assume exactly two NEAR environments:
  metrics iterate installed instance configs (chain-aware), backups discover every
  `x402_*` instance database, and deployment verification accepts the Base
  (`eip155`) networks alongside NEAR.

### Notes

- No public HTTP contract change. `/supported` still advertises the NEAR `exact`
  scheme; the relayer, sponsorship, and idempotency semantics are identical.

## [0.1.3]

Initial hardened NEAR-only release lineage (durable journal, dual-RPC
reconciliation, leadership failover, sponsorship budgets). See git history.
