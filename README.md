# Openstake

A Solana peer-to-peer prediction market protocol. Trading is priced by a
share-ratio geometric AMM (`C(q) = sqrt(Σqᵢ²)`); settlement is pari-mutuel —
the winning outcome's holders split the entire pool, proportional to their
holdings. See `docs/MVP_PLAN.md` for the full design and the reasoning behind
each decision.

Built with [Pinocchio](https://github.com/anza-xyz/pinocchio) (not Anchor).
Outcome shares are standard SPL tokens, one mint per outcome per market —
buying mints, claiming burns. The vault is a plain SPL token account holding
USDC, authority-controlled by the market's own PDA.

## Layout

- **`programs/core/`** — the one deployable program. Organized by part, not
  by layer: `amm/` (primary market — curve pricing, `initialize_market`,
  `place_bet`), `settlement/` (`settle_market`, `claim`, the pari-mutuel
  payout formula), `config/` (protocol-wide admin settings — fees, slippage
  limits, the stablecoin markets must be denominated in), `clob/` (secondary
  market — **not built yet**, stub only). Each part owns its own `state.rs`
  with that part's account structs and instruction discriminators. There is
  no separate controller layer — every instruction is one file: the argument
  struct plus the full `process_*` logic, by explicit design choice.
- **`oracle/`** — scaffold only. `settle_market` currently checks one
  hardcoded authority pubkey (`constants::AUTHORITY`); this crate is where a
  real oracle/quorum system eventually lives, kept genuinely separate from
  the on-chain program.
- **`sdk/`** — scaffold only. Off-chain client crate for third-party
  providers integrating against Openstake's markets. Never compiled to SBF.
- **`tools/compute-discriminators/`** — dev-only generator for the
  account/instruction discriminator constants baked into each part's
  `state.rs`. Regenerate with `cargo run --bin compute-discriminators` if a
  struct is renamed or a new account/instruction is added — never hand-edit
  a discriminator.

## Status

Primary market (AMM) and settlement are implemented, SPL-token-based, and
integration-tested (see `TESTS.md`). Secondary market (CLOB) is designed but
not started. Settlement authority is a single hardcoded key — a known,
deliberately temporary centralization point, tracked as a TODO in
`settlement/instructions/settle_market.rs`.

## Common commands

```
just build-core        # cargo build-sbf, produces target/deploy/openstake_core.so
just test-core          # the full litesvm integration suite (builds first)
just test <crate>       # e.g. `just test core` for just the unit tests, no VM
just bench <crate>      # criterion benchmarks
just verify             # everything
```

See `TESTS.md` for what's actually covered by the test suite, how to run it
with readable output, and a known environment limitation (`cargo build-sbf`
doesn't run on every setup).
