# NEAR paid-work reference service

This runnable Express service protects `POST /work` with a 1,000-atomic-USDC
NEAR x402 payment. It uses the official x402 server middleware and NEAR scheme,
authenticates to this facilitator, and independently deduplicates the
`payment-identifier` before delivering work.

The in-memory delivery journal is intentionally a development example. A real
service must replace it with durable, transactional storage shared by every
resource-server instance.

## Run

```sh
npm ci
export FACILITATOR_URL=https://test.x402.mikedotexe.com
export FACILITATOR_API_KEY_FILE=/secure/path/test-resource-server-api-key
export NETWORK=near:testnet
export ASSET=3e2210e1184b45b64c8a434c0a7e7b23cc04ea7eb7a6c3c32520d03d4afcb8af
export PAY_TO=merchant.mike.testnet
npm start
```

`FACILITATOR_API_KEY_FILE` must be a mode-0600 regular file containing exactly
one newline-terminated key. The service never accepts the key directly in an
environment variable and never logs it.

An unpaid `POST /work` receives the normal x402 `402 Payment Required`
challenge. After a compatible NEAR client resubmits with a payment signature,
the endpoint settles through the configured facilitator and returns a
deterministic SHA-256 result. Replaying the same identifier and payment returns
the stored result without another settlement or another work execution. The
same identifier with another payload returns 409.

This example is a launch workload, not evidence that either planned hostname
is currently deployed.
