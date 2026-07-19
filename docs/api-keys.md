# API key administration

API keys authenticate resource servers to `/verify` and `/settle`. They are not
payer credentials and do not authorize a payee outside their exact database
policy.

Clients should send `X-API-Key`. `Authorization: Bearer` is an alternative.
When a compatibility layer sends both headers, the service accepts the request
only when both carry the identical raw key; different values receive the same
401 response as any other credential conflict.

## Format and storage

Keys have one of these forms:

```text
x402_test_<public-id>.<secret>
x402_live_<public-id>.<secret>
```

The public ID is 12 random bytes and the secret is 32 random bytes; both are
lowercase hexadecimal (24 and 64 characters, respectively). The public ID and
a short display prefix may be stored in clear text. The full raw
key is displayed once by `x402-near-admin`; the database stores only an
HMAC-SHA256 digest produced with the separately provisioned service pepper.

Do not pass a raw key as a command-line argument. Write it directly to the
client's secret manager or a mode-0600 file through a pipe or password-manager
integration that does not echo. Never put it in shell history, issue text,
chat, screenshots, test fixtures, or Honeycomb.

## Required policy

Before a key is enabled, record:

- human-readable client name and operational owner;
- environment and status;
- exact `network`, `asset`, and `pay_to` rows;
- verify and settle rate limits;
- daily sponsorship limit;
- expiration or review date;
- incident and revocation contact.

The facilitator database enforces the client name, environment, status,
allowlist, limits, and budget. Owner/contact/review metadata and each operator
action belong in FastNEAR's access registry or ticketing system; they are not
stored in the settlement database. Link that external audit record to the
client UUID without copying the raw key.

The initial launch policies are:

| Environment | Network | Asset | Payee |
| --- | --- | --- | --- |
| Testnet | `near:testnet` | Circle test USDC contract | `merchant.mike.testnet` |
| Mainnet | `near:mainnet` | Circle native USDC contract | `count.mike.near` |

No wildcard fields are allowed.

## Lifecycle

The administrative interface must support these operations without returning a
stored secret:

```text
x402-near-admin client create
x402-near-admin client allow-payee
x402-near-admin client set-budget
x402-near-admin client rotate
x402-near-admin client revoke
```

Exact flags are discoverable with `--help`; database and pepper inputs come
from secret files. A create operation emits the new key once. Rotation
atomically revokes every active key for that client, inserts the replacement,
and emits the replacement once. The operator must record create, policy,
budget, rotation, and revocation actions in the external access registry
without the raw key. Revocation is immediate for new requests; in-flight
settlement reconciliation continues by journal identity, not API-key validity.

Rotation procedure:

1. Confirm the client owner and exact policy.
2. Coordinate a brief cutover with the client owner.
3. Rotate the credential and transfer the replacement out of band.
4. Verify one authenticated `/verify` request with the replacement.
5. Confirm the old key receives 401 and the new key retains the same exact
   payee and budget policy.
6. Record date, operator, client ID, and evidence without either raw key.

Suspected compromise requires immediate revocation, review of recent request
and settlement IDs, budget disablement if necessary, and incident handling
from [the runbook](runbook.md).
