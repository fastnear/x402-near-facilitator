#!/bin/sh
set -eu

[ "$(id -u)" -eq 0 ] || {
  echo "error: installer integration test must run as root" >&2
  exit 1
}

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=v999.999.999
archive_root=x402-near-facilitator-$version-x86_64-unknown-linux-gnu
destination=/opt/x402-near-facilitator/releases/$version

[ ! -e "$destination" ] || {
  echo "error: reserved installer-test destination already exists: $destination" >&2
  exit 1
}

work=$(mktemp -d)
cleanup() {
  rm -rf "$work" "$destination"
}
trap cleanup EXIT HUP INT TERM

payload=$work/payload/$archive_root
install -d -m 0755 "$payload/deploy"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$payload/x402-near-facilitator"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$payload/x402-near-admin"
install -m 0755 \
  "$repo_root/deploy/promote-release.sh" \
  "$payload/deploy/promote-release.sh"
chmod 0755 "$payload/x402-near-facilitator" "$payload/x402-near-admin"

archive=$work/$archive_root.tar.gz
tar -czf "$archive" -C "$work/payload" "$archive_root"
(
  cd "$work"
  sha256sum "$archive_root.tar.gz" >"$archive_root.tar.gz.sha256"
)

"$repo_root/deploy/install-release.sh" \
  "$archive" \
  "$archive.sha256"

test -x "$destination/x402-near-facilitator"
test -x "$destination/x402-near-admin"
test -x "$destination/deploy/promote-release.sh"
test ! -e /opt/x402-near-facilitator/current-mainnet
test ! -e /opt/x402-near-facilitator/current-testnet

echo "installer integration test passed"
