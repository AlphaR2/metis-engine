# Open Pool: how Metis uses Meteora's Dynamic Bonding Curve (DBC)

**The problem.** A new prediction market is empty. Normally someone has to fund it: the creator puts in a seed, or a
market maker rests orders. Both are capital.

**What we built.** The **Open Pool**: a crowd funds a market's opening pot instead. A creator opens a market with
$0. Backers buy a token on a Meteora DBC pool. When the pool fills, the raised USDC becomes the market's pot and betting
opens. Liquidity comes from matching a crowd to the call, not from one party's capital.

## The flow

```text
1  Creator   initialize_backed_market      market opens Pending, with a seed target and a deadline
2  Keeper    create DBC config + pool      a Meteora pool for this market; the backer token is its base mint
3  Backers   swap2 (buy the token)         each buy raises the pool's quote reserve
4  Keeper    pool reaches its threshold    threshold = seed / 0.888 (see "Sizing")
5  Keeper    migrateToDammV2               Meteora moves the pool into a DAMM v2 pool
6  Keeper    removeAllLiquidity            takes the unlocked 89% back as USDC (with a minimum-out guard)
7  Keeper    burn leftovers, fund the vault, then an operator calls activate_market
8  Anyone    bets, settlement, claims      an ordinary market from here on
9  Backers   claim_backer                  redeem the token for a share of the market's seed shares
   (if the deadline passes first: cancel_pending_market, and backers sell back on the curve)
```

## Which Meteora calls we make

All in `keeper/src/` using `@meteora-ag/dynamic-bonding-curve-sdk` and `@meteora-ag/cp-amm-sdk`:

| Meteora call | Where | Why |
|---|---|---|
| `partner.createConfig` | `dbc.ts` `ensureConfig` | One DBC config per seed size: 6-decimal SPL base token with its authorities revoked, a flat 1% curve fee, migration to DAMM v2, 89% of the liquidity unlocked and 11% vested, threshold `seed / 0.888` |
| `creator.createPool` | `dbc.ts` `createPool` | Launches the pool for one market |
| `pool.swap2` | `dbc.ts` `buildBack` | A backer's buy (and the sell-back after a cancel); built unsigned, signed in the backer's wallet |
| `migration.migrateToDammV2` | `raise.ts` `drive` | Moves the filled pool into DAMM v2 |
| `CpAmm.removeAllLiquidity` / `refreshVesting` | `raise.ts` `removeLiquidity` | Takes the unlocked liquidity back (and, after the cliff, the vested slice) |

## Where the code is

| Layer | Path | What it does |
|---|---|---|
| Shared types and maths | [`src/`](src/) (crate `metis-dbc`) | The `initialize_backed_market` instruction layout, a backer's redemption arithmetic (`pro_rata`), the escrow address, the 0.888 sizing factor. Imported by the program, the aggregator and the operator tools. |
| **Keeper** (TypeScript) | [`keeper/src/`](keeper/src/) | Runs every pool: `raise.ts` is the state machine (launch, watch, migrate, fund, activate, cancel); `dbc.ts` makes the Meteora calls; `service.ts` is the HTTP service the website talks to. |
| On-chain instructions | [`programs/core/src/dbc/`](../programs/core/src/dbc/) | `initialize_backed_market`, `activate_market`, `cancel_pending_market`, `claim_backer`. They live with the program because they use its market state, errors and validation. |
| Aggregator | [`aggregator/src/services/metis/dbc.rs`](../aggregator/src/services/metis/dbc.rs) | Builds the create transaction, the backer claim instruction, and the "backer" portfolio position. |
| Operator tools | [`tools/metis-ops/src/dbc.rs`](../tools/metis-ops/src/dbc.rs) | Command-line builders for the same instructions. |
| Proof against the real programs | [`scripts/dbc-spike/`](../scripts/dbc-spike/) | The original run against Meteora's real DBC and DAMM v2 programs on a local validator. |
| Design notes | [`docs/dbc-backer-switch.md`](../docs/dbc-backer-switch.md) | Decisions, findings and limits. |

## Why the program never calls Meteora

The Metis program does **not** make a cross-program call to Meteora. The pool, the migration and the liquidity move are
off-chain, done by the keeper. The program only checks the result when `activate_market` runs: the vault holds the
seed and the backer token is final (no mint or freeze authority, some supply). This keeps the market program small and
independent: **the engine runs without the DBC.** The feature is a switch (`Config.dbc_backing_enabled`); with it off every
market is creator-seeded and none of the code above runs.

## Sizing, and what backers get

Meteora requires at least 10% of the migrated liquidity to stay locked, so we lock 11% (vested behind a cliff of about two
days) and take 89% back at once. A raise is therefore sized `seed / 0.888` (measured 0.88822 on the real programs) so the
unlocked part covers the seed. The locked slice goes to the treasury for now.

Backers hold the token, which is a claim on the market's seed shares and on the backers' share of its fees. If few people
bet, they can get back less than they put in (the curve takes 1%, and the locked slice does not return to them).

## What is proven and what is not

- **Proven** against Meteora's real programs on a local validator: raise, migrate, remove liquidity, burn leftovers, fund,
  activate, settle, backer claim, and the cancel path.
- **Not run on devnet yet:** a live migration. The Meteora programs, the DAMM v2 migration configs and a funded pool
  authority are present on devnet.
- The vested slice cannot be claimed in a short demo (the cliff is about two days).

## Tests

```bash
cargo test -p metis-dbc                     # the shared arithmetic, including property tests
cargo test -p metis-core                    # the instructions, including programs/core/tests/backed_market.rs
cargo test -p metis-aggregator backer       # the aggregator's create, claim and position paths
(cd keeper && npm ci && npm test)           # the keeper's state store and sizing
```
