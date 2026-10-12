# DBC backer raise: what is built and what is not

## Decision

A market's seed can be raised from outside backers through a Meteora dynamic bonding curve (DBC)
instead of coming wholly from the creator. It is an **admin switch** (`Config.dbc_backing_enabled`,
0 or 1; `update_config` sets it). Off, every market behaves exactly as before.

## What is built (program, tested)

State, in `programs/core`:

* `MarketStatus::Pending` (5): a backed market whose raise has not opened it. No seed shares exist,
  nobody can bet. Transitions: `Pending -> Open` (activate) and `Pending -> Cancelled` (give up).
* `Market` grew from 320 to 376 bytes: `seed_target`, `backer_supply`, `raise_deadline`,
  `backer_mint`. `seed_source` is `DbcBackers` for these markets.

Instructions (discriminators in `amm/state.rs` and `settlement/state.rs`, regenerate with
`cargo run -p compute-discriminators`):

| Instruction | Who | What |
|---|---|---|
| `initialize_backed_market` | the creator (pays rent only) | Creates the market, vault and outcome mints as `Pending`, recording the DBC base mint, the seed the raise must deliver (30 to 100,000 USDC) and a raise deadline between `MIN_RAISE_SECS` and `MAX_RAISE_SECS` from now and not after betting would close. Needs `dbc_backing_enabled = 1`. |
| `activate_market` | an operator | Requires the vault to hold the seed, the backer mint to be the recorded one with **no mint or freeze authority** and some supply, and the deadline not passed. Mints the seed shares to escrow accounts (the market's associated accounts), sets `total_collected = seed`, and opens the market. No CPI into Meteora. |
| `cancel_pending_market` | anyone after the deadline; an operator or root any time | Marks the market `Cancelled`. Nothing is held at that point, so nothing is refunded here: backers sell their tokens back on the DBC curve. |
| `claim_backer` | any backer, after settle or cancel | Burns **all** the backer's tokens and the matching slice of the escrowed seed shares, and pays out. No per-backer record. |
| `settle_market` | unchanged | Now accepts `SeedSource::DbcBackers`; the backers' share of the revenue is recorded as `backers_owed`. |

How `claim_backer` pays: the slice is `tokens burned / backer tokens still outstanding` (the live
mint supply), applied to what is left. After each redemption the ratio of what remains to what is
outstanding is unchanged, so every token is worth the same whenever it is redeemed. Rounding is
down, so only dust is left behind.

* Settled: the slice of the escrow's *winning* shares is paid like any winning shares
  (`shares * payout_pool / all winning shares`), plus the slice of `backers_owed`.
* Cancelled: the slice of the escrow's shares on both sides is refunded like any other holder's
  (`shares * pool / all shares`).

Tests: `programs/core/tests/backed_market.rs` (17) and the unit tests in `settlement/backer.rs`. The
main test has every holder redeem and asserts the vault holds exactly what the protocol, creator and
platform are still owed, plus under 40 atoms of dust, and that every atom is accounted for.

## What is proven against the real Meteora programs

`scripts/dbc-spike` runs the whole raise on a local validator with the real DBC and DAMM v2
programs: config, pool, buy to graduation, migrate, remove liquidity. Measured: a 40 USDC raise
returned **35.528799 USDC (88.8%)** to the authority right after migration, plus 177,644 of the
1,000,000 base tokens (to be burned), and left the base mint with no mint authority and no freeze
authority. See `scripts/dbc-spike/README.md`.

**It differs from the original hypothesis.** Meteora requires at least 10% of the migrated liquidity
to stay locked one day after migration, so 100% unlocked partner liquidity is refused. The spike
configures 89% unlocked and 11% vested (2-day cliff). Consequences:

1. Size the raise so that about 88.8% of it equals the seed (`raise = seed / 0.888`).
2. The vested 11% (about 4.5 USDC per 40 USDC raised, plus about 22,000 tokens) comes to the partner
   only after the cliff. **There is no instruction yet that returns it to the backers**, so today it
   would stay with whoever holds the partner key. Decide before launch: top it up to the vault and
   credit `backers_owed`, or accept it as the cost of the raise and say so.
3. The roughly 22,000 tokens still in the locked DAMM position are counted in the supply
   `claim_backer` divides by. Nobody can redeem them until someone buys them from the pool, so
   that slice of the escrow stays unredeemed until then (about 2.7% in the spike's configuration).

## What is not built

* **The Meteora keeper**: create the DBC pool per market, watch the raise, migrate and remove
  liquidity back to back (with a minimum-out so it fails safe), burn the leftover tokens the
  authority receives, move the USDC into the vault, call `activate_market`, and call
  `cancel_pending_market` at the deadline. The spike is its core; it needs wrapping and a driver.
* Leftover burn is a keeper duty, not enforced: `activate_market` cannot see which accounts hold
  unsold tokens. If the operator holds backer tokens it can redeem them like a backer.
* `metis-ops` and the aggregator do not yet create, show raise progress for, or activate a backed
  market. The aggregator reports a `Pending` market's phase as `pending` and quotes nothing for it.
* The oracle keeper's crank ignores `Pending` markets (activation is the Meteora keeper's job).
* Devnet: the pool-authority rent during migration, and the manual migration step, are unverified.
* Anything above the seed in the vault at activation is not counted and goes to the treasury when
  the market is closed.
