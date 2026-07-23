# Configuration

Each process loads one non-secret JSON file using `--config` and reads secrets
from files. Checked-in examples live in `deploy/config/`.
Production files are installed as:

```text
/etc/x402-near-facilitator/mainnet.json
/etc/x402-near-facilitator/testnet.json
```

The service must reject startup when a required key is unknown, a number is
out of range, a network does not match its Circle asset, a public bind address
is configured for the native deployment, or a secret value is supplied inline.

## Secret file inputs

| Variable | Credential filename | Contents |
| --- | --- | --- |
| `DATABASE_URL_FILE` | `database-url` | PostgreSQL service-role URL for this environment |
| `DATABASE_DIRECT_URL_FILE` | `database-direct-url` | Direct PostgreSQL URL for session leadership; may equal the application URL only when it is already direct |
| `RELAYER_KEY_FILE` | `relayer-key` | Dedicated relayer service key; never the Mike recovery key |
| `API_KEY_PEPPER_FILE` | `api-key-pepper` | Random HMAC pepper independent of all API keys |
| `OTEL_EXPORTER_OTLP_HEADERS_FILE` | `otel-headers` | OTLP authorization header; absent at launch, added by a systemd drop-in only when telemetry is adopted |

Files must contain only the value, end with a newline, be owned by root, and be
mode 0600 before systemd imports them. The service should trim one terminal
newline, but no other whitespace. Secret values must never be accepted through
CLI arguments.

Telemetry export is disabled at launch: leave both OTLP inputs unset. If an
OTLP backend is adopted later, set its HTTPS endpoint, resource attributes
for `service.name=x402-near-facilitator`, `deployment.environment.name`, and
`service.version`, and repeat the sanitized-event verification before
production use. Never put a dataset name or API key in source-controlled
examples if it identifies a private environment.

## Environment isolation

The two example files intentionally differ in every value that can prevent a
cross-network mistake:

| Setting | Testnet | Mainnet |
| --- | --- | --- |
| Network | `near:testnet` | `near:mainnet` |
| Bind address | `127.0.0.1:8403` | `127.0.0.1:8402` |
| Relayer | `x402-relayer.mike.testnet` | `x402-relayer.mike.near` |
| Primary RPC | `rpc.testnet.fastnear.com` | `rpc.mainnet.fastnear.com` |
| Backup RPC | `archival-rpc.testnet.fastnear.com` | `archival-rpc.mainnet.fastnear.com` |
| Global daily cap | 2 NEAR | 0.50 NEAR |
| Default client cap | 1 NEAR | 0.10 NEAR |
| Balance warning | 2 NEAR | 1 NEAR |
| Hard stop | 0.50 NEAR | 0.25 NEAR |

All NEAR-denominated configuration is expressed as decimal yoctoNEAR strings,
not floating point. Circle USDC quantities are decimal atomic-unit strings.
The launch minimum is 1,000 atomic USDC.

## Database roles

Create independent mainnet and testnet databases in the launch host's
loopback-only PostgreSQL cluster. Each environment has:

- an owner/migration role used only by `x402-near-admin migrate`;
- a service role with connect and DML privileges on the facilitator schema,
  but no schema creation, alteration, role management, or cross-database
  access;
- an operator-observer role with column-level read access only to sanitized
  settlement state/timestamps/reasons and global sponsorship totals. It has no
  access to client/account identities, hashes, payload or transaction bytes,
  terminal response bodies, or API-key data.

Both URL files may contain the same direct localhost URL: there is no
connection pooler, so the application connection already satisfies the
session-pinned leadership requirement. Do not reuse a database or role from
any other service.

## Validation before service start

The effective configuration check must confirm:

- config and each credential file are readable by the service;
- the database schema version is compatible, without applying migrations;
- the advisory leadership connection can remain session-pinned;
- primary and backup RPCs report the configured network and final blocks;
- configured asset, relayer, and minimum amount match the environment;
- the relayer key belongs to the configured account and is FullAccess;
- at least one API client is active;
- recipient policies exist for every enabled API client;
- the relayer is not quarantined and its balance is above the hard stop;
- nonterminal settlement reconciliation has completed.

Only then may `/readyz` return 200.
