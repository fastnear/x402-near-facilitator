# Demo resource workload deployment

Deploys `examples/resource-server` on the facilitator host as the real
reference workload behind `x402-demo.mikedotexe.com` (mainnet) and
`x402-demo-test.mikedotexe.com` (testnet). It exercises the full paid
flow — `402 Payment Required` → signed payment → facilitator settle →
deterministic work result — against the production facilitator endpoints
with real (tiny) USDC amounts.

Known limitation, accepted for the demo: the example's delivery journal is
in-memory (per its README, production resource servers need durable
transactional storage). A restart clears the delivered-work cache; the
facilitator's own settlement idempotency is unaffected.

## Install outline

1. **DNS**: A/AAAA records for both demo hostnames to the facilitator host
   (Route 53; apply the mutation gate — preview the change batch first).
2. **TLS**: `certbot certonly --webroot -w /var/www/x402-near-acme
   -d x402-demo.mikedotexe.com -d x402-demo-test.mikedotexe.com` after the
   port-80 vhosts in `x402-demo.conf` are enabled (they serve the shared
   ACME webroot). The monitoring metrics script picks up the new lineage
   automatically; add a matching `CertDaysRemaining` alarm for it.
3. **Runtime**: install Node.js LTS from NodeSource. Copy
   `examples/resource-server/` (from the deployed release's source tag) to
   `/opt/x402-demo/app`, root-owned, then `npm ci --omit=dev` there.
4. **Users**: `x402-demo-testnet` and `x402-demo-mainnet` system users, no
   login shell.
5. **Facilitator clients**: create one dedicated client per network with a
   small daily budget and an exact payee allowlist row matching the env
   file's `NETWORK`/`ASSET`/`PAY_TO`:

   ```sh
   x402-near-admin client create --name demo-resource-server \
     --environment <env> --daily-budget-yocto-near <budget>
   x402-near-admin client allow-payee --client-id <id> \
     --network near:<env> --asset <usdc-account> --pay-to <merchant>
   ```

   Save each printed key to
   `/etc/x402-demo/credentials/<env>/api-key` (mode 0600, owned by the
   instance user, newline-terminated). The key is printed exactly once.
6. **Config**: install the two `.env` files from the examples in this
   directory to `/etc/x402-demo/`, filling in the exact asset accounts.
7. **Units**: install `x402-demo@.service`, then
   `systemctl enable --now x402-demo@testnet x402-demo@mainnet`.
8. **Nginx**: install `x402-demo.conf` to `/etc/nginx/sites-available/`,
   symlink into `sites-enabled/`, `nginx -t`, reload.

## Verification

- Unpaid `POST /work` on both public hostnames returns `402` with x402
  payment requirements.
- A paying client completes the flow and receives the deterministic
  SHA-256 work result; the settlement lands on chain through the
  facilitator relayer.
- Replaying the identical request returns the cached result with no second
  settlement (facilitator returns `duplicate_settlement`).
- The same payment identifier with a different payload returns `409`.
- All other paths/methods on the demo hostnames return `404`/`405`; every
  funded broadcast follows the runbook mutation gate.
