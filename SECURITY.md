# Security policy

## Reporting a vulnerability

Please report suspected vulnerabilities through this repository's private
GitHub security-advisory channel. Do not open a public issue, include live
signed delegate actions, or send funded key material.

Include the affected version or commit, the smallest safe reproduction, the
expected security impact, and whether the issue may already have been
exploited. Use unfunded deterministic keys and redacted configuration in every
reproduction.

The maintainer will acknowledge a complete report within three business days,
coordinate remediation and disclosure with the reporter, and publish an
advisory for affected released versions. This response target is not a
guarantee of a fix timeline.

## Supported versions

Only the currently deployed release and the latest tagged release receive
security fixes.

## Dependency advisory triage

Advisories with no patched release are never left open and unexplained:
they are dismissed only together with a recorded reachability analysis,
kept here.

- **GHSA-848j-6mx2-7j84** (`elliptic`, low, disputed; no patched version
  exists — the vulnerable range is every release). Transitive only:
  `@near-js/crypto` (latest) → npm `secp256k1` → `elliptic` as its
  pure-JS fallback. Dismissed 2026-07-24 in both flagged lockfiles:
  `crates/x402-chain-near/fixtures/` is the CI-only interoperability
  oracle, which signs committed public test keys over fixed vectors (no
  adversarial input, no secret material); `examples/resource-server/`
  performs no local curve operations at all — request bodies are hashed
  with `node:crypto` and all payment verification is delegated to the
  Rust facilitator, which has no npm dependency surface. Revisit if
  either tree ever verifies signatures locally or handles secret key
  material in Node.

## Operational incidents

Availability, settlement reconciliation, relayer balance, or API-client
incidents that do not disclose a product vulnerability follow the private
operator escalation path in `docs/runbook.md`.
