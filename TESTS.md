# Tests

## Running

```bash
cargo build-sbf --manifest-path programs/core/Cargo.toml
cargo test -p openstake-core -- --nocapture --test-threads=1
```

The build must run first — the litesvm tests load the compiled program from
`target/deploy/openstake_core.so`. `--test-threads=1` matters: without it,
output from parallel tests interleaves and becomes unreadable, since most
tests print the numbers they're asserting on.

Unit tests only (fast, no build needed):

```bash
cargo test -p openstake-core --lib -- --nocapture --test-threads=1
```

One file:

```bash
cargo test -p openstake-core --test claim -- --nocapture --test-threads=1
```

Settlement tests need a real keypair for the authority that settles
markets — set `OPENSTAKE_AUTHORITY_KEYPAIR` to its path, or place it at
`keys/authority.json` (gitignored; never commit it). It must match
`constants::AUTHORITY`.

## What's covered

**Unit tests** (`programs/core/src/**/*.rs`, `#[cfg(test)] mod tests`) — pure
functions, no accounts, no litesvm. The curve math (`amm/curve.rs`), the
pari-mutuel payout formula (`settlement/payout.rs`), struct layout
round-trips, discriminator uniqueness, and the two pure validation checks.
Cheapest and fastest signal — extend these first whenever a formula changes.
Every test states its inputs as dialable `let` bindings and prints its
numbers, so a value can be changed to explore an edge case without editing
assertions.

**Integration tests** (`programs/core/tests/*.rs`) — one file per instruction,
run against the real compiled program in litesvm:

| File | Covers |
|---|---|
| `initialize_market.rs` | accounts created, mint decimals, creator receives seed shares, stake leaves their wallet, kickoff-too-soon rejected, single-outcome rejected |
| `place_bet.rs` | tokens minted match the curve, mint supply tracks the curve, top-ups, hedging both sides, slippage demonstrated, post-close rejected, zero/dust deposits rejected, `max_in_bps_of_vault` cap |
| `settle_market.rs` | authority gate, before-kickoff rejected, double-settle rejected, out-of-range outcome rejected, vault surplus tolerated, insolvent vault blocked |
| `claim.rs` | burn + payout, double-claim rejected, pre-settlement rejected, losers get nothing, every winner gets identical value per share, payouts never exceed the pool |
| `initialize_config.rs` / `update_config.rs` | admin gates, admin rotation, bps bounds, zeroed admin/mint rejected |
| `scenario_bulk_bettors.rs` | 35 bettors, printed price/slippage table |
| `scenario_bulk_solvency.rs` | 35 bettors end-to-end (bet → settle → claim), money conserved, dust bounded |
| `demo_shared_authority.rs` | two unrelated creators share the same hardcoded settlement authority — documents the single-point-of-failure tracked as a TODO |

The test most worth reading first if you're checking correctness rather than
just running the suite: `claim::every_winner_gets_the_same_value_per_share`.
It guards the one bug in this design that would be easy to introduce and hard
to notice — payout dividing by live mint supply instead of the settlement
snapshot, which would silently pay later claimers more than earlier ones
until the vault ran dry.

## Not yet covered

- Secondary market / CLOB — not built.
- Oracle-driven settlement — `settle_market` is a single-signer authority
  today (see the TODO in `settlement/instructions/settle_market.rs`).
- `n > 2` outcomes — `MAX_OUTCOMES` is currently `2`; nothing exercises a
  bigger value yet.
- Token-2022 / transfer-hook enforcement of the post-deadline trading freeze —
  standard SPL Token can freeze one account at a time, not a whole mint; see
  `docs/MVP_PLAN.md` for the reasoning.

## Known environment limitation

`cargo build-sbf` cannot run on this Windows/WSL-UNC-path setup — the
Solana toolchain installer fails with a symlink-privilege error (Windows
error 1314). Every integration test in this repo type-checks cleanly here,
but has not actually been executed from this environment. Run the real build
and test suite on Linux/macOS, or WSL with a native filesystem path, before
treating anything beyond the 30 unit tests as verified.
