# Openstake

A Cargo workspace, restructured (see `../rust-zero-copy-alignment.md` for the
struct-padding background, and the plan this restructure came from). Layout:

- `programs/core/` — the one deployable Solana program (Pinocchio). Internally
  organized `state/` (account structs), `math/` (pure pricing + settlement
  arithmetic), `controller/` (mutation logic), `instructions/` (thin
  entrypoint-facing handlers), `validation/` (shared account checks). See
  `programs/core/tests/` for the full litesvm integration suite and
  `programs/core/benches/` for perf benchmarks of the pure math.
- `oracle/` — scaffold only, explicitly deferred. `settle_market` today
  checks one hardcoded authority pubkey; this is where a real oracle
  integration eventually lives, as a genuinely separate crate.
- `sdk/` — scaffold only, off-chain client crate for third-party providers
  to integrate against Openstake's markets. Never compiled to SBF.
- `tools/compute-discriminators/` — dev-only generator for the account/
  instruction discriminator constants baked into `programs/core/src/state/discriminator.rs`.

## Common commands

```
just build-core        # cargo build-sbf, produces target/deploy/openstake_core.so
just test-core         # the full litesvm integration suite (builds first)
just test <crate>      # e.g. `just test core` for just the unit tests, no VM
just bench <crate>     # criterion benchmarks
just verify            # everything
```

See `arch/` for design docs (`MARKET_DESIGN.md`, `USER_FLOWS.md`,
`diagram.md`) and `TESTS.md` for the test-tier plan — these describe V0.1's
behavior and haven't yet been updated for the file moves in this restructure
(the underlying program logic is unchanged, only reorganized).
