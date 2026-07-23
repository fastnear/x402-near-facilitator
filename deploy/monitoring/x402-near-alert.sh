#!/bin/bash
# Publish a systemd unit failure to the facilitator SNS alert topic.
# Invoked as x402-near-alert@<failing-unit>.service via OnFailure=.
set -euo pipefail

readonly REGION=us-east-1
readonly TOPIC_ARN=arn:aws:sns:us-east-1:341982967115:x402-facilitator-alerts

unit=${1:?failing unit name required}
host=$(hostname)
aws sns publish \
  --region "$REGION" \
  --topic-arn "$TOPIC_ARN" \
  --subject "x402-near unit failure: $unit" \
  --message "systemd unit '$unit' failed on host '$host' at $(date -u -Is). Inspect with: journalctl -u '$unit' -n 100"
