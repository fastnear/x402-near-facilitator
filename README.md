# x402 facilitator for NEAR

A production-oriented Rust facilitator for x402 v2 `exact` payments in Circle
USDC on NEAR. It verifies a payer's NEP-366 signed delegate action, sponsors
the outer transaction through a dedicated relayer, and reports success only
after the inner NEP-141 `ft_transfer` receipt succeeds.

> **Status: live.** Both production facilitators have run v0.1.3 since
> 2026-07-23 — testnet at `test.x402.mikedotexe.com` and mainnet at
> `x402.mikedotexe.com` — with real paid traffic through the public demo
> workload. Evidence:
> [testnet go-live](docs/evidence/2026-07-23-testnet-golive.md),
> [mainnet go-live](docs/evidence/2026-07-23-mainnet-golive.md), and the
> later entries in [docs/evidence/](docs/evidence/); gate-by-gate record
> in [the launch checklist](docs/launch-checklist.md).

## Launch profile

| Environment | Network | Planned URL | Asset | Initial payee |
| --- | --- | --- | --- | --- |
| Testnet | `near:testnet` | `https://test.x402.mikedotexe.com` | Circle test USDC | `merchant.mike.testnet` |
| Mainnet | `near:mainnet` | `https://x402.mikedotexe.com` | Circle native USDC | `count.mike.near` |

The service is intentionally narrow at launch:

- x402 v2 and the `exact` scheme only.
- One configured NEAR network and one Circle USDC contract per process.
- ED25519 and SECP256K1 classic NEP-366 delegate actions.
- API-key authentication on `/verify` and `/settle`.
- Exact per-client network, asset, and payee allowlists.
- PostgreSQL-backed settlement deduplication, relayer nonce serialization,
  sponsorship budgets, and restart reconciliation.
- Optional `payment-identifier` idempotency, advertised by `/supported`.

Native NEAR payments, arbitrary NEP-141 assets, anonymous settlement, wildcard
payees, gas-key relayers, and DelegateV2 are not launch features.

## Architecture

The workspace builds two binaries and one reusable mechanism crate:

- `x402-chain-near` implements NEAR verification, RPC access, settlement, and
  final receipt validation using the extension traits from
  [`x402-rs`](https://github.com/x402-rs/x402-rs).
- `x402-near-facilitator` provides the authenticated Axum HTTP boundary and
  durable PostgreSQL workflow.
- `x402-near-admin` performs migrations and administrative operations without
  exposing secrets through the public service.

The existing x402
[`exact` NEAR specification](https://github.com/x402-foundation/x402/blob/main/specs/schemes/exact/scheme_exact_near.md)
and official TypeScript `@x402/near` package are the protocol authority.
See [architecture](docs/architecture.md) and
[threat model](docs/threat-model.md) for the trust and failure boundaries.

## HTTP interface

| Method | Path | Authentication | Purpose |
| --- | --- | --- | --- |
| `GET` | `/supported` | Public | Advertise this instance's network, scheme, signer, and extensions |
| `GET` | `/healthz` | Public | Process liveness only |
| `GET` | `/readyz` | Public | Sanitized database, leadership, reconciliation, RPC, and relayer readiness |
| `POST` | `/verify` | API key | Verify without broadcasting |
| `POST` | `/settle` | API key | Claim, reverify, submit once, and wait for the inner receipt |

The primary credential header is `X-API-Key`; `Authorization: Bearer` is also
accepted. A request may send both forms only when they contain the identical
key; conflicting values are rejected with 401. Expected payment rejection is
an HTTP 200 x402 response. Authentication, malformed input, policy limits,
idempotency conflicts, and unavailable or indeterminate infrastructure use
HTTP errors. The normative wire contract is in [OpenAPI](docs/openapi.yaml).

## Development

Rust 1.93 is pinned by `rust-toolchain.toml`.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

With `cargo-deny` and `cargo-audit` installed, the complete local check is:

```sh
./scripts/check.sh
```

Parser fuzz targets cover standard-base64/Borsh delegate decoding, strict
NEP-141 transfer JSON, and the canonical HTTP request boundary:

```sh
cargo install cargo-fuzz --version 0.13.2 --locked
rustup toolchain install nightly-2026-07-01 --profile minimal
cargo +nightly-2026-07-01 fuzz run decode_signed_delegate
cargo +nightly-2026-07-01 fuzz run decode_ft_transfer_args
cargo +nightly-2026-07-01 fuzz run parse_http_request
```

For local-only configuration, use `.env.example` as a variable inventory and
export file paths explicitly; the binary does not implicitly load `.env`.
Use unfunded or testnet credentials. Production uses JSON configuration and
systemd credentials, never an environment file. The complete configuration
contract and non-secret examples are in
[configuration](docs/configuration.md) and `deploy/config/`.

The [runnable Express reference resource server](examples/resource-server/)
uses the official x402 middleware and NEAR server scheme, requires
`payment-identifier`, and independently deduplicates paid work delivery.

## Operations

Production is deployed as two hardened systemd services behind Nginx on a
single personal host. Releases are installed under
`/opt/x402-near-facilitator/releases/<version>` and selected through an atomic
`current-mainnet` or `current-testnet` symlink. Installation never changes
either pointer: the packaged promotion tool first runs an on-host
`--version` ABI smoke check, then promotes one named environment. An OCI image
is published as a portable artifact, but it is not the production runtime.
Because the launch policy requires a loopback bind, run the image with host
networking or an equivalent loopback-only network boundary; it deliberately
exposes no bridge-network port.

Start with:

1. [API key administration](docs/api-keys.md)
2. [Operations runbook](docs/runbook.md)
3. [Launch checklist](docs/launch-checklist.md)

Publishing a GitHub release does not deploy it. Testnet must pass funded,
restart, and fault-injection acceptance before mainnet is enabled. Every
funded launch or provisioning broadcast, including testnet, requires an
immediate human confirmation of the network, asset, amount, payer, recipient,
relayer, and maximum sponsored cost. Account and access-key changes follow the
same fresh-preview gate; see the runbook.

## License and attribution

Licensed under Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). This
repository depends on and follows the modular shape of x402-rs; it does not
claim affiliation with or endorsement by x402-rs, Circle, or the x402
Foundation. Report vulnerabilities through the private process in
[SECURITY.md](SECURITY.md).
