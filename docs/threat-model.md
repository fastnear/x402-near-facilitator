# Threat model

## Protected assets and trust boundaries

The facilitator protects:

- the dedicated relayer private key and its NEAR balance;
- payer-authorized Circle USDC transfers;
- merchant binding and API client policy;
- the single-settlement and idempotency guarantees;
- PostgreSQL journal integrity and sponsorship accounting;
- API keys, the HMAC pepper, database credentials, and Honeycomb key;
- truthful settlement responses and operational telemetry.

The process trusts its checked-in policy logic, environment-specific config,
PostgreSQL, dedicated relayer credential, and two independently operated RPC
endpoints. It does not trust callers, signed-payload fields before signature
verification, arbitrary RPC error text, forwarded client IP headers, or outer
transaction success by itself.

## Threats and required controls

| Threat | Controls | Required evidence |
| --- | --- | --- |
| Forged payer or modified delegate | Strict Borsh decode, key/signature curve match, NEAR domain-separated verification | ED25519/SECP256K1 oracle fixtures and mutation tests |
| Relayer used for arbitrary calls | One action, `ft_transfer`, fixed asset/payee/amount, 1 yocto, 30 TGas, FullAccess payer, configured relayer | Structural negative matrix |
| Wrong network or token | One pinned network/asset per process; exact API-client policy; no client RPC | Cross-network and wrong-asset rejection |
| Duplicate resource access for one payment | Global delegate dedupe; identifier/fingerprint conflict; resource-server dedupe documented | Concurrent duplicate and identifier tests |
| Relayer nonce races | One active instance; durable nonce uniqueness; mutex spanning final nonce through outcome | High-concurrency settlement test |
| Broadcast accepted but response lost | Persist exact bytes/hash, durably mark `submitted`, then broadcast; query both RPCs; exact-byte rebroadcast only while unexpired | Crash/fault injection after every journal stage |
| False success from asynchronous receipts | Bind final outcome identity to the stored transaction; require the unique inner token receipt `SuccessValue`; keep ambiguous graphs nonterminal | Outer-only, missing, ambiguous, identity-mismatch, and failed receipt fixtures |
| Payer state changes after verify | Full re-verification under the settlement claim and mutex | Nonce/balance/storage race tests |
| Sponsorship drain | API keys, exact payees, rate limits, minimum payment, gas cap, atomic daily budgets, low-balance stop | Quota, balance, and reservation rollback tests |
| API-key database theft | High-entropy one-time key, HMAC-SHA256 digest with separate pepper, constant-time compare, rotation/revocation | Authentication and redaction tests |
| Credential leakage in telemetry | Sensitive-header marking, no request bodies, bounded fields, hashes excluded from metric labels | Automated log/trace redaction test |
| PostgreSQL split brain | Session advisory leadership lock; readiness false without leadership | Competing-instance test |
| RPC lies, lags, or partitions | Finality, pinned blocks, typed errors, independent backup reconciliation, fail closed | Failover and disagreement tests |
| Database loss or tampering | Neon backups/PITR, least-privileged role, append-oriented journal, restore exercise | Dated restore drill |
| Host compromise | Dedicated unprivileged users, systemd sandboxing, root-only credentials, immutable releases | Unit hardening review and credential-permission check |
| Supply-chain substitution | Locked dependencies, deny/audit checks, checksums, SBOM, build provenance | Green release workflow and verified artifact install |

## Idempotency-specific analysis

The optional `payment-identifier` is scoped by API client and bound to a
fingerprint of the full payment payload, resource, requirements, and delegate
hash. Replaying the same identifier and fingerprint returns the same terminal
response. Reusing an identifier for different work returns HTTP 409.

An identifier is not a payment authorization and does not replace the delegate
hash. Without an identifier, or with a different identifier, the same delegate
still resolves to `duplicate_settlement`. The resource server must bind and
deduplicate the identifier independently before releasing protected work.

## Recovery decision boundary

Recovery distinguishes proof of failure from absence of proof. A typed final
transaction, delegate, reachable-receipt, or token-receipt failure is
definitive. When both RPCs report the stored hash unknown, unchanged relayer
nonces plus a passed delegate expiry height are also definitive: the
authorization can no longer execute and the exact bytes must not be
rebroadcast.

A pending lookup, RPC error or disagreement, missing or ambiguous receipt,
transaction-identity mismatch, or unknown hash paired with an advanced relayer
nonce is not safely attributable to failure. Those cases remain nonterminal
and fail readiness. An advanced nonce with an unknown hash additionally
quarantines the relayer. No recovery path signs replacement bytes.

## Residual risks

- Mainnet and testnet share a host. Host, Nginx, kernel, or network failure can
  affect both.
- Targeted preflight cannot simulate NEAR's asynchronous cross-shard runtime.
  Valid verification can still fail at settlement if state changes.
- PostgreSQL and RPC availability are launch dependencies. Failing closed
  protects funds but reduces availability.
- A compromised full-access relayer key can spend the relayer's NEAR balance.
  The deliberately small balance, daily caps, alerts, and recovery key limit
  but do not eliminate this risk.
- API clients can legitimately request many invalid preflights. Rate limits
  bound work but do not make public RPC exhaustion impossible.
- The origin is directly exposed to the public Internet with no CDN or proxy
  tier absorbing floods or TLS-layer attacks. Nginx limits, API-key
  authentication, and fail-closed readiness bound abuse, but volumetric
  denial of service is mitigated only by the host's network.
- Honeycomb receives sanitized operational metadata. Field allowlisting and
  review are still necessary before adding telemetry.
- The pinned official `@x402/near@2.19.0` development/reference dependency
  transitively includes `elliptic`, for which
  [GHSA-848j-6mx2-7j84](https://github.com/advisories/GHSA-848j-6mx2-7j84)
  currently has no patched release. It is not linked into either Rust
  production binary, and the reference resource server neither holds payer
  keys nor signs payments. CI fails on high-severity npm findings and this
  low-severity exception must be re-evaluated whenever `@x402/near` changes.

## Security review triggers

Repeat the threat review before enabling any of:

- another token or wildcard payee policy;
- native NEAR payments;
- anonymous/public settlement;
- more than one active instance or relayer;
- gas-key relayers, DelegateV2, or another signature curve;
- automatic relayer refill;
- partner-controlled webhooks or administrative HTTP endpoints;
- transaction replacement or any recovery path that signs new bytes.
