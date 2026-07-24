# Launch checklist

Unchecked items are not claims of completion. Add a date, owner, and durable
evidence link beside every checked gate. A specification, local test, branch,
or conversation alone is not launch evidence.

## Ownership and external prerequisites

- [ ] Operational owner and second incident contact recorded. — 2026-07-23:
      owner Mike Purvis / FastNEAR recorded across ops docs; the **second
      incident contact is deliberately deferred (accepted risk, solo
      operator)** — the only item keeping this box open
      ([operational hardening](evidence/2026-07-23-operational-hardening.md)).
- [x] Mainnet and testnet PostgreSQL databases on the launch host bind only
      loopback, with separate migration and service roles, a nightly dump
      timer with off-host copy, and a tested restore procedure. — 2026-07-23;
      loopback, roles, the nightly `x402-near-backup.timer` pushing to a
      hardened S3 bucket via a least-privilege instance role, and a passing
      restore drill, [operational hardening](evidence/2026-07-23-operational-hardening.md).
- [x] Environment-specific observer logins can read only sanitized settlement
      state/timestamp/reason columns and global sponsorship totals; they cannot
      read identities, hashes, payload bytes, terminal bodies, or API-key data.
      — 2026-07-23, boundary verified,
      [operational hardening](evidence/2026-07-23-operational-hardening.md).
- [x] Telemetry export confirmed disabled (no OTLP endpoint or header
      credential installed); sanitized journald output verified for both
      environments. — 2026-07-23.
- [x] Route 53 records for both launch hostnames point only at the intended
      host; the change batch was previewed and confirmed, and the DNS-editing
      AWS credential remains only on the operator workstation. — 2026-07-22,
      Mike Purvis,
      [EC2 host and DNS repoint evidence](evidence/2026-07-22-ec2-host-and-dns-repoint.md)
- [x] One publicly trusted certificate covers exactly both launch hostnames;
      automated renewal and its Nginx reload hook are verified. — 2026-07-22,
      Let's Encrypt, certbot.timer active,
      [testnet go-live](evidence/2026-07-23-testnet-golive.md).
- [x] External verification shows both hostnames serve only packaged
      endpoints over TLS, plain HTTP redirects to HTTPS, and unknown-hostname
      or bare-IP requests are refused. — 2026-07-23 (also verified over IPv6).
- [x] External `/readyz` checks configured for both hostnames. — 2026-07-23,
      AWS Route 53 health checks (30-second interval) with CloudWatch alarms
      to SNS, [operational hardening](evidence/2026-07-23-operational-hardening.md).
      The alert email subscription awaits its one-time confirmation click.
- [ ] Changed `fn-test-pro` SSH key investigated out-of-band; no key was
      accepted automatically. This host is not a launch dependency. —
      Superseded: launch runs on the dedicated EC2 host
      `i-0537770b34b04b820` migrated to 2026-07-22, not the former shared
      host, so this is no longer a launch dependency
      ([EC2 host and DNS repoint](evidence/2026-07-22-ec2-host-and-dns-repoint.md)).

## Repository and supply chain

- [x] Public `fastnear/x402-near-facilitator` repository exists with Apache-2.0
      license, branch protection, security contact, and no secrets in history.
      — 2026-07-24: public repo, Apache-2.0 (`LICENSE`), protected `main`,
      `SECURITY.md` security contact.
- [x] The Rust toolchain (pinned to 1.93 by `rust-toolchain.toml`; container
      build image `rust:1.97-bookworm` per `Dockerfile`), `Cargo.lock`, x402-rs
      crate versions (`x402-types` / `x402-facilitator-local` `=2.0.2`), NEAR
      crate versions (`near-*` `=0.37.1`, `near-jsonrpc-client` `=0.22.0`), and
      official `@x402/near@2.19.0` oracle version are pinned. — 2026-07-24.
- [x] Formatting, Clippy with warnings denied, all tests, migration test,
      oracle diff, `cargo deny`, and `cargo audit` pass in CI. — 2026-07-23:
      enforced on every push and on v0.1.3 by `scripts/check.sh` and
      `.github/workflows/ci.yml`.
- [x] Tagged release contains both x86-64 binaries, checksum, SBOM, provenance,
      migrations, the reviewed `docs/` and `deploy/` trees, and a GHCR image.
      — 2026-07-23, v0.1.3 with checksum, SBOM, provenance/SBOM attestations,
      and GHCR image
      ([evidence](evidence/2026-07-23-v013-rollback-and-alerting.md)).
- [x] The archive checksum and GitHub attestation were verified before any
      member was executed; the root installer used the packaged, attested copy
      rather than a mutable checkout, and the installed `deploy/` and `docs/`
      asset manifests pass. — 2026-07-23: `gh attestation verify` before
      staging; packaged installer; on-host asset manifests pass
      ([evidence](evidence/2026-07-23-testnet-golive.md)).

## Protocol and security acceptance

All boxes in this section are gated by CI (`scripts/check.sh` +
`.github/workflows/ci.yml`, including the pinned TypeScript interoperability
oracle) on every push and on the v0.1.3 release tag; the named tests are the
durable coverage.

- [x] ED25519 and SECP256K1 fixtures interoperate with pinned
      `@x402/near@2.19.0`. — 2026-07-23: `tests.rs::decodes_typescript_ed25519_and_secp256k1_fixtures`
      against `fixtures/signed-delegates.json` (both curves) + the CI oracle job.
- [x] Version, network, scheme, requirement, base64, Borsh/trailing-byte,
      curve, DelegateV2, action, method, JSON, token, payee, amount, deposit,
      gas, timeout, nonce, permission, account, code, balance, and storage
      negatives pass. — 2026-07-23: the `x402-chain-near/src/tests.rs`
      negative-path suite (canonical-first-failure ordering, unsafe action
      shapes, expiry/nonce/block boundaries, typed access-key/account
      failures, balance/storage preflights).
- [x] Every RPC error path fails closed using typed errors. — 2026-07-23:
      `rpc.rs` (`NearRpcError`) + the `typed_*` failure tests.
- [x] Receipt tests cover inner success, outer-only success, inner failure,
      missing/ambiguous token receipt, and pending status. — 2026-07-23:
      the `receipt_graph_*` tests in `tests.rs` over `receipt.rs`.
- [x] Official TypeScript `HTTPFacilitatorClient` accepts `/supported`,
      `/verify`, `/settle`, and canonical failure/omission shapes. — 2026-07-23:
      `conformance/http-client/check.mjs` (`@x402/core@2.19.0`) +
      `service_http_tests.rs::custom_http_surface_matches_x402_contract`.
- [x] API-key header and Bearer alternatives pass; identical dual credentials
      pass; missing, invalid, revoked, and non-identical dual credentials fail
      without leaking comparison state. — 2026-07-23: `auth.rs` tests +
      `service_http_tests.rs` revoke path.
- [x] `payment-identifier` same-ID/same-fingerprint replay, in-flight join,
      different-fingerprint 409, and no-ID delegate duplicate pass. — 2026-07-23:
      `store_postgres_tests.rs::identifier_conflicts_and_delegate_duplicates_do_not_reserve_twice`,
      `::lifecycle_terminalization_and_replay_are_durable_and_idempotent`.
- [x] Body/content-type, rate, timeout, concurrency, minimum amount, exact
      payee, budget, and low-balance controls pass. — 2026-07-23: `config.rs`
      bound tests, `service_recovery_tests.rs::hard_balance_stop_prevents_preparation_and_broadcast`,
      `store_postgres_tests.rs::client_budget_failure_rolls_back_global_reservation_atomically`,
      nginx body limit.
- [x] Fuzz and redaction checks find no panic or sensitive output. — 2026-07-23:
      three `fuzz/fuzz_targets/*` + `fuzz.yml`;
      `tests.rs::delegate_debug_output_redacts_payment_material`.

## PostgreSQL and recovery acceptance

The behavioral boxes are gated by the PostgreSQL integration tests in CI on
v0.1.3 (named below); the restore box is a live drill.

- [x] Two competing instances prove exactly one advisory-lock leader. —
      2026-07-23: `leadership_postgres_tests.rs::competing_instances_have_exactly_one_leader_and_fail_over`.
- [x] Hundreds of concurrent identical settlements create one prepared outer
      transaction and at most one broadcast. — 2026-07-23:
      `store_postgres_tests.rs::two_hundred_identical_claims_create_one_reservation`.
- [x] Concurrent distinct settlements serialize relayer nonce use. — 2026-07-23:
      `service_recovery_tests.rs::concurrent_distinct_settlements_serialize_unique_relayer_nonces`.
- [x] `submitted` is durable before broadcast, and a crash after every journal
      transition recovers without re-signing. — 2026-07-23:
      `service_recovery_tests.rs::crash_restart_matrix_recovers_each_durable_transition_exactly_once`
      + `assert_submitted_before_broadcast`.
- [x] Accepted-but-response-dropped submission resolves by stored hash. —
      2026-07-23: `service_recovery_tests.rs::accepted_response_drop_recovers_without_second_transaction`.
- [x] Primary unknown/backup final and primary/backup disagreement paths pass.
      — 2026-07-23: `service_recovery_tests.rs::backup_final_result_recovers_when_primary_is_unknown`,
      `::conflicting_primary_and_backup_finals_fail_closed` (live operational
      complement drilled 2026-07-24,
      [RPC resilience](evidence/2026-07-24-rpc-resilience-and-host-verification.md)).
- [x] Expired unknown transactions fail without rebroadcast; pending,
      identity-mismatched, missing-receipt, and ambiguous outcomes remain
      nonterminal and fail readiness. — 2026-07-23: the
      `service_recovery_tests.rs` expiry/nonterminal suite
      (`expired_prepared_and_submitted_rows_never_rebroadcast`, and the
      incomplete-receipt / wrong-identity / pending cases).
- [x] Unknown hash with advanced relayer nonce quarantines the key and fails
      readiness. — 2026-07-23: `service_recovery_tests.rs::both_unknown_with_advanced_backup_nonce_quarantines_relayer`,
      `::quarantined_relayer_policy_prevents_preparation_and_broadcast`.
- [x] Revocation, payee-policy failure, budget exhaustion, lost leadership,
      database failure, and stale reservation release pass. — 2026-07-23:
      `admin_cli.rs` lifecycle + `store`/`store_postgres_tests.rs` payee-policy
      and budget-rollback + `leadership_postgres_tests.rs` fail-over +
      disconnected-store and reconciliation/expiry tests.
- [x] Budget reservation and settlement claim rollback atomically on failure.
      — 2026-07-23: `store_postgres_tests.rs::client_budget_failure_rolls_back_global_reservation_atomically`.
- [x] Database restore drill preserves uniqueness and reconciles all
      nonterminal rows. — 2026-07-23: restore drill; all tables and both unique
      constraints restored, duplicate insert rejected
      ([operational hardening](evidence/2026-07-23-operational-hardening.md)).

## Host hardening

Host-state boxes verified live 2026-07-24
([host verification](evidence/2026-07-24-rpc-resilience-and-host-verification.md), §2).

- [x] Separate `x402-near-mainnet` and `x402-near-testnet` users have no login
      shell or home. — 2026-07-24: both `home=/nonexistent
      shell=/usr/sbin/nologin`.
- [x] Versioned release is root-owned; `current-mainnet` and
      `current-testnet` are the only mutable pointers, installation changes
      neither, and each environment is promoted separately. — 2026-07-23:
      `releases/v0.1.3` root-owned; atomic pointer swap leaves the other
      environment untouched
      ([evidence](evidence/2026-07-23-v013-rollback-and-alerting.md);
      `deploy/promote-release.sh`).
- [x] Host glibc, architecture, kernel, systemd, and Nginx versions are
      recorded and compatible with the release build baseline; the packaged
      binary's on-host `--version` ABI smoke check passes before each
      environment pointer changes. — 2026-07-24: kernel `6.17.0-1019-aws`,
      `x86_64`, glibc `2.39`, systemd `255`, nginx `1.24.0`; ABI smoke check
      per go-live records.
- [x] Config is non-secret; credentials are separate root-owned mode-0600
      regular files and are not reused across environments. — 2026-07-24: all
      ten `/etc/x402-near-facilitator/credentials/{mainnet,testnet}/*` files
      are `600 root:root`; `deploy/config/*.json.example` are non-secret.
- [x] systemd sandbox settings and writable paths were reviewed with
      `systemd-analyze security`. — 2026-07-24: overall exposure **1.5 OK**
      for both units.
- [x] `LimitCORE=0` is effective for both units and `coredumpctl` contains no
      service core dumps from acceptance or fault testing. — 2026-07-24:
      `LimitCORE=0` in the unit; both PIDs show `Max core file size 0 0`;
      `coredumpctl` lists no x402 dumps.
- [x] Nginx binds public TLS; services bind only loopback ports 8402/8403. —
      2026-07-24: services listen only on `127.0.0.1:8402`/`8403`; Nginx
      terminates public TLS (`nginx/x402-near-facilitator.conf`).
- [x] Nginx limits bodies to 64 KiB, preserves canonical application errors,
      disables CORS, and does not log authentication headers or bodies. —
      `nginx/x402-near-facilitator.conf` (`client_max_body_size 64k`,
      canonical 413 JSON, CSP `default-src 'none'`, `combined` access log).
- [x] Origin access, OS firewall, clock sync, disk space, and host patching
      checked; detailed terminal records have at least 90-day retention,
      durable settlement identities are never recycled, and sanitized
      journald plus dedicated Nginx retention is verified at no more than
      14 days. — 2026-07-24: security-group firewall; NTP synchronized; root
      12% used / 25 G free; `unattended-upgrades` enabled; Nginx logrotate
      `rotate 13`/`maxage 14`; 90-day terminal retention and non-recycled
      identities per `docs/architecture.md`.
- [x] Binary rollback drill succeeds without a schema rollback. —
      2026-07-23, testnet v0.1.3→v0.1.2→v0.1.3, 4 s stop-to-ready each way
      ([evidence](evidence/2026-07-23-v013-rollback-and-alerting.md))

## Testnet launch

- [x] Immediately before local service-key generation, a human confirms the
      testnet target account, ED25519 algorithm, create-new mode-0600 path, and
      no-private-output behavior. — 2026-07-19, and enforced by
      `admin_cli.rs::generate_relayer_is_create_new_mode_0600_and_never_prints_private_material`
      ([relayer provisioning](evidence/2026-07-19-relayer-provisioning.md)).
- [x] Immediately before each testnet account creation, access-key addition or
      removal, and funding transaction, a human confirms a fresh CLI preview
      containing network, operation, signer, target account, public key and
      permission when applicable, and exact funding amount. Every funded
      preview also names the asset, payer, recipient, relayer or `none`, and
      maximum sponsored NEAR; no confirmation is reused after a command or
      field changes. — 2026-07-19 / 2026-07-23, field-by-field previews
      ([relayer provisioning](evidence/2026-07-19-relayer-provisioning.md);
      [real traffic](evidence/2026-07-23-real-traffic-and-recovery.md)).
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
- [x] Immediately before the direct transfer, a human confirms
      `near:testnet`; the exact configured Circle test-USDC contract; 1,000
      atomic units; `merchant.mike.testnet`; `mike.testnet`; no relayer; and
      zero sponsored NEAR. — 2026-07-23, Mike Purvis
- [x] That confirmed direct transfer succeeds and its transaction evidence is
      recorded. — 2026-07-23,
      [testnet go-live evidence](evidence/2026-07-23-testnet-golive.md)
      (tx `GjUHrMfYm2UUXaqQaLED1KSuwjKr7P35XCFhkXFQMzkF`)
- [x] Immediately before the facilitator payment, a human confirms
      `near:testnet`; the same exact asset contract; 1,000 atomic units;
      `mike.testnet`; `merchant.mike.testnet`;
      `x402-relayer.mike.testnet`; and 0.01 NEAR maximum sponsorship
      reservation. — 2026-07-23, Mike Purvis
- [x] That confirmed facilitator payment reaches final inner-token success;
      its exact recipient balance delta, journal result, and telemetry evidence
      are recorded. — 2026-07-23,
      [testnet go-live evidence](evidence/2026-07-23-testnet-golive.md)
      (tx `FHuswy7QNXc1T1nHHR5jT55f8UML8rNW2iwmDnsrzgdP`, SuccessValue,
      0.000331 NEAR sponsored)
- [x] Replay creates no second transfer and returns the recorded outcome. —
      2026-07-23, `duplicate_settlement` with unchanged relayer balance
      ([evidence](evidence/2026-07-23-testnet-golive.md))
- [x] Restart, RPC-outage fail-closed/recovery, low-balance, and
      external-monitor alert drills pass. — Restart and low-balance drills
      2026-07-23 ([operational hardening](evidence/2026-07-23-operational-hardening.md));
      alert delivery proven for every alarm plus the OnFailure path
      ([alerting](evidence/2026-07-23-v013-rollback-and-alerting.md));
      RPC-outage fail-closed + in-place recovery drilled 2026-07-24, mainnet
      isolation confirmed
      ([RPC resilience](evidence/2026-07-24-rpc-resilience-and-host-verification.md)).
      (No automatic hot-path RPC failover exists by design; the backup RPC is
      the reconciliation cross-check substrate — see the evidence note.)
- [x] Testnet service is enabled at boot. — 2026-07-23, after funded
      acceptance; all restart/RPC/low-balance/monitor drills above now pass.

## Mainnet launch

- [x] Known `mike.near` credential files are mode 0600, ownership is correct,
      and no suspected exposure requires rotation. — 2026-07-24:
      `~/.near-credentials/mainnet/mike.near.json` and
      `x402-relayer2.mike.near.json` both mode `0600`; the lost original
      relayer keys were never exposed
      ([mainnet go-live](evidence/2026-07-23-mainnet-golive.md);
      [host verification](evidence/2026-07-24-rpc-resilience-and-host-verification.md)).
- [x] Immediately before local service-key generation, a human confirms the
      mainnet target account, ED25519 algorithm, create-new mode-0600 path, and
      no-private-output behavior. — 2026-07-23, generated locally with no
      private print, mode-0600, ED25519
      ([mainnet go-live](evidence/2026-07-23-mainnet-golive.md)).
- [x] Immediately before each mainnet account creation, access-key addition or
      removal, and funding transaction, a human confirms a fresh CLI preview
      containing network, operation, signer, target account, public key and
      permission when applicable, and exact funding amount. Every funded
      preview also names the asset, payer, recipient, relayer or `none`, and
      maximum sponsored NEAR; no confirmation is reused after a command or
      field changes. — 2026-07-23, each broadcast previewed and confirmed
      ([mainnet go-live](evidence/2026-07-23-mainnet-golive.md);
      [real traffic](evidence/2026-07-23-real-traffic-and-recovery.md)).
- [x] The mainnet relayer exists with dedicated service and separate recovery
      keys. The original `x402-relayer.mike.near` (2026-07-19) became
      unrecoverable — its keys were lost and 5 NEAR is locked — so a fresh
      `x402-relayer2.mike.near` subaccount of `mike.near` was created
      2026-07-23 (5 NEAR, service key preserved in the credential store,
      `mike.near` recovery key), [mainnet go-live
      evidence](evidence/2026-07-23-mainnet-golive.md).
- [x] Mainnet config, Circle contract, RPC identity, exact `count.mike.near`
      policy, 0.50 NEAR global cap, 0.10 NEAR client cap, relayer key, and
      balance pass readiness. — 2026-07-23 (`/readyz` true).
- [x] Public mainnet DNS/TLS, `/healthz`, `/readyz`, and `/supported` pass. —
      2026-07-23, [evidence](evidence/2026-07-23-mainnet-golive.md)
- [x] Human confirms immediately before broadcast:
      `near:mainnet`; the exact configured Circle native-USDC contract; 1,000
      atomic units; `mike.near`; `count.mike.near`;
      `x402-relayer2.mike.near`; 0.01 NEAR maximum reservation. — 2026-07-23
- [x] Final mainnet token receipt, exact recipient balance delta, transaction
      hash, terminal journal response, actual sponsorship cost, and sanitized
      log evidence recorded. — tx
      `3KpKfbGcgKTsnbF9cj9y6Eh3oRM2yLdSfXXxV6RPgQrs`, SuccessValue,
      0.000334 NEAR ([evidence](evidence/2026-07-23-mainnet-golive.md))
- [x] Mainnet replay proves one transfer and stable terminal response. —
      2026-07-23, `duplicate_settlement`, unchanged relayer balance.
- [x] Recovery, rollback, API-key revocation, and operator escalation drills
      pass. — Rollback drill 2026-07-23
      ([evidence](evidence/2026-07-23-v013-rollback-and-alerting.md));
      API-key revocation drill
      ([evidence](evidence/2026-07-23-operational-hardening.md));
      indeterminate-settlement recovery drill 2026-07-23, including a true
      broadcast-then-crash recovery with no rebroadcast
      ([evidence](evidence/2026-07-23-real-traffic-and-recovery.md));
      operator escalation is the documented solo self-escalation path
      (second contact deliberately deferred, accepted risk).
- [x] Mainnet service is enabled at boot after owner go/no-go review. —
      2026-07-23, after funded acceptance.

## Distribution evidence

- [x] Public README, API documentation, threat model, runbook, and example
      resource workload match the deployed release. — 2026-07-23: both
      environments run v0.1.3; the public demo workload runs the example
      from the v0.1.3 source tag; docs changed since the tag are evidence
      and checklist records only.
- [x] A real reference workload settles repeatedly and measurable activity is
      recorded without customer-identifying data. — 2026-07-23, three
      public testnet settles and two mainnet settles with replay and
      conflict proofs
      ([evidence](evidence/2026-07-23-real-traffic-and-recovery.md))
- [ ] Repository, endpoints, release, owner, transaction evidence, current
      phase, and remaining work are added to the NEAR integrations coordination
      hub on a separate clean branch.

## Post-launch decisions

- [x] NEAR Intents (`assetTransferMethod: "intents"`) adoption decided —
      2026-07-24: carried by a future mainnet-only **sibling service**,
      never by relaxing this service's invariants; upstream spec PR
      opened (x402-foundation/x402#2948); engineering gates G1–G5 open in
      [near-intents-adoption-gates.md](near-intents-adoption-gates.md).
