# Metis

**Liquidity by matching, not by capital.** Metis is an intent exchange for prediction markets. A call is filled by
finding someone who disagrees, across venues, instead of by a market maker's money.

Two parts do the work:

- **The Metis market engine** (this repo's on-chain programs): an AMM-priced, pari-mutuel prediction market for
  football calls, settled by a two-reporter oracle.
- **The aggregator**: one API that lists the same question from several venues (Panta on mainnet, Metis on devnet),
  matches them, and scores where a bet is most likely to fill (the **Fill Score**).

**Open Pool** ties them together: instead of a creator or a market maker funding a market, a crowd fills its
opening pot on a Meteora Dynamic Bonding Curve. When the pool fills, the market opens for betting. Backers share
in the market's fees and result, and carry the risk.

## How it fits together

```text
  website (companion repo: Metis-Markets)
        |
        v
  aggregator (aggregator/)  ----->  Panta (mainnet)
        |
        v
  Solana devnet
    programs/core    market AMM: bets, settlement, claims, Open Pool markets
    programs/oracle  football oracle: two attested reporters must agree
    Manifest         order books holders trade on after betting closes
    Meteora DBC      the Open Pool raise, then migration to DAMM v2

  background services
    tools/oracle-keeper   reporters and the settlement crank
    keepers/meteora       runs each Open Pool: launch, watch, migrate, fund, activate
```

## Repository map

| Path | What it is |
|---|---|
| `programs/core` | The market program (Pinocchio, no Anchor). Priced by a share-ratio AMM, settled pari-mutuel; outcome shares are SPL tokens. |
| `programs/oracle` | The football oracle. A result needs two distinct attested reporters to agree; there is a dispute window and a two-root override. |
| `authority` | The trust roots (3 roots and a guardian), compiled in per network. |
| `aggregator` | `metis-api`: the cross-venue API, transaction builders and the Fill Score. |
| `tools/metis-ops`, `tools/oracle-keeper` | Setup and operator commands; the reporters and the crank. |
| `keepers/meteora` | The Open Pool service (TypeScript): raise, migration, faucet, status. |
| `devnet/` | Scripts to deploy, initialise and verify on devnet. |
| `localnet/` | A local-validator harness with throwaway keys. |
| `docs/` | Architecture, formulas, security review, the Open Pool design, deployment guides. |

## Run the tests

```bash
cargo build-sbf --manifest-path programs/core/Cargo.toml
cargo build-sbf --manifest-path programs/oracle/Cargo.toml
cargo test --workspace
(cd keepers/meteora && npm ci && npm test)
```

The litesvm tests load `target/deploy/metis_core.so`, so rebuild it after any program change. Some aggregator tests
execute the transactions it builds against the real compiled program.

## Read first

- [`docs/architecture.md`](docs/architecture.md): how the pieces fit, including the demo timers.
- [`docs/dbc-backer-switch.md`](docs/dbc-backer-switch.md): the Open Pool design and what is proven.
- [`docs/security-review.md`](docs/security-review.md): findings and what was checked.
- [`docs/formulas.md`](docs/formulas.md): the curve and payout maths.
- [`docs/devnet-setup.md`](docs/devnet-setup.md) and [`docs/deployment.md`](docs/deployment.md): deploying it.
- [`docs/disclosure.md`](docs/disclosure.md): what existed before 14 September 2026 and what is new.

## What is real and what is demo

- **Real:** the programs, the oracle logic, the aggregator, the Open Pool flow (proven on a local validator with the
  real Meteora programs), and the transactions, which are signed in the user's own wallet.
- **Demo:** the deployed programs use short timers so a market lives about ten minutes, and match results come from
  a labelled mock feed. Panta runs on mainnet, everything else on devnet. A live Open Pool migration on devnet has
  not been run yet.

## Security notes

No private key is stored in this repository. The roots and guardian are compiled in as public keys only; demo hot
keys exist only on the machine or hosting account that runs the demo. `localnet` builds use throwaway keys and must
never be deployed to a public cluster (the devnet build script refuses a binary that contains one).
