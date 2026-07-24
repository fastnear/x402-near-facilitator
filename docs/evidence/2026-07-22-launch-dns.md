# Launch DNS provisioning evidence — 2026-07-22

Owner: Mike Purvis

This record covers only the creation of the two public launch DNS records.
It is not evidence that TLS, either facilitator service, a release, a
database, an API client, or any funded flow is live. Relayer provisioning
evidence remains a separate document.

## Decision

The launch host is the known main host (`fn-main-pro`, `core-m1`) at
`65.108.111.182`. The exact change batch below was previewed and explicitly
confirmed by the owner before execution. Migration to personally owned
infrastructure remains an open post-launch option; because the database is
external, it would consist of a host reinstall plus a DNS repoint.

## Change

- Hosted zone: `mikedotexe.com.` (`ZEBBWSGTKUUP6`) in personal AWS account
  `341982967115`.
- Route 53 change `C0039768197E3YLTNTSPJ`, submitted 2026-07-22T16:53:51Z,
  observed `INSYNC` within minutes of submission.
- Batch contents, both `CREATE` actions, which fail closed if a name
  already exists:
  - `x402.mikedotexe.com.` `A` → `65.108.111.182`, TTL 300
  - `test.x402.mikedotexe.com.` `A` → `65.108.111.182`, TTL 300
- AAAA records are deferred until the host's IPv6 address is confirmed.

## Verification

Queried immediately after submission, before `INSYNC` was observed:

- Authoritative `ns-799.awsdns-35.net` answered `65.108.111.182` for both
  names.
- Public resolver `1.1.1.1` answered `65.108.111.182` for both names.

## Credential scope

DNS is edited only from the operator workstation with the dedicated
`for-easy-dns` IAM user; no AWS credential exists on the service host. Zone
writes were denied until 2026-07-22, when the owner attached an inline
policy (named `route53-stuff`) to that user; this change and its
`GetChange` polling exercised exactly the added permissions.

## Remaining launch gates

No TLS certificate is issued for either name, the origin virtual hosts are
not installed, and neither hostname serves HTTPS as of this evidence date.
Certificate issuance and renewal verification, external direct-origin
verification, readiness, and every later checklist gate remain unchecked.
