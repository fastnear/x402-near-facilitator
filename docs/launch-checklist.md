# Launch checklist

Unchecked items are not claims of completion. Add a date, owner, and durable
evidence link beside every checked gate. A specification, local test, branch,
or conversation alone is not launch evidence.

## Ownership and external prerequisites

- [ ] Operational owner and second incident contact recorded.
- [ ] Mainnet and testnet PostgreSQL databases on the launch host bind only
      loopback, with separate migration and service roles, a nightly off-host
      dump timer, and a tested restore procedure.
- [ ] Environment-specific observer logins can read only sanitized journal
      state/timestamp/reason columns and global sponsorship totals; they cannot
      read identities, hashes, payload bytes, terminal bodies, or API-key data.
- [ ] Telemetry export confirmed disabled (no OTLP endpoint or header
      credential installed); sanitized journald output verified for both
      environments.
- [x] Route 53 records for both launch hostnames point only at the intended
      host; the change batch was previewed and confirmed, and the DNS-editing
      AWS credential remains only on the operator workstation. — 2026-07-22,
      Mike Purvis,
      [EC2 host and DNS repoint evidence](evidence/2026-07-22-ec2-host-and-dns-repoint.md)
- [ ] One publicly trusted certificate covers exactly both launch hostnames;
      automated renewal, its Nginx reload hook, and expiry monitoring are
      verified.
- [ ] External verification shows both hostnames serve only packaged
      endpoints over TLS, plain HTTP redirects to HTTPS, and unknown-hostname
      or bare-IP requests are refused.
- [ ] External 60-second `/readyz` checks configured for both hostnames.
- [ ] Changed `fn-test-pro` SSH key investigated out-of-band; no key was
      accepted automatically. This host is not a launch dependency.

## Repository and supply chain

- [ ] Public `fastnear/x402-near-facilitator` repository exists with Apache-2.0
      license, branch protection, security contact, and no secrets in history.
- [ ] Rust 1.93, `Cargo.lock`, x402-rs crate versions, NEAR crate versions, and
      official `@x402/near` oracle version are pinned.
- [ ] Formatting, Clippy with warnings denied, all tests, migration test,
      oracle diff, `cargo deny`, and `cargo audit` pass in CI.
- [ ] Tagged release contains both x86-64 binaries, checksum, SBOM, provenance,
      migrations, the reviewed `docs/` and `deploy/` trees, and a GHCR image.
- [ ] The archive checksum and GitHub attestation were verified before any
      member was executed; the root installer used the packaged, attested copy
      rather than a mutable checkout, and the installed `deploy/` and `docs/`
      asset manifests pass.

## Protocol and security acceptance

- [ ] ED25519 and SECP256K1 fixtures interoperate with pinned
      `@x402/near@2.19.0`.
- [ ] Version, network, scheme, requirement, base64, Borsh/trailing-byte,
      curve, DelegateV2, action, method, JSON, token, payee, amount, deposit,
      gas, timeout, nonce, permission, account, code, balance, and storage
      negatives pass.
- [ ] Every RPC error path fails closed using typed errors.
- [ ] Receipt tests cover inner success, outer-only success, inner failure,
      missing/ambiguous token receipt, and pending status.
- [ ] Official TypeScript `HTTPFacilitatorClient` accepts `/supported`,
      `/verify`, `/settle`, and canonical failure/omission shapes.
- [ ] API-key header and Bearer alternatives pass; identical dual credentials
      pass; missing, invalid, revoked, and non-identical dual credentials fail
      without leaking comparison state.
- [ ] `payment-identifier` same-ID/same-fingerprint replay, in-flight join,
      different-fingerprint 409, and no-ID delegate duplicate pass.
- [ ] Body/content-type, rate, timeout, concurrency, minimum amount, exact
      payee, budget, and low-balance controls pass.
- [ ] Fuzz and redaction checks find no panic or sensitive output.

## PostgreSQL and recovery acceptance

- [ ] Two competing instances prove exactly one advisory-lock leader.
- [ ] Hundreds of concurrent identical settlements create one prepared outer
      transaction and at most one broadcast.
- [ ] Concurrent distinct settlements serialize relayer nonce use.
- [ ] `submitted` is durable before broadcast, and a crash after every journal
      transition recovers without re-signing.
- [ ] Accepted-but-response-dropped submission resolves by stored hash.
- [ ] Primary unknown/backup final and primary/backup disagreement paths pass.
- [ ] Expired unknown transactions fail without rebroadcast; pending,
      identity-mismatched, missing-receipt, and ambiguous outcomes remain
      nonterminal and fail readiness.
- [ ] Unknown hash with advanced relayer nonce quarantines the key and fails
      readiness.
- [ ] Revocation, payee-policy failure, budget exhaustion, lost leadership,
      database failure, and stale reservation release pass.
- [ ] Budget reservation and settlement claim rollback atomically on failure.
- [ ] Database restore drill preserves uniqueness and reconciles all
      nonterminal rows.

## Host hardening

- [ ] Separate `x402-near-mainnet` and `x402-near-testnet` users have no login
      shell or home.
- [ ] Versioned release is root-owned; `current-mainnet` and
      `current-testnet` are the only mutable pointers, installation changes
      neither, and each environment is promoted separately.
- [ ] Host glibc, architecture, kernel, systemd, and Nginx versions are
      recorded and compatible with the release build baseline; the packaged
      binary's on-host `--version` ABI smoke check passes before each
      environment pointer changes.
- [ ] Config is non-secret; credentials are separate root-owned mode-0600
      regular files and are not reused across environments.
- [ ] systemd sandbox settings and writable paths were reviewed with
      `systemd-analyze security`.
- [ ] `LimitCORE=0` is effective for both units and `coredumpctl` contains no
      service core dumps from acceptance or fault testing.
- [ ] Nginx binds public TLS; services bind only loopback ports 8402/8403.
- [ ] Nginx limits bodies to 64 KiB, preserves canonical application errors,
      disables CORS, and does not log authentication headers or bodies.
- [ ] Origin access, OS firewall, clock sync, disk space, and host patching
      checked; detailed terminal records have at least 90-day retention,
      durable settlement identities are never recycled, and sanitized
      journald plus dedicated Nginx retention is verified at no more than
      14 days.
- [ ] Binary rollback drill succeeds without a schema rollback.

## Testnet launch

- [ ] Immediately before local service-key generation, a human confirms the
      testnet target account, ED25519 algorithm, create-new mode-0600 path, and
      no-private-output behavior.
- [ ] Immediately before each testnet account creation, access-key addition or
      removal, and funding transaction, a human confirms a fresh CLI preview
      containing network, operation, signer, target account, public key and
      permission when applicable, and exact funding amount. Every funded
      preview also names the asset, payer, recipient, relayer or `none`, and
      maximum sponsored NEAR; no confirmation is reused after a command or
      field changes.
- [x] `x402-relayer.mike.testnet` exists with dedicated service and separate
      recovery keys and exactly 10 testnet NEAR initial funding. — 2026-07-19,
      Mike Purvis,
      [relayer provisioning evidence](evidence/2026-07-19-relayer-provisioning.md).
      Service key rotated 2026-07-22 to `ed25519:C577dij...` after the
      original was lost; account now holds exactly the recovery and new
      service keys.
- [x] Testnet config, Circle contract, RPC identity, exact merchant policy,
      budget, relayer key, and balance pass readiness. — 2026-07-23,
      Mike Purvis, [testnet go-live evidence](evidence/2026-07-23-testnet-golive.md)
- [x] Public testnet DNS/TLS, `/healthz`, `/readyz`, and `/supported` pass. —
      2026-07-23, Mike Purvis,
      [testnet go-live evidence](evidence/2026-07-23-testnet-golive.md)
- [ ] Immediately before the direct transfer, a human confirms
      `near:testnet`; the exact configured Circle test-USDC contract; 1,000
      atomic units; `merchant.mike.testnet`; `mike.testnet`; no relayer; and
      zero sponsored NEAR.
- [ ] That confirmed direct transfer succeeds and its transaction evidence is
      recorded.
- [ ] Immediately before the facilitator payment, a human confirms
      `near:testnet`; the same exact asset contract; 1,000 atomic units;
      `mike.testnet`; `merchant.mike.testnet`;
      `x402-relayer.mike.testnet`; and 0.01 NEAR maximum sponsorship
      reservation.
- [ ] That confirmed facilitator payment reaches final inner-token success;
      its exact recipient balance delta, journal result, and telemetry evidence
      are recorded.
- [ ] Replay creates no second transfer and returns the recorded outcome.
- [ ] Restart, RPC failover, low-balance, and external-monitor alert drills
      pass.
- [ ] Testnet service is enabled at boot only after all prior gates.

## Mainnet launch

- [ ] Known `mike.near` credential files are mode 0600, ownership is correct,
      and no suspected exposure requires rotation.
- [ ] Immediately before local service-key generation, a human confirms the
      mainnet target account, ED25519 algorithm, create-new mode-0600 path, and
      no-private-output behavior.
- [ ] Immediately before each mainnet account creation, access-key addition or
      removal, and funding transaction, a human confirms a fresh CLI preview
      containing network, operation, signer, target account, public key and
      permission when applicable, and exact funding amount. Every funded
      preview also names the asset, payer, recipient, relayer or `none`, and
      maximum sponsored NEAR; no confirmation is reused after a command or
      field changes.
- [x] `x402-relayer.mike.near` exists with dedicated service and separate
      recovery keys and exactly the approved 5 NEAR initial funding. —
      2026-07-19, Mike Purvis / FastNEAR,
      [relayer provisioning evidence](evidence/2026-07-19-relayer-provisioning.md)
- [ ] Mainnet config, Circle contract, RPC identity, exact `count.mike.near`
      policy, 0.50 NEAR global cap, 0.10 NEAR client cap, relayer key, and
      balance pass readiness.
- [ ] Public mainnet DNS/TLS, `/healthz`, `/readyz`, and `/supported` pass.
- [ ] Human confirms immediately before broadcast:
      `near:mainnet`; the exact configured Circle native-USDC contract; 1,000
      atomic units; `mike.near`; `count.mike.near`;
      `x402-relayer.mike.near`; 0.01 NEAR maximum reservation.
- [ ] Final mainnet token receipt, exact recipient balance delta, transaction
      hash, terminal journal response, actual sponsorship cost, and sanitized
      log evidence recorded.
- [ ] Mainnet replay proves one transfer and stable terminal response.
- [ ] Recovery, rollback, API-key revocation, and operator escalation drills
      pass.
- [ ] Mainnet service is enabled at boot after owner go/no-go review.

## Distribution evidence

- [ ] Public README, API documentation, threat model, runbook, and example
      resource workload match the deployed release.
- [ ] A real reference workload settles repeatedly and measurable activity is
      recorded without customer-identifying data.
- [ ] Repository, endpoints, release, owner, transaction evidence, current
      phase, and remaining work are added to the NEAR integrations coordination
      hub on a separate clean branch.
