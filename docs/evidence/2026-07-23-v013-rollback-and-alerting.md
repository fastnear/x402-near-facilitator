# v0.1.3 release, rollback drill, and automated alerting — 2026-07-23

Owner: Mike Purvis

## v0.1.3 release

Purpose: carry the merged dependency majors and operational assets, and —
because `v0.1.1` cannot run on this host — create the first valid on-host
rollback target (`v0.1.2`). No schema changes.

- Content merged through squash PRs #26 and #32–#38: consolidated
  dependency majors (artifact/attestation/CI actions, `hmac` 0.13,
  `sha2` 0.11, `rand` 0.10 with a new typed entropy-failure path,
  `tower-http` 0.7, example `express` 5.2.1), version-controlled host
  monitoring/alerting assets (`deploy/monitoring/`), demo workload assets
  (`deploy/demo/`), and the version bump. The Rust toolchain bump
  (dependabot #1) was deliberately deferred as an audited-baseline change.
- Signed annotated tag `v0.1.3` on the protected `main` head; the release
  workflow's six jobs passed on the first run — the first live run of
  `upload-artifact` v7.0.1, `download-artifact` v8.0.1,
  `attest-build-provenance` v4.1.1, and `attest-sbom` v4.1.0 — publishing
  the immutable release with checksum, SBOM, and verified provenance/SBOM
  attestations, plus the GHCR image and byte-identical `latest` alias.
- Install: archive checksum and `gh attestation verify` confirmed on the
  operator workstation, staged to a root-0700 path, installed by the
  packaged installer (`releases/v0.1.3` root-owned 0755, binary
  `--version` = 0.1.3), then testnet promoted and restarted. Local and
  public `/readyz` 200 with all five checks ready; public deployment
  smoke checks passed; a non-broadcast `/verify` attributed the payer
  correctly through the new dependency stack. Mainnet promotion is
  pending its own confirmation.

## Rollback drill (testnet)

Executed exactly per the runbook rollback procedure at 20:34 UTC:

- `systemctl stop` → `releases/v0.1.2/deploy/promote-release.sh testnet
  v0.1.2` (the prior release's own packaged tool, which ran its ABI smoke
  check before the atomic pointer swap) → `systemctl start`.
- **Stop-to-ready: 4 seconds.** Startup reconciliation completed; `/readyz`
  200 with all checks ready on version 0.1.2; the mainnet pointer was
  untouched throughout.
- The rolled-back binary served the schema written under v0.1.3 (no schema
  changes between the releases, satisfying the schema-compatibility
  precondition), passed the public deployment smoke checks, and answered a
  non-broadcast `/verify` correctly.
- Roll-forward to v0.1.3 by the same procedure: 4 seconds stop-to-ready.

The binary rollback path is now tested. Both environments share the same
host, release store, and mechanism, and both now have a runnable prior
release installed.

## Automated alerting

Closes the "external `/readyz` monitoring is the only automated alerting"
gap. Assets are version-controlled in `deploy/monitoring/` and installed on
the host; full design, thresholds, and the failure-path coverage map live
in `deploy/monitoring/README.md`.

- The host pushes `RelayerBalanceNear` (both networks, read from the same
  config and RPC the service uses), per-lineage `CertDaysRemaining`, and a
  nightly `BackupSuccess` signal to CloudWatch namespace `x402near`
  (us-east-1) every five minutes via a sandboxed systemd timer.
- The nightly backup script now fails its unit when an S3 push fails
  (previously a silent warning) and publishes `BackupSuccess` only on full
  success.
- `OnFailure=` hooks on the backup unit, `certbot.service`, and the
  metrics unit itself publish the failing unit name to the existing SNS
  alert topic.
- The instance role gained exactly two scoped permissions:
  `cloudwatch:PutMetricData` conditioned to the `x402near` namespace and
  `sns:Publish` limited to the alert topic.
- Four alarms — mainnet balance < 2 NEAR, testnet balance < 3 NEAR,
  certificate < 21 days, backup missing for a day — all treat missing
  data as breaching, so each doubles as a host/timer/credential dead-man
  switch alongside the existing Route 53 `/readyz` health checks.

Verification performed:

- An induced backup-unit failure fired the `OnFailure=` chain and returned
  an SNS message ID; delivery to the confirmed operator subscription was
  observed.
- All four alarms were force-fired in a delivery drill and recovered.
- The dead-man design caught a real defect during rollout: the metrics
  script initially published with a malformed dimension syntax, the
  alarms latched on the resulting missing data exactly as designed, and
  the fix (PR #38) restored OK state across all alarms.
