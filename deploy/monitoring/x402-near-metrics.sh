#!/bin/bash
# Publish operational metrics for the x402 NEAR facilitator host.
#
# Every run pushes to CloudWatch (region us-east-1, namespace x402near,
# colocated with the readyz alarms and the SNS alert topic):
#   - RelayerBalanceNear{Network=mainnet|testnet}: relayer account balance
#     in NEAR, read from the same RPC endpoint the service uses.
#   - CertDaysRemaining{Host=<lineage>}: days until each Let's Encrypt
#     certificate lineage expires (one datapoint per lineage).
#
# The companion CloudWatch alarms treat missing data as breaching, so a
# host, timer, or credential failure that stops these pushes raises the
# same alert as a low balance.
set -euo pipefail

readonly REGION=us-east-1
readonly NAMESPACE=x402near
readonly CONFIG_DIR=/etc/x402-near-facilitator
readonly CERT_LIVE_DIR=/etc/letsencrypt/live

fail=0

publish() {
  local metric=$1 dimensions=$2 value=$3
  aws cloudwatch put-metric-data \
    --region "$REGION" \
    --namespace "$NAMESPACE" \
    --metric-name "$metric" \
    --dimensions "$dimensions" \
    --value "$value" \
    --unit None
}

relayer_balance_near() {
  local config=$1
  local account rpc response amount
  account=$(jq -r .relayer_account_id "$config")
  rpc=$(jq -r .primary_rpc_url "$config")
  response=$(curl -sS --fail --max-time 20 "$rpc" \
    -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":"metrics","method":"query","params":{"request_type":"view_account","finality":"final","account_id":"'"$account"'"}}')
  amount=$(jq -er .result.amount <<<"$response")
  # yoctoNEAR -> NEAR; float precision loss is irrelevant at alert scale.
  awk -v y="$amount" 'BEGIN { printf "%.6f", y / 1e24 }'
}

for network in mainnet testnet; do
  config="$CONFIG_DIR/$network.json"
  if balance=$(relayer_balance_near "$config"); then
    publish RelayerBalanceNear "Name=Network,Value=$network" "$balance"
    echo "RelayerBalanceNear network=$network balance=$balance"
  else
    echo "WARN: failed to read relayer balance for $network" >&2
    fail=1
  fi
done

found_cert=0
for cert in "$CERT_LIVE_DIR"/*/cert.pem; do
  [ -e "$cert" ] || continue
  found_cert=1
  host=$(basename "$(dirname "$cert")")
  if end_date=$(openssl x509 -enddate -noout -in "$cert" 2>/dev/null); then
    end_epoch=$(date -d "${end_date#notAfter=}" +%s)
    days=$(( (end_epoch - $(date +%s)) / 86400 ))
    publish CertDaysRemaining "Name=Host,Value=$host" "$days"
    echo "CertDaysRemaining host=$host days=$days"
  else
    echo "WARN: failed to read certificate expiry for $host" >&2
    fail=1
  fi
done
if [ "$found_cert" -eq 0 ]; then
  echo "WARN: no certificate lineages found under $CERT_LIVE_DIR" >&2
  fail=1
fi

exit "$fail"
