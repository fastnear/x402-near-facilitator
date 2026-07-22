#!/bin/sh
set -eu

usage() {
  echo "usage: $0 <release.tar.gz> <release.tar.gz.sha256>" >&2
  exit 2
}

[ "$#" -eq 2 ] || usage
[ "$(id -u)" -eq 0 ] || {
  echo "error: install-release must run as root" >&2
  exit 1
}

archive=$1
checksum=$2
[ -f "$archive" ] || {
  echo "error: archive not found: $archive" >&2
  exit 1
}
[ -f "$checksum" ] || {
  echo "error: checksum not found: $checksum" >&2
  exit 1
}

archive_name=$(basename -- "$archive")

case "$archive_name" in
  x402-near-facilitator-v*-x86_64-unknown-linux-gnu.tar.gz) ;;
  *)
    echo "error: unexpected release archive name: $archive_name" >&2
    exit 1
    ;;
esac

release_root=/opt/x402-near-facilitator/releases
install -d -m 0755 "$release_root"
root_staging=$(mktemp -d "$release_root/.install.XXXXXX")
chmod 0700 "$root_staging"
cleanup() {
  rm -rf "$root_staging"
}
trap cleanup EXIT HUP INT TERM

# Cross the privilege boundary exactly once. Everything parsed, hashed,
# inspected, or extracted below is the root-owned staging copy.
archive_copy=$root_staging/$archive_name
checksum_copy=$root_staging/checksum
install -m 0600 -- "$archive" "$archive_copy"
install -m 0600 -- "$checksum" "$checksum_copy"

checksum_lines=$(awk 'NF { count += 1 } END { print count + 0 }' "$checksum_copy")
[ "$checksum_lines" -eq 1 ] || {
  echo "error: checksum file must contain exactly one non-empty entry" >&2
  exit 1
}
set -- $(awk 'NF { print $1, $2 }' "$checksum_copy")
[ "$#" -eq 2 ] || {
  echo "error: malformed checksum file" >&2
  exit 1
}
expected_hash=$1
expected_name=${2#\*}
case "$expected_hash" in
  *[!0-9A-Fa-f]* | "")
    echo "error: checksum is not hexadecimal" >&2
    exit 1
    ;;
esac
[ "${#expected_hash}" -eq 64 ] || {
  echo "error: checksum is not SHA-256" >&2
  exit 1
}
[ "$expected_name" = "$archive_name" ] || {
  echo "error: checksum names $expected_name, not $archive_name" >&2
  exit 1
}
actual_hash=$(sha256sum "$archive_copy" | awk '{ print $1 }')
[ "$actual_hash" = "$expected_hash" ] || {
  echo "error: release archive checksum mismatch" >&2
  exit 1
}

version=${archive_name#x402-near-facilitator-}
version=${version%-x86_64-unknown-linux-gnu.tar.gz}
archive_root=${archive_name%.tar.gz}
destination=$release_root/$version

[ ! -e "$destination" ] || {
  echo "error: release already installed: $destination" >&2
  exit 1
}

unpack=$root_staging/unpack
install -d -m 0700 "$unpack"

members=$root_staging/archive-members
tar -tzf "$archive_copy" >"$members"
awk -v root="$archive_root/" '
  {
    if ($0 == "" || substr($0, 1, 1) == "/" || substr($0, 1, length(root)) != root) {
      exit 1
    }
    count = split($0, parts, "/")
    for (part_number = 1; part_number <= count; part_number += 1) {
      if (parts[part_number] == "..") {
        exit 1
      }
    }
  }
' "$members" || {
  echo "error: archive contains an unsafe or unexpected member path" >&2
  exit 1
}
if tar -tvzf "$archive_copy" | awk '
  substr($1, 1, 1) == "l" || substr($1, 1, 1) == "h" { found = 1 }
  END { exit found ? 0 : 1 }
'; then
  echo "error: archive must not contain symbolic or hard links" >&2
  exit 1
fi
rm -f "$members"

tar -xzf "$archive_copy" -C "$unpack" --strip-components=1 \
  --no-same-owner --no-same-permissions
if find "$unpack" ! -type f ! -type d -print -quit | grep -q .; then
  echo "error: extracted release contains a special file" >&2
  exit 1
fi
for binary in x402-near-facilitator x402-near-admin; do
  [ -f "$unpack/$binary" ] && [ ! -L "$unpack/$binary" ] || {
    echo "error: release is missing $binary" >&2
    exit 1
  }
  chmod 0755 "$unpack/$binary"
done
[ -x "$unpack/deploy/promote-release.sh" ] || {
  echo "error: release is missing the promotion tool" >&2
  exit 1
}

chown -R root:root "$unpack"
chmod -R go-w "$unpack"
mv "$unpack" "$destination"

echo "installed $version; no environment was promoted and no service was restarted"
