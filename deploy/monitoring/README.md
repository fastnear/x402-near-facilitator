# Host monitoring and alerting assets

Version-controlled copies of the operational monitoring assets installed on
the facilitator host. These are operator-installed host assets; they are not
part of the release archive and changing them never requires a new release.

All metrics and alarms live in CloudWatch region `us-east-1`, namespace
`x402near`, colocated with the `/readyz` health-check alarms and the SNS
topic `arn:aws:sns:us-east-1:341982967115:x402-facilitator-alerts`.

## What runs where

| Asset | Installed at | Purpose |
| --- | --- | --- |
| `x402-near-metrics.sh` | `/usr/local/bin/` | Push relayer balances + per-lineage cert expiry every 5 minutes |
| `x402-near-metrics.{service,timer}` | `/etc/systemd/system/` | Drive the metrics push |
| `x402-near-backup.sh` | `/usr/local/bin/` | Nightly dumps, S3 push, `BackupSuccess` signal |
| `x402-near-backup.{service,timer}` | `/etc/systemd/system/` | Drive the nightly backup |
| `x402-near-alert.sh` | `/usr/local/bin/` | Publish a unit failure to SNS |
| `x402-near-alert@.service` | `/etc/systemd/system/` | `OnFailure=` target for any monitored unit |
| `certbot-onfailure.conf` | `/etc/systemd/system/certbot.service.d/x402-near-onfailure.conf` | Alert on failed certificate renewal |

Install scripts mode 0755 root-owned, units 0644 root-owned, then
`systemctl daemon-reload` and `systemctl enable --now` both timers.

## Instance-role policy

The host authenticates with the `x402-near-backup-role` instance role
(IMDSv2 temporary credentials, no static key). Inline policy
`s3-backup-put`, least-privilege:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "s3:PutObject",
      "Resource": "arn:aws:s3:::x402-near-backups-341982967115/dumps/*"
    },
    {
      "Effect": "Allow",
      "Action": "cloudwatch:PutMetricData",
      "Resource": "*",
      "Condition": { "StringEquals": { "cloudwatch:namespace": "x402near" } }
    },
    {
      "Effect": "Allow",
      "Action": "sns:Publish",
      "Resource": "arn:aws:sns:us-east-1:341982967115:x402-facilitator-alerts"
    }
  ]
}
```

The host can write dumps, metrics in the `x402near` namespace, and alert
messages to the one topic — nothing else: no list, read, delete, or any
other namespace/topic.

## Alarms

All alarms notify the SNS topic on both ALARM and OK. `TreatMissingData:
breaching` makes every alarm double as a dead-man switch: a stopped timer,
broken credential, or dead host raises the same alert as the condition
itself.

| Alarm | Metric | Threshold | Periods |
| --- | --- | --- | --- |
| `x402-mainnet-relayer-balance-low` | `RelayerBalanceNear{Network=mainnet}` | `< 2` NEAR | 3 × 5 min |
| `x402-testnet-relayer-balance-low` | `RelayerBalanceNear{Network=testnet}` | `< 3` NEAR | 3 × 5 min |
| `x402-cert-expiry-soon` | `CertDaysRemaining{Host=x402.mikedotexe.com}` | `< 21` days | 3 × 5 min |

The metrics script emits one `CertDaysRemaining` datapoint per Let's
Encrypt lineage; when a new lineage is issued (for example a demo
workload's hostnames), create a matching alarm on its `Host` dimension.
| `x402-backup-missing` | `BackupSuccess` (Sum, 1-day period) | `< 1` | 1 × 1 day |

The balance thresholds sit above the configured service warning thresholds,
so the operator is paged with refill headroom before the facilitator itself
starts warning, and well before the hard-stop halts settlement.

## Failure-path coverage

- Relayer balance low → balance alarm (before the service's own warning).
- Metrics push broken (timer, RPC, credentials, host down) → missing-data
  on every 5-minute metric → balance/cert alarms.
- Nightly backup fails, including a failed S3 push → unit exits nonzero →
  `OnFailure=` SNS alert, and no `BackupSuccess` datapoint → dead-man alarm
  within a day.
- Certificate renewal failing → `certbot.service` `OnFailure=` alert
  immediately, and `CertDaysRemaining` decays toward the 21-day alarm as a
  backstop.
- Service unhealthy → the existing Route 53 `/readyz` health-check alarms.
