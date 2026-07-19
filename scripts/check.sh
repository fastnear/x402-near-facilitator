#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked

if command -v cargo-deny >/dev/null 2>&1; then
  # RustSec is checked by cargo-audit below. Keeping the advisory scan out of
  # cargo-deny also avoids making this gate depend on cargo-deny's support for
  # the newest CVSS document version.
  cargo deny check bans licenses sources
else
  echo "note: cargo-deny is not installed; CI must run this gate" >&2
fi

if command -v cargo-audit >/dev/null 2>&1; then
  # sqlx's facade declares every optional database driver, so Cargo.lock
  # contains the inactive sqlx-mysql -> rsa edge. Refuse the exception if rsa
  # ever becomes reachable by a production workspace target.
  if cargo tree --workspace --edges normal,build -i rsa 2>/dev/null | grep -q .; then
    echo "error: rsa became reachable; remove the RUSTSEC-2023-0071 exception" >&2
    exit 1
  fi
  cargo audit --ignore RUSTSEC-2023-0071
  cargo audit --file fuzz/Cargo.lock --ignore RUSTSEC-2023-0071
else
  echo "note: cargo-audit is not installed; CI must run this gate" >&2
fi

python3 -m json.tool deploy/config/mainnet.json.example >/dev/null
python3 -m json.tool deploy/config/testnet.json.example >/dev/null
python3 scripts/check-docs.py

git diff --check
