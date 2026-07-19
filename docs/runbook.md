# Operations runbook

Owner: Mike Purvis / FastNEAR. Record a second incident contact before mainnet
launch. Commands below are examples for an operator who has already obtained
the necessary authorization; they do not authorize external changes.

## Mandatory mutation gate

This gate applies to testnet and mainnet. Immediately before every local
service-key generation or rotation, NEAR account creation, access-key
addition/removal, and funded transaction, show a fresh preview and receive an
explicit human confirmation. Execute the previewed action once. A changed
command, key, amount, account, or signed payload invalidates the confirmation;
an indeterminate result must be reconciled before any retry.

For a local key operation, preview the environment, target account, ED25519
algorithm, create-new mode-0600 output path, and whether private material can
appear in output. For an account or access-key operation, preview the network,
operation, signer, target account, public key, permission, and attached
funding. For every funded broadcast, including testnet, preview:

```text
network:
asset contract or native NEAR:
atomic amount:
payer:
recipient:
relayer (or "none" for a direct transaction):
expected maximum sponsored NEAR:
```

Keep only the confirmation and public transaction evidence. Never record a
private key, API key, or raw signed delegate in the change ticket.

## Known launch topology

- The intended host is the existing FastNEAR `fn-main-pro` machine
  (`core-m1`), Linux x86-64 with systemd and Nginx.
- Production runs native binaries. Docker and Podman are not installed on the
  host; the OCI image is a release artifact, not the deployment mechanism.
- `x402.fastnear.com` and `test.x402.fastnear.com` are not live until their
  launch checklist DNS gates are checked.
- Do not connect to or deploy through `fn-test-pro`: its observed ED25519 SSH
  fingerprint does not match the trusted entry. Resolve that discrepancy
  out-of-band rather than accepting a changed key.
- Mainnet and testnet run on the known main host as separate service users,
  ports, keys, configs, and databases.

## One-time prerequisites

### Relayer accounts

Create dedicated service-key files locally without printing private material,
then show only each public key during account creation:

- `x402-relayer.mike.testnet`, funded with 10 testnet NEAR;
- `x402-relayer.mike.near`, funded with 5 mainnet NEAR.

Generate each service credential into a new mode-0600 file:

```sh
x402-near-admin key generate-relayer --output /secure/path/relayer-key
```

The command prints only the public key. Refuse an existing output path rather
than overwriting or rotating implicitly. Apply the mandatory mutation gate
immediately before each invocation.

Use `near --help` for the installed CLI's exact account-creation syntax. Do not
place a key in a command-line argument. Add the corresponding Mike account's
public key as a separate FullAccess recovery key. Only the dedicated service
key may sign facilitator transactions; operators must never use it manually.
Preview and confirm each account creation, key addition/removal, and funding
broadcast separately unless the CLI presents one atomic transaction containing
the exact combined actions.

Immediately before initial funding, the funded previews are:

| Field | Testnet | Mainnet |
| --- | --- | --- |
| Network | `near:testnet` | `near:mainnet` |
| Asset | Native NEAR | Native NEAR |
| Amount | 10 NEAR (`10000000000000000000000000` yoctoNEAR) | 5 NEAR (`5000000000000000000000000` yoctoNEAR) |
| Payer | `mike.testnet` | `mike.near` |
| Recipient | `x402-relayer.mike.testnet` | `x402-relayer.mike.near` |
| Relayer | None; direct account funding | None; direct account funding |
| Maximum sponsored NEAR | 0 | 0 |

The CLI's own maximum transaction-fee preview is additional to these fields.
Pause for an environment-specific confirmation immediately before each
broadcast.

Before using `mike.near`, make its two known local credential copies mode 0600:

```sh
chmod 600 \
  "$HOME/.near-credentials/mainnet/mike.near.json" \
  "$HOME/.near-credentials/mike.near.json"
```

Confirm ownership and whether the workstation was ever multi-user. If exposure
is plausible, stop and rotate the root key instead of merely changing its mode.
Never install a Mike credential on the service host.

### Databases

Provision separate Neon mainnet and testnet databases with separate migration
and service roles. Save credentials directly into the operator's secret
manager. Do not reuse another FastNEAR database.

Apply forward-only migrations before a binary first uses the schema:

```sh
x402-near-admin migrate --database-url-file /secure/path/migration-database-url
```

The application URL uses the least-privileged service role. Confirm that it
cannot create/alter schema or roles and that leadership uses a session-pinned
direct connection when required.

For operational triage, provision a separate read-only observer login with
column-level access only to settlement state/timestamps/reason and global
sponsorship totals. It must not be able to select API-key digests, payment or
transaction hashes, account IDs, policies, terminal response bytes, or signed
transaction bytes. Do not give an operator the service DML role merely to
inspect readiness.

### Honeycomb

Create the FastNEAR Honeycomb environment and an ingest-only key. Provision the
OTLP authorization header as a systemd credential, configure
`service.name`, `service.version`, and `deployment.environment.name` resource
attributes, and create exactly these initial triggers:

1. mainnet readiness/5xx or settlement-error degradation;
2. low relayer balance, sponsorship budget over 80%, settlement pending over
   two minutes, or relayer quarantine.

Send a sanitized test event and verify that it contains no account ID, payment
identifier, transaction/delegate hash label, API key, raw payload, database
URL, or private key.

### TLS and DNS

Create a Cloudflare Origin CA certificate covering only
`x402.fastnear.com` and `test.x402.fastnear.com`, store its private key
root-only on the host, and configure the zone for Full (strict). A public
Let's Encrypt certificate is an acceptable alternative if renewal is
documented and tested.

Use a new least-privileged Cloudflare token that can edit DNS only in the
`fastnear.com` zone. Do not reuse a dashboard or processor token. Add proxied
records for both names to the main host only after the local origin checks
pass. Never commit the token or origin private key.

## Install a release

The attested release archive contains both binaries, migrations, license
notices, an embedded SBOM, and the reviewed `docs/` and `deploy/` trees. The
archive checksum and a second copy of the SBOM are sibling release artifacts;
provenance and the SBOM attestation cover the archive as a whole, including its
deployment tools and documentation. On an operator machine:

```sh
gh release download vX.Y.Z \
  --repo fastnear/x402-near-facilitator \
  --pattern 'x402-near-facilitator-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz*'
sha256sum --check \
  x402-near-facilitator-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz.sha256
gh attestation verify \
  x402-near-facilitator-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz \
  --repo fastnear/x402-near-facilitator
```

Inspect the archive member list and extract
`deploy/install-release.sh` from that verified archive. Copy the archive,
checksum, and extracted installer to the host. As root, copy the packaged
installer into a root-owned mode-0700 staging path before executing it; do not
run an installer from a Git checkout or a user-writable path. The installer
copies the archive and checksum exactly once into its own root-only staging
directory before parsing, hashing, inspecting, or extracting them. It refuses
an existing version.

```sh
sudo install -o root -g root -m 0700 \
  /path/to/packaged-install-release.sh \
  /root/x402-near-install-release-vX.Y.Z
sudo /root/x402-near-install-release-vX.Y.Z \
  /path/to/archive.tar.gz \
  /path/to/archive.tar.gz.sha256
(cd /opt/x402-near-facilitator/releases/vX.Y.Z && \
  sha256sum --check deploy-assets.sha256 && \
  sha256sum --check docs-assets.sha256)
```

Installation creates only
`/opt/x402-near-facilitator/releases/vX.Y.Z`. It does not change
`current-mainnet` or `current-testnet`, and it does not restart either
environment. Promotion is a separate, environment-specific step after host
compatibility and configuration checks.

## Configure the host

Before installing service files or changing a deployment pointer, record the
host baseline and compare it with the release build baseline:

```sh
uname -m
getconf GNU_LIBC_VERSION
systemd --version
nginx -v
/opt/x402-near-facilitator/releases/vX.Y.Z/x402-near-admin --version
```

Require x86-64, a compatible glibc ABI, and systemd support for every sandbox
and credential directive in the packaged unit. Reject the release if the
admin binary cannot execute. The packaged promotion tool separately executes
the service binary with `--version` before it changes an environment pointer;
that on-host ABI smoke check is a mandatory promotion gate.

Create system users once:

```sh
sudo useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin \
  x402-near-mainnet
sudo useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin \
  x402-near-testnet
sudo install -d -m 0755 /etc/x402-near-facilitator
sudo install -d -m 0700 \
  /etc/x402-near-facilitator/credentials/mainnet \
  /etc/x402-near-facilitator/credentials/testnet
```

Install reviewed JSON config files as root:service-group mode 0640. Install
pooled database URL, direct database URL, relayer key, HMAC pepper, and OTLP
header files as root:root mode 0600 under the matching credentials directory.
Validate that no credential is a symlink and none is reused by the other
environment.

Install and validate service and Nginx configuration:

```sh
release=/opt/x402-near-facilitator/releases/vX.Y.Z
sudo install -m 0644 \
  "$release/deploy/systemd/x402-near-facilitator@.service" \
  /etc/systemd/system/
sudo systemctl daemon-reload
sudo install -d -m 0755 /etc/nginx/snippets
sudo install -m 0644 "$release/deploy/nginx/x402-near-proxy.conf" \
  /etc/nginx/snippets/x402-near-proxy.conf
sudo install -m 0644 "$release/deploy/nginx/x402-near-facilitator.conf" \
  /etc/nginx/sites-available/x402-near-facilitator
sudo ln -s /etc/nginx/sites-available/x402-near-facilitator \
  /etc/nginx/sites-enabled/x402-near-facilitator
sudo systemd-analyze verify \
  /etc/systemd/system/x402-near-facilitator@.service
sudo nginx -t
sudo systemctl reload nginx
```

If the site symlink already exists, inspect it rather than replacing it.
Verify core dumps are disabled and that the settings resolve as expected after
each instance has been loaded:

```sh
systemctl show x402-near-facilitator@mainnet -p LimitCORE
systemctl show x402-near-facilitator@testnet -p LimitCORE
coredumpctl list x402-near-facilitator
```

Both `LimitCORE` values must be zero. Investigate and remove any credential
exposure risk before continuing if a service core exists; never attach a core
file to an issue.

## Testnet deployment

1. Confirm the config says `near:testnet`, port 8403, the test Circle asset,
   and `x402-relayer.mike.testnet`.
2. Confirm migrations, credential modes, RPC network identity, relayer public
   key, balance, and exact merchant policy.
3. Promote only testnet. The tool runs the native service binary's `--version`
   smoke check before atomically changing `current-testnet`; it does not touch
   `current-mainnet` or restart a service:

   ```sh
   sudo /opt/x402-near-facilitator/releases/vX.Y.Z/deploy/promote-release.sh \
     testnet vX.Y.Z
   readlink -f /opt/x402-near-facilitator/current-testnet
   ```

4. Start and inspect:

   ```sh
   sudo systemctl start x402-near-facilitator@testnet
   sudo systemctl status x402-near-facilitator@testnet --no-pager
   curl --fail --silent http://127.0.0.1:8403/healthz
   curl --fail --silent http://127.0.0.1:8403/readyz
   curl --fail --silent http://127.0.0.1:8403/supported
   ```

5. Run `scripts/verify-deployment.sh` from the matching source or release
   tooling against the public testnet URL.
6. Complete fixture, authentication, funded transfer, duplicate, restart,
   accepted-response-dropped, RPC failover, and telemetry gates.
7. Enable at boot only after those checks:

   ```sh
   sudo systemctl enable x402-near-facilitator@testnet
   ```

### Testnet funded acceptance

This sequence needs two separate, immediate confirmations.

For the direct transfer into `mike.testnet`, preview:

```text
network: near:testnet
asset: 3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af
amount: 1000 atomic USDC
payer: merchant.mike.testnet
recipient: mike.testnet
relayer: none (direct transaction)
maximum sponsored reservation: 0 NEAR
```

After confirmation, broadcast once and record the transaction and exact
recipient balance delta. Do not treat that confirmation as approval for the
facilitated return payment.

Construct the signed delegate that returns the funds, then immediately before
the facilitator can broadcast it preview:

```text
network: near:testnet
asset: 3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af
amount: 1000 atomic USDC
payer: mike.testnet
recipient: merchant.mike.testnet
relayer: x402-relayer.mike.testnet
maximum sponsored reservation: 0.01 NEAR
```

After a fresh confirmation, submit that exact signed delegate once. Any change
to its bytes invalidates the confirmation. Require final success of the inner
token receipt, the exact recipient balance delta, the terminal journal result,
budget reconciliation, and sanitized telemetry. Replay the identical request
and prove that no second transfer was created.

## Mainnet deployment

Mainnet remains stopped until every testnet gate and mainnet preflight gate has
dated evidence. Repeat config and credential checks, then promote only
mainnet. Promotion performs the on-host service-binary smoke check and leaves
the testnet pointer unchanged:

```sh
sudo /opt/x402-near-facilitator/releases/vX.Y.Z/deploy/promote-release.sh \
  mainnet vX.Y.Z
readlink -f /opt/x402-near-facilitator/current-mainnet
```

Start without enabling:

```sh
sudo systemctl start x402-near-facilitator@mainnet
sudo systemctl status x402-near-facilitator@mainnet --no-pager
curl --fail --silent http://127.0.0.1:8402/readyz
```

Before the 1,000-atomic-USDC acceptance payment, display and explicitly confirm:

```text
network: near:mainnet
asset: 17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1
amount: 1000 atomic USDC ($0.001)
payer: mike.near
recipient: count.mike.near
relayer: x402-relayer.mike.near
maximum sponsored reservation: 0.01 NEAR
```

That confirmation must occur immediately before submission, applies to one
exact delegate, and expires if any field or signed bytes change. An
indeterminate result is reconciled by its stored hash and exact bytes; never
sign a retry. Verify final token receipt, recipient balance delta, journal
terminal response, sponsorship reconciliation, and Honeycomb trace. Then test
replay without another broadcast. Only after evidence review:

```sh
sudo systemctl enable x402-near-facilitator@mainnet
```

## Routine checks

- Public `/readyz` is checked every 60 seconds.
- Honeycomb triggers cover availability and financial/reconciliation risk.
- Review daily sponsorship use and relayer balance; refills are manual.
- Review API clients, exact payee policy, and owner contacts monthly.
- Confirm backups/PITR and perform a sanitized restore drill quarterly.
- Patch the OS and rotate API, database, Honeycomb, and DNS credentials under
  documented maintenance windows.
- Confirm the recorded glibc/systemd baseline after host upgrades and rerun
  both binary `--version` smoke checks before the next promotion.
- Confirm `LimitCORE=0` remains effective and review `coredumpctl` after every
  crash or fault-injection exercise.
- Keep terminal journal detail online for at least 90 days and sanitized
  journald data for no more than 14 days. Settlement identity and delegate-hash
  rows must remain durable after any later archival so an old authorization
  can never become payable again. Never delete nonterminal rows. Configure and
  verify the host's `systemd-journald` `MaxRetentionSec=14day` policy before
  launch; this host-wide setting requires review alongside its other services.

Useful commands:

```sh
systemctl status 'x402-near-facilitator@*' --no-pager
journalctl -u x402-near-facilitator@mainnet --since '30 minutes ago'
journalctl -u x402-near-facilitator@testnet --since '30 minutes ago'
curl --fail --silent https://x402.fastnear.com/readyz
curl --fail --silent https://test.x402.fastnear.com/readyz
```

Do not paste full journal output into issues until it has been reviewed for
sensitive data.

### Sanitized journal triage

`x402-near-admin` deliberately has no journal dump or status command. Use its
documented `reconcile` command for recovery. For routine state counts, connect
with the observer login through a libpq service entry or another secret-file
mechanism that keeps credentials out of arguments, then run only the
column-level query that role is allowed to execute:

```sql
SELECT
    state,
    count(*) AS rows,
    max(extract(epoch FROM (now() - created_at)))::bigint
        AS oldest_age_seconds,
    max(reconciliation_attempts) AS max_reconciliation_attempts,
    max(last_reconciled_at) AS last_reconciled_at
FROM settlements
GROUP BY state
ORDER BY state;
```

For global financial triage, the observer may also read only
`usage_date`, `reserved_yocto_near`, `spent_yocto_near`, and `updated_at` from
`daily_global_sponsorship`. Do not broaden that login to raw settlement rows,
`api_keys`, or client/account identity fields. Store only the aggregate output
in an incident ticket.

## Incidents

### Readiness fails

Stop routing settlement if the failure is leadership, reconciliation,
quarantine, database, RPC disagreement, or low relayer balance. `/healthz`
remaining green does not make settlement safe. Inspect sanitized service logs
and the aggregate state query above; do not force readiness or invent an admin
inspection command.

### Settlement is indeterminate

Do not sign another transaction and do not use `near` CLI with the service
relayer key. Stop the service instance, preserve the database, and run the
existing `x402-near-admin reconcile --config <environment-config>` command
with that environment's systemd credential files while it alone can acquire
leadership. Before invoking an operator-directed reconciliation that may
rebroadcast, preview the original journaled network, asset, amount, payer,
recipient, relayer, and maximum sponsorship reservation and obtain a fresh
confirmation. Reconciliation performs these decisions:

1. validate that the stored bytes, hash, relayer, delegate, and journal row
   agree before trusting an RPC result;
2. query the exact stored hash on primary and independent backup RPCs;
3. accept a final outcome only when its transaction identity matches the
   stored transaction;
4. terminalize only a proven inner success, a typed on-chain failure, an
   authoritative transaction rejection, or an expired delegate whose hash is
   unknown on both RPCs while both relayer nonces remain unchanged;
5. keep pending, missing, structurally ambiguous, identity-mismatched, or
   RPC-ambiguous evidence nonterminal and readiness false;
6. quarantine when a relayer nonce advanced but the stored transaction remains
   unknown;
7. rebroadcast only the exact stored bytes, only while the delegate remains
   valid, and only after the dual-RPC nonce checks remain stable.

Restart the service only after the admin command releases leadership. Keep the
resource response unavailable until a terminal result is authoritative.

### Relayer key is suspected compromised

Disable settlement for that environment, revoke or quarantine the service key
using the separately held recovery key, retain journal/database evidence, and
fund no replacement until the incident boundary is understood. Create a new
service key and reconcile every nonterminal row before restoring readiness.

### API key is suspected compromised

Revoke the client immediately, disable its remaining sponsorship budget,
review its recent request and settlement records, notify its owner, and rotate.
Do not rotate the global pepper unless its secrecy is also in doubt; a pepper
rotation requires a controlled reissue of every client key.

### Database unavailable or restored

Settlement fails closed. Do not serve from an empty or stale replacement.
Restore to an isolated database, validate schema and settlement uniqueness,
reconcile all nonterminal transactions against both RPCs, then acquire
leadership before reopening readiness.

### RPC disagreement

Stop settlement and preserve both sanitized responses. A definitive final
transaction on either trusted provider is evidence; absence on one is not
proof of absence. Do not change provider or re-sign until the stored
transaction and relayer nonce are reconciled.

## Rollback

1. Stop the affected systemd instance.
2. Confirm the preceding release supports the current schema.
3. Run that installed release's packaged promotion tool for only the affected
   environment. It executes the prior service binary's on-host ABI smoke test
   before atomically changing `current-mainnet` or `current-testnet`:

   ```sh
   sudo /opt/x402-near-facilitator/releases/vPREVIOUS/deploy/promote-release.sh \
     testnet vPREVIOUS
   ```

   Substitute `mainnet` only when mainnet is the affected instance. Confirm
   the other environment's pointer did not change.
4. Start the instance and require startup reconciliation plus `/readyz`.
5. Run public smoke checks and one non-broadcast `/verify`.

Never roll back migrations destructively. If the prior binary is not compatible
with the current schema, roll forward with a fixed binary instead.
