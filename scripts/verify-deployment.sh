#!/bin/sh
set -eu

usage() {
  echo "usage: $0 <base-url> [api-key-file]" >&2
  exit 2
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage
base_url=${1%/}
key_file=${2-}

case "$base_url" in
  https://*) ;;
  *)
    echo "error: public deployment checks require an https URL" >&2
    exit 1
    ;;
esac

curl_common="--fail --silent --show-error --max-time 15"

# shellcheck disable=SC2086
curl $curl_common "$base_url/healthz" >/dev/null
# shellcheck disable=SC2086
curl $curl_common "$base_url/readyz" >/dev/null
# shellcheck disable=SC2086
supported=$(curl $curl_common "$base_url/supported")

printf '%s' "$supported" | python3 -c '
import json
import sys

body = json.load(sys.stdin)
assert len(body["kinds"]) == 1
kind = body["kinds"][0]
assert kind["x402Version"] == 2
assert kind["scheme"] == "exact"
assert kind["network"] in {
    "near:testnet", "near:mainnet",   # NEAR instances
    "eip155:84532", "eip155:8453",    # Base Sepolia / Base mainnet (eip155)
}
assert "payment-identifier" in body["extensions"]
assert isinstance(body["signers"], dict)
'

unauthenticated_status=$(
  curl --silent --output /dev/null --write-out '%{http_code}' \
    --max-time 15 \
    --header 'Content-Type: application/json' \
    --data '{}' \
    "$base_url/verify"
)
[ "$unauthenticated_status" = "401" ] || {
  echo "error: unauthenticated /verify returned $unauthenticated_status, expected 401" >&2
  exit 1
}

if [ -n "$key_file" ]; then
  [ -f "$key_file" ] || {
    echo "error: API key file not found: $key_file" >&2
    exit 1
  }
  python3 -c '
import os
import sys

if os.stat(sys.argv[1]).st_mode & 0o077:
    raise SystemExit(1)
' "$key_file" || {
    echo "error: API key file has group or world permissions" >&2
    exit 1
  }

  authenticated_status=$(
    {
      printf 'header = "X-API-Key: '
      tr -d '\r\n' <"$key_file"
      printf '"\n'
    } | curl --config - \
      --silent --output /dev/null --write-out '%{http_code}' \
      --max-time 15 \
      --header 'Content-Type: application/json' \
      --data '{}' \
      "$base_url/verify"
  )
  [ "$authenticated_status" = "400" ] || {
    echo "error: authenticated malformed /verify returned $authenticated_status, expected 400" >&2
    exit 1
  }
fi

echo "deployment smoke checks passed for $base_url"
