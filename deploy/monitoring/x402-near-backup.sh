#!/bin/bash
# Nightly PostgreSQL dumps for both facilitator databases, with off-host
# copies in encrypted S3 and a CloudWatch success signal.
#
# Local dumps are root-only with 14-day retention. The S3 push uses the
# instance role (temporary credentials via IMDSv2; no static key). Any
# failure — including a failed S3 push — exits nonzero so the unit's
# OnFailure alert fires; the BackupSuccess metric is published only when
# every dump and every push succeeded, feeding a dead-man alarm that
# treats a missing daily datapoint as an incident.
set -euo pipefail

readonly REGION=us-east-1
readonly NAMESPACE=x402near
dest=/var/backups/x402-near
bucket=x402-near-backups-341982967115
install -d -m 0700 -o root -g root "$dest"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
push_failed=0
for db in x402_near_mainnet x402_near_testnet; do
  out="$dest/${db}-${stamp}.dump"
  sudo -u postgres pg_dump -Fc "$db" > "$out"
  chmod 600 "$out"
  echo "wrote $out ($(stat -c %s "$out") bytes)"
  if aws s3 cp "$out" "s3://${bucket}/dumps/$(basename "$out")" --only-show-errors; then
    echo "pushed $(basename "$out") to s3://${bucket}/dumps/"
  else
    echo "ERROR: s3 push failed for $(basename "$out")" >&2
    push_failed=1
  fi
done
find "$dest" -name '*.dump' -mtime +14 -delete
if [ "$push_failed" -ne 0 ]; then
  exit 1
fi
aws cloudwatch put-metric-data \
  --region "$REGION" \
  --namespace "$NAMESPACE" \
  --metric-name BackupSuccess \
  --value 1 \
  --unit Count
