# Operational hardening — 2026-07-23

Owner: Mike Purvis

Post-launch operational hygiene performed after both networks reached live
acceptance. Drills were run on testnet to avoid disrupting mainnet.

## Observer database role

Each environment's `x402-<env>-observer` login received column-level
`SELECT` only on the sanitized triage surface:

- `settlements`: `state`, the lifecycle timestamps
  (`created_at`, `updated_at`, `prepared_at`, `submitted_at`,
  `finalized_at`, `last_reconciled_at`), `error_code`, and
  `reconciliation_attempts`.
- `daily_global_sponsorship`: `usage_date`, `reserved_yocto_near`,
  `spent_yocto_near`, `updated_at`.

Verified denied: `settlements.payer`, `pay_to`, `payment_hash`,
`outer_transaction_bytes`, `terminal_response_bytes`,
`delegate_public_key`, `policy_snapshot`, and the entire `api_keys`,
`api_clients`, `api_client_payees`, `daily_client_sponsorship`,
`settlement_events`, and `relayers` tables. The aggregate state query and
the global sponsorship read both succeed.

## Backups and restore drill

`/usr/local/bin/x402-near-backup.sh` dumps both databases in custom format
to `/var/backups/x402-near/` (root-owned, mode 0600, 14-day retention),
driven nightly by the `x402-near-backup.timer` systemd timer. A restore
drill streamed a dump into a scratch database: all nine tables, the
settlement rows, and both unique constraints restored, and a duplicate
settlement insert was rejected, confirming the deduplication uniqueness
survives a restore. Off-host copy to encrypted S3 is pending an AWS
permission grant; until then the dumps are local only and do not survive
host loss.

## Drills (testnet)

- Restart: the service recovers with leadership acquisition and startup
  reconciliation and returns `/readyz` 200.
- API-key revocation: a throwaway client's key authenticated (malformed
  body returned 400), then returned 401 after `client revoke`, with the
  live client unaffected.
- Low-relayer-balance halt: with the hard-stop threshold set above the
  relayer balance (both thresholds raised to preserve the
  hard-stop-below-warning invariant), the service stayed up but reported
  `relayer: not_ready` and `/readyz` 503, correctly gating settlement; the
  original configuration was then restored to ready. Note: an invalid
  sponsorship configuration crash-loops and can trip the systemd start
  limiter, requiring `systemctl reset-failed`.

## Rollback

A live rollback drill is deferred: `v0.1.1` cannot run on this host because
of the systemd credential-mode defect fixed in `v0.1.2`, so there is no
compatible prior release to roll back to until a later version ships. The
promotion tool's on-host `--version` smoke check and atomic pointer swap
remain the rollback mechanism.

## IPv6

The instance received an Amazon-provided IPv6 block (VPC `/56`, subnet
`/64`, `::/0` route to the internet gateway, a stable ENI address). The OS
configured the address via router advertisement, nginx already listens on
`[::]:80` and `[::]:443`, and the security group already admits IPv6 on
both ports. AAAA records for both hostnames resolve to the instance
address, and an external IPv6 request to `/readyz` returned 200.

## External monitoring

Both `/readyz` endpoints are monitored by AWS Route 53 health checks
(HTTPS, 30-second interval, three-failure threshold) reporting healthy from
all regional checkers. CloudWatch alarms in `us-east-1`
(`x402-mainnet-readyz-unhealthy`, `x402-testnet-readyz-unhealthy`) fire to
an SNS topic on failure and recovery. The email subscription to the
operator address is created and awaits the one-time confirmation click to
activate delivery.

## Off-host backup

The nightly dumps are pushed to a private S3 bucket
(`x402-near-backups-341982967115`, block-public, AES-256 default
encryption, versioning, 90-day lifecycle). The host authenticates with an
instance IAM role (`x402-near-backup-role`) whose only permission is
`s3:PutObject` to the `dumps/` prefix — verified least-privilege: the host
can write but cannot list, read, or delete, and holds no static credential
(temporary credentials arrive through IMDSv2). An encrypted EBS snapshot of
the root volume provides an additional immediate restore point.

## Deferred

The second incident contact was deliberately deferred. The rollback drill
remains deferred until a release later than `v0.1.2` exists.
