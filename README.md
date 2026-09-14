# Metis

Metis is a robust protocol and infrastructure for prediction markets. We are a meta-level protocol and aggregation layer for fragmented prediction-market liquidity, markets, and execution. The vision is one integration layer through which products can discover markets, access liquidity, route trades, and build prediction-market experiences without having to integrate every venue independently. We are building an "all you need" basket for prediction markets.

**Metis is not primarily an AMM.** The in-house AMM in this repo is our own prediction-market primitive. This lets Metis operate its own markets, build out a proper protocol with proper market mechanics, and have skin in the game while the broader protocol aggregates and connects external prediction-market venues.

```text
                    METIS
          Prediction Market Infrastructure
                       │
        ┌──────────────┼──────────────┐
        │              │              │
     Discovery       Liquidity      Execution
        │              │              │
        └──────────────┼──────────────┘
                       │
              ┌────────┴────────┐
              │                 │
        External Markets    Metis Markets
                                          
```

The infrastructure layer is the product; this repo's own market engine is one part of it, not the entire thing.

## This repo

`programs/core` is the in-house EMM/AMM — the one deployable Solana program implementing Metis's own market primitive. Trading is priced by a share-ratio geometric AMM (`C(q) = sqrt(Σqᵢ²)`); settlement is pari-mutuel — the winning outcome's holders split the entire pool, proportional to their holdings. See `docs/mvp.md` for the implementation plan and `docs/research.md` for the reasoning behind each decision.

Built with [Pinocchio](https://github.com/anza-xyz/pinocchio) (not Anchor). Outcome shares are standard SPL tokens, one mint per outcome per market — buying mints, claiming burns. The vault is a plain SPL token account holding USDC, authority-controlled by the market's own PDA.


## Status

Primary market (AMM) and settlement are implemented and SPL-token-based. The MVP-locked mechanisms from `docs/mvp.md` §1.6 are all wired in: `r = 2` curvature, virtual liquidity, the relative trade-size cap plus a forward-only hard price-impact ceiling, and caller-supplied `min_shares_out` (which replaced the old `Config.slippage_limit_bps` check — that field and mechanism are gone). Curve math and payout math have solid unit/property-test coverage; per-instruction litesvm coverage (every rejection path, multi-trade sequences, settlement/claim edge cases) is not yet at the same bar — see `docs/plan.md` item 7 for the open test-work scope.

Secondary market (CLOB) is designed but not started. Settlement authority isa single hardcoded key — a known, deliberately temporary centralization point, tracked as a TODO in `settlement/instructions/settle_market.rs` and as item 1–2 in `docs/plan.md` (multisig, then real oracle plumbing).

See `docs/plan.md` for the full prioritized list of what's next and how bigeach piece is. Everything in that plan and in this repo today is the in-house EMM/AMM primitive — the discovery/liquidity/execution aggregation layer around it is a separate, broader effort this repo doesn't yet cover.

## Common commands

```
just build-core        # cargo build-sbf, produces target/deploy/metis_core.so
just test-core          # the full litesvm integration suite (builds first)
just test <crate>       # e.g. `just test core` for just the unit tests, no VM
just bench <crate>      # criterion benchmarks
just verify             # everything
```

`just test-core` needs `cargo build-sbf` to succeed first, which requires the Solana SBF platform-tools to be installed with permission to create symlinks — on Windows this means Developer Mode (or an elevated shell); it fails with an `os error 1314` otherwise. `just test core` (unit tests only, no VM) has no such requirement and is the fast path for iterating on curve/payout logic.
