# RPC-outage resilience drill and host-state verification — 2026-07-24

Two operational verifications performed on the launch host
(`i-0537770b34b04b820`, `100.23.147.163`) with no service restart and no
funded broadcast. The first closes the last open drill on the testnet
launch line; the second generates dated evidence for the host-hardening
gates that previously rested on source/config artifacts alone.

## 1. RPC-outage fail-closed and recovery drill (testnet, mainnet-isolated)

### What the architecture actually provides

There is no automatic hot-path RPC failover by design: `broadcast_exact`,
`query_transaction`, and the relayer/readiness reads all use the primary
RPC (`crates/x402-chain-near/src/provider.rs`). The backup RPC serves the
reconciliation cross-check (`query_transaction_backup`,
`backup_relayer_head`) and is a readiness dependency — `/readyz`'s
`refresh_chain_readiness` requires **both** RPCs to report the configured
network and finality (`crates/x402-near-facilitator/src/service.rs:129-136`).
The primary-unknown/backup-final and primary/backup-disagreement logic is
unit-evidenced (`service_recovery_tests.rs::backup_final_result_recovers_when_primary_is_unknown`,
`::conflicting_primary_and_backup_finals_fail_closed`). This drill proves
the operational complement on the live host: an RPC outage fails the
service closed and it recovers cleanly, isolated from mainnet.

### Method

The testnet primary and backup RPC hostnames (`rpc.testnet.fastnear.com`,
`archival-rpc.testnet.fastnear.com`) both resolve to the same Cloudflare
edge IPs, which mainnet's RPC hostname shares as well. Isolation is
therefore by **service user**, not by address: a `REJECT --reject-with
tcp-reset` rule (iptables + ip6tables) scoped with `-m owner --uid-owner
997` (the `x402-near-testnet` user) to the edge IPs on `:443` blackholes
the testnet service's RPC access while the mainnet service (uid 999,
identical destination IPs) is not matched. The rule is fully reversible;
the drill script removed it and a trap guaranteed removal on any exit.
Readiness refreshes on a 15-second timer.

### Timeline (observed)

| Phase | testnet `/readyz` | testnet `rpc` gate | testnet `/healthz` | mainnet `/readyz` |
|---|---|---|---|---|
| baseline | 200 | ready | 200 | 200 |
| block +10s | 200 | ready | 200 | 200 |
| **block +15s** | **503** | **not_ready** | 200 | 200 |
| block +15…70s | 503 | not_ready | 200 | 200 |
| restore +15s | 503 | not_ready | 200 | 200 |
| **restore +20s** | **200** | **ready** | 200 | 200 |
| restore +20…50s | 200 | ready | 200 | 200 |

### Conclusions

- **Fail-closed**: loss of RPC flips the `rpc` readiness gate to
  `not_ready` and `/readyz` to 503 within one refresh tick — the service
  refuses to advertise readiness (and so will not accept settlements it
  could not later reconcile) rather than operating blind.
- **Liveness vs readiness**: `/healthz` stayed 200 throughout — the
  process is alive, only not ready.
- **Mainnet isolation on shared infrastructure**: mainnet `/readyz`
  stayed 200 for the entire ~70 s testnet outage despite sharing the exact
  Cloudflare edge IPs — the uid-scoped isolation holds.
- **In-place recovery**: `/readyz` returned to 200 ~20 s after the rule
  was removed, with no restart (`ActiveEnterTimestamp` unchanged at
  2026-07-23 21:02:57 UTC).
- **Clean teardown**: zero residual `uid-owner` rules in either table.

## 2. Host-state verification snapshot (2026-07-24T12:15Z)

Captured live on the launch host; maps to the host-hardening and
mainnet-credential checklist gates.

- **Service users have no login (`[103]`)**: `x402-near-mainnet` and
  `x402-near-testnet` both `home=/nonexistent shell=/usr/sbin/nologin`.
- **ABI baseline (`[108]`)**: kernel `6.17.0-1019-aws`, `x86_64`, glibc
  `2.39`, systemd `255`, nginx `1.24.0` (Ubuntu). Compatible with the
  release build baseline; the packaged binary's on-host `--version` ABI
  smoke check runs before each pointer change (per go-live records).
- **systemd sandbox (`[114]`)**: `systemd-analyze security` reports
  overall exposure **1.5 OK** for both `x402-near-facilitator@mainnet`
  and `@testnet`.
- **Core dumps (`[116]`)**: `LimitCORE=0` in the unit; both running PIDs
  show `Max core file size 0 0` in `/proc/<pid>/limits`; `coredumpctl`
  lists no x402 dumps.
- **Credential modes (`[112]`)**: all ten
  `/etc/x402-near-facilitator/credentials/{mainnet,testnet}/*` files are
  `600 root:root` (relayer-key, api-key-pepper, database-url,
  database-direct-url, otel-headers per environment).
- **Loopback binding (`[118]`)**: services listen only on
  `127.0.0.1:8402` (mainnet) and `127.0.0.1:8403` (testnet).
- **Origin/clock/disk/patching (`[121]`)**: NTP synchronized (`yes`);
  root filesystem 12% used, 25 G free; `unattended-upgrades` enabled
  (`2.9.1+nmu4ubuntu1`); Nginx logrotate `rotate 13` / `maxage 14`.
- **Mainnet credential modes (`[189]`, operator workstation)**:
  `~/.near-credentials/mainnet/mike.near.json` and
  `x402-relayer2.mike.near.json` are both mode `0600`; no suspected
  exposure (the lost original relayer keys were never exposed — see the
  mainnet go-live record).
