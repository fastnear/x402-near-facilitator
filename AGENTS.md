# Repository instructions

These instructions apply recursively to the entire repository. Do not add
nested `AGENTS.md` files.

## Purpose

Build and operate a narrowly scoped Rust facilitator for x402 v2 `exact`
Circle USDC payments on `near:testnet` and `near:mainnet`. Keep the reusable
NEAR mechanism separable from the production HTTP, policy, and persistence
boundary.

The authoritative behavior is, in order:

1. The upstream x402 v2 core and `exact` NEAR specifications.
2. Interoperability fixtures generated from a pinned official `@x402/near`.
3. This repository's documented launch policy.

Do not silently diverge from the first two to make a test or partner payload
pass. Record and resolve the incompatibility.

## Launch boundaries

- Support x402 v2, scheme `exact`, classic NEP-366, and NEP-141 only.
- Launch with the configured Circle USDC contract only.
- Pin one network per process. Never infer a network from a relayer key or
  accept a client-selected RPC endpoint.
- Permit only exact client/network/asset/payee policy rows. No wildcards.
- Reject native NEAR, multiple actions, non-`ft_transfer` calls, attached
  deposit other than 1 yoctoNEAR, gas over 30 TGas, FunctionCall or gas-key
  payer permissions, ML-DSA, and DelegateV2.
- Use a full-access Transaction V0 relayer at launch. Gas-key relayers require
  a separate protocol and security review.

## Settlement invariants

- Verify the signature before using the claimed payer for policy, telemetry,
  or responses.
- Read chain state at finality and pin a final block across preflight queries
  where the RPC permits it. Fail closed on unknown or ambiguous RPC results.
- `/settle` performs verification again after it owns the durable settlement
  claim and relayer lock.
- Hash the exact decoded signed delegate bytes with the documented domain
  prefix. A delegate is globally single-use in the settlement journal.
- Reserve sponsorship budget and claim settlement in the same database
  transaction.
- Serialize a relayer from the final nonce read through terminal outcome.
- Persist exact signed outer transaction bytes and hash before broadcast.
  Never create a replacement transaction for an indeterminate submission.
- Success requires the unique inner token receipt to finish with
  `SuccessValue`. Outer transaction or delegate-receipt success is insufficient.
- Never TTL-delete nonterminal settlement records. Reconcile them on startup
  before readiness becomes true.
- If the relayer nonce advanced while the stored transaction is unknown on
  both independent RPCs, quarantine the relayer and fail readiness.

## Security and privacy

- Never commit, print, log, trace, snapshot, fixture, or paste a real API key,
  funded private key, credentialed database URL, Honeycomb key, live signed
  delegate, or funded wallet credential.
- The sole key-material exception is the checked-in interoperability fixture
  generator: its deterministic public test keys must be labeled `DO NOT FUND`,
  used only for impossible/expired fixture accounts, and never reused outside
  fixture generation.
- Treat signed delegate payloads as sensitive bearer authorizations even after
  they expire. Persist only fields required for safe replay protection and
  reconciliation.
- Mark authentication headers as sensitive before tracing middleware runs.
- Metric labels must be bounded and low-cardinality. Account IDs, payment
  identifiers, transaction hashes, and delegate hashes are not labels.
- Show raw API key material exactly once at creation. Store an HMAC-SHA256
  digest using a separately provisioned server pepper and compare in constant
  time.
- Production secrets enter through systemd `LoadCredential` or an equivalent
  secret file. Production `.env` files are prohibited.
- Migrations use a separate privileged database role. The service role cannot
  create or alter schema.
- Do not reuse credentials, Cloudflare tokens, databases, or relayer keys from
  another FastNEAR service.

## Network and funds safety

Local tests, mocked RPC tests, read-only RPC calls, and test database work are
allowed. Do not create accounts, add keys, alter DNS, deploy services, issue
production API keys, or broadcast a transaction merely because a command or
script exists in this repository.

Before every funded broadcast, require an explicit human confirmation showing:

- network;
- asset contract and atomic amount;
- payer;
- recipient;
- relayer;
- expected maximum sponsored NEAR.

Mainnet confirmation must occur immediately before broadcast and cannot be
reused for a retry. An indeterminate broadcast is reconciled by its stored
hash; it is never retried by signing a new transaction.

## Engineering conventions

- Keep `#![forbid(unsafe_code)]` and workspace lints intact.
- Use typed protocol/RPC errors. Do not determine account, key, method, or
  receipt status by substring matching.
- Parse decimal token amounts as integers and reject lossy or permissive JSON.
- Keep protocol rejection separate from malformed HTTP, authentication,
  policy, quota, and infrastructure errors.
- Write forward-only SQL migrations. Production startup must not migrate.
- Every concurrency or recovery fix needs a deterministic regression test.
- Keep generated TypeScript oracle tooling in development/test scope; the
  production binary must not depend on Node.
- Update OpenAPI, configuration examples, runbook, and threat model when an
  externally visible behavior or operational dependency changes.

Run `./scripts/check.sh` and `git diff --check` before committing. Do not
describe a deployment, transaction, alert, or partner integration as complete
without a dated evidence link.
