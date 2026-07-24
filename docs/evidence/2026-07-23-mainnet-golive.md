# Mainnet go-live, relayer replacement, and acceptance — 2026-07-23

Owner: Mike Purvis

This record covers the mainnet facilitator reaching public readiness,
replacing an unrecoverable relayer account, and the funded acceptance. Each
funded broadcast was previewed and explicitly confirmed immediately before
submission.

## Relayer replacement

The original mainnet relayer `x402-relayer.mike.near` (provisioned
2026-07-19) became unusable: neither its service key nor its recovery key
could be located on the operator workstation, and modifying an account
requires one of its existing keys. That account retains 5 NEAR that is now
permanently locked. Root cause: the service key had been generated to a
host-only path that was not preserved or recorded.

A fresh relayer was provisioned with the key preserved in the standard
credential store:

- Account `x402-relayer2.mike.near`, a subaccount of `mike.near`, funded
  with 5 NEAR. Creation transaction
  `BEFV41nPAkhC8cHsXFHNPYa5sJBv4ZZVjM3hukRZ2dCc`, signed by `mike.near`.
- Service key `ed25519:A8QQpCfyKqqZGSXSh3SHaxF9icHnzBui7KjpPxDcmYV4`,
  generated locally without printing the private half and stored mode-0600
  at `~/.near-credentials/mainnet/x402-relayer2.mike.near.json`, then
  installed as the host relayer credential.
- Recovery key `ed25519:6sXNZuK6…` (the `mike.near` FullAccess key) added
  as a second key so the service key can always be revoked. Transaction
  `HC4GQva5QZViB2NcXArjrk7pTiu2a9qjKUsFMD2o485h`. The account holds exactly
  these two FullAccess keys.
- `mainnet.json` `relayer_account_id` updated to `x402-relayer2.mike.near`.

## Host readiness

- `current-mainnet` -> `releases/v0.1.2`; `x402-near-facilitator@mainnet`
  active and enabled at boot, version `0.1.2`, bound to `127.0.0.1:8402`.
- Local and public `/readyz` return 200 `ready: true` (database, leadership,
  reconciliation, rpc, relayer). Public `https://x402.mikedotexe.com`
  `/supported` advertises `near:mainnet` with signer
  `x402-relayer2.mike.near`; unauthenticated `/verify` returns 401. Both RPC
  endpoints are FastNEAR (regular primary, archival backup).

## Funded acceptance (real Circle USDC)

- Pre-broadcast `/verify` returned `isValid: true` with `payer: mike.near`.
- Facilitated settlement `mike.near` → `count.mike.near`, 1,000 atomic USDC,
  through `x402-relayer2.mike.near`. Transaction
  `3KpKfbGcgKTsnbF9cj9y6Eh3oRM2yLdSfXXxV6RPgQrs`, outer `SuccessValue`, all
  four receipts succeeded, sponsorship cost `0.000334` NEAR against the
  `0.01` NEAR maximum reservation. Balances moved exactly: `mike.near`
  3606329 → 3605329, `count.mike.near` 332001 → 333001.
- Replay of the identical request returned `duplicate_settlement` with an
  empty transaction and an unchanged relayer balance, proving no second
  transfer was created.

## Remaining operational items

External 60-second `/readyz` monitoring for both hostnames, a second
incident contact, the nightly off-host database dump with a restore drill,
and the explicit restart and low-balance drills remain open.
