# Relayer provisioning evidence — 2026-07-19

Owner: Mike Purvis / FastNEAR

This record covers only creation, funding, and recovery-key provisioning for
the dedicated facilitator relayer accounts. It is not evidence that either
facilitator service, database, API client, hostname, or funded USDC acceptance
flow is live.

All state below was independently queried at finality through both the
FastNEAR RPC and the corresponding official NEAR RPC. The credential files
and signed transaction bytes are intentionally excluded.

## Testnet

- Account: `x402-relayer.mike.testnet`
- Initial funding: 10 NEAR
- Final balance after recovery-key provisioning: `9.999958035075 NEAR`
- Deployed code: none
- Access keys: exactly two, both `FullAccess`
  - Service:
    `ed25519:6qUu8pD5nfJJq4C4YBJWrXostfRGs1PujEBat9bw9k6Y`
  - Recovery:
    `ed25519:5mu6yBY3Bgrdpszq7rBxPoa3vnCfYHJmFgtPAxqeD35J`
- Account creation, funding, and service key:
  [`5bvZEvB7KPyqKpKmVaQYzDuPaQYrZR1uM27ei3prvPGa`](https://testnet.nearblocks.io/txns/5bvZEvB7KPyqKpKmVaQYzDuPaQYrZR1uM27ei3prvPGa)
- Recovery key:
  [`G4UJoaJbu3UirwrSd1uMstKHUPhRMZbFggSCaQGNREw2`](https://testnet.nearblocks.io/txns/G4UJoaJbu3UirwrSd1uMstKHUPhRMZbFggSCaQGNREw2)

The creation transaction contains exactly `CreateAccount`, a 10 NEAR
`Transfer`, and `AddKey` for the service key. The recovery transaction contains
exactly one `AddKey` action.

## Mainnet

- Account: `x402-relayer.mike.near`
- Initial funding: 5 NEAR
- Final balance after recovery-key provisioning: `4.999958035075 NEAR`
- Deployed code: none
- Access keys: exactly two, both `FullAccess`
  - Service:
    `ed25519:HLPRXAZtCdQN6Wxx5JPSnH1CLpJpu6g59pRrGdEqxWmB`
  - Recovery:
    `ed25519:2VVgn3GrTwCNzhcBJ9afce5TUZtTzBrgXkS6TkLnSU9h`
- Account creation, funding, and service key:
  [`3TgrheK21omRdmwAPXEXrLsS3u2UzPn2osHz3XuZyYFa`](https://nearblocks.io/txns/3TgrheK21omRdmwAPXEXrLsS3u2UzPn2osHz3XuZyYFa)
- Recovery key:
  [`3KY4HLnMWF1FUvuUJ2uzd55D4VS8hP6Du5F3NsC5kzMY`](https://nearblocks.io/txns/3KY4HLnMWF1FUvuUJ2uzd55D4VS8hP6Du5F3NsC5kzMY)

The creation transaction contains exactly `CreateAccount`, a 5 NEAR
`Transfer`, and `AddKey` for the service key. The recovery transaction contains
exactly one `AddKey` action. The recovery transaction burned
`0.000041964925 NEAR` across its transaction and receipt outcomes, matching
the final balance delta.

## Remaining launch gates

Neither public hostname resolves as of this evidence date. No release,
database, API client, service process, funded USDC acceptance payment, replay,
or operational recovery drill is claimed by this document. Those gates remain
unchecked in the launch checklist.
