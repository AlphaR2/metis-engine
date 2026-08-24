# Test Plan — V0.1

Build in this order. Each tier depends on the one above it passing.

## Tier 0 — unit tests (done)

`curve.rs`, 8 tests. Pure math, no accounts, no litesvm. Keep adding to
these whenever the formula changes — cheapest and fastest signal.

## Tier 1 — per-instruction happy path

One litesvm test per instruction, proving the basic case works alone.

- [ ] `initialize_market` — Market/Vault/creator's Position all exist,
      correct discriminators, correct seed shares
- [ ] `place_bet` — first-time bettor (creates Position)
- [ ] `place_bet` — repeat bettor (tops up existing Position)
- [ ] `settle_market` — succeeds when called by the real authority
- [ ] `claim` — winning position, correct payout amount

## Tier 2 — per-instruction rejections

One test per check already written in the code. Walk the error list, prove
each one actually fires.

- [ ] Missing/wrong signer on each instruction
- [ ] Discriminator mismatch (pass a Vault where a Position is expected)
- [ ] `place_bet` on a market that isn't Open
- [ ] `settle_market` called by someone who isn't the recorded authority
- [ ] `settle_market` called twice (already Settled)
- [ ] `claim` before settlement
- [ ] `claim` twice on the same position
- [ ] `claim` from a position that isn't the caller's
- [ ] `outcome_index` / `winning_outcome` out of bounds
- [ ] Vault balance mismatch (needs a deliberately corrupted vault to trigger)

## Tier 3 — full lifecycle scenario

The one that proves the system, not just its parts.

- [ ] `initialize_market` → several `place_bet` (mixed sides, mixed sizes)
      → `settle_market` → every winner calls `claim` → assert the vault's
      final balance is exactly the rent-exempt minimum, nothing more,
      nothing less

This is the on-chain version of the design doc's solvency proof.

## Tier 4 — randomized solvency test

Not built yet — was in the original plan, never landed.

- [ ] N random bettors (random side, random amount, random order) from
      random keypairs, settle on a random winner, everyone claims,
      assert `sum(payouts) <= total_collected`
- [ ] Repeat across many seeds

This is the test that would have caught the sequential-slippage /
order-dependence finding automatically, instead of by hand-inspection.
Worth having going forward to catch the next surprise the same way.

## Tier 5 — adversarial / substitution attempts

- [ ] Pass someone else's Market PDA where this bettor's own is expected
- [ ] Pass a Position belonging to a different bettor
- [ ] Derive and pass a PDA with a wrong bump
- [ ] Reuse a nonce (not really an attack — just confirm it derives a
      different address, no collision)

## Known blocker (not part of this plan — separate fix)

`litesvm = "0.15.2"` in Cargo.toml is the crates.io release with the
`touched_flags`/`solana-program-runtime` mismatch — none of the tiers
above can run until that's resolved. Explicitly not fixing this here per
instruction — fix when encountered.
