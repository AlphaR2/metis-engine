# Metis — Post-MVP Plan

**Status:** the AMM-side MVP gaps from [mvp.md](mvp.md) §1.6 are closed (virtual liquidity, the
relative-cap + forward-impact trade cap, `min_shares_out`). This is the next batch of work,
in priority order, each sized by rough volume of work so it can be staffed/scheduled.

Sizing is relative, not a time estimate: **S** = one instruction or one focused change, mostly
self-contained. **M** = several files, one coherent mechanism, moderate new surface area. **L** =
a new subsystem with its own state, multiple instructions, and real design decisions still open.
**XL** = a second major subsystem on the scale of the AMM itself.

---

## Priority order

### 1. Multisig, fitted wherever a single key currently has unilateral control (M)

Today two places trust one bare keypair with no second signer, no timelock, no rotation:
- `Config.admin` — controls every protocol-wide dial (`fee_bps`, `max_in_bps_of_vault`,
  `virtual_liquidity_multiple`, `quote_mint`, `max_outcomes`) via `update_config`. Already flagged
  with a `TODO(squads-multisig)` doc comment on the field.
- `constants::AUTHORITY` / `Market.authority` — the single key that can call `settle_market` and
  decide every market's winning outcome. Flagged with `TODO(single-point-of-failure)`.

Work: decide on a scheme (Squads CPI vs. a minimal custom N-of-M multisig account this program
owns directly — Squads is less code but adds an external program dependency; custom is more code
but no external trust). Either way: a multisig-controlled `Address` replaces the bare admin/
authority key everywhere it's checked (`update_config.rs`, `settle_market.rs`), with the actual
signer-count/threshold check happening either via CPI (Squads) or in `validation.rs` (custom).
No new account *types* needed if Squads is chosen (it's just a different `Address` behind the
same field); a custom scheme needs its own account struct, roughly the size of `Config` itself.

### 2. Oracle plumbing (L)

Currently `settle_market` is "one key says the outcome, instantly, no dispute" — see the security
review above. Real oracle plumbing means:
- An oracle-report account/instruction, separate from the multisig admin key — likely an
  optimistic-oracle pattern (propose outcome → open dispute window → bond-backed challenge →
  finalize) rather than a single report call, since sports/event outcomes aren't cleanly
  computable on-chain the way a price feed is.
- New `MarketStatus` transition(s) between `Open` and `Settled` (a `Proposed`/`Disputed` state),
  new timestamps on `Market` for the dispute window, a bond/stake mechanism for challengers.
  This is the largest open design surface in this list — "which oracle pattern" is itself a real
  decision, not just an implementation detail (own optimistic oracle vs. integrating an existing
  one like UMA/Pyth's dispute primitives if applicable to arbitrary event outcomes).
- Depends on (1): whoever can *force* a dispute resolution needs to be the multisig, not a bare
  key, or the oracle's dispute layer is trivially bypassable by the same single-key risk it exists
  to fix.

### 3. CLOB (secondary market / order book) work (XL)

Per `docs/research.md`/`docs/mvp.md`'s "still open" list — buy-only AMM today, no secondary
market exists once `primary_end` hits and the AMM stops minting. This is effectively a second
program-sized subsystem:
- Order account type (maker, price, size, side, expiry), a matching mechanism (on-chain order
  book with crank, or a simpler request-for-quote/AMM-hybrid), fill/cancel instructions, and
  settlement of fills into the same outcome-token mints the AMM already uses (so secondary trades
  and primary-AMM-bought shares are fungible, no separate token standard needed).
- `MAX_FEE_TAKER_BPS` already exists in `constants.rs` for this, unused today — this is where it
  gets wired in.
- Biggest single item on this list by volume — realistically its own multi-week design + build
  effort, not a slot in a batch alongside the others.

### 4. SDK work (M)

Client-side (TS) layer wrapping every instruction: account derivation (PDAs), instruction
builders (mirroring `tests/common/mod.rs`'s `ix_*` helpers, but for real client use, not test
scaffolding), and — per the earlier discussion this session — a **pre-trade quote function** that
replicates `curve.rs`'s `cost`/`buy_shares`/`quote_buy`/the trade-cap formula off-chain against
live `Market`/`Config` reads, so a UI can show price impact, the trade cap, and a suggested
`min_shares_out` *before* the user signs anything. The curve math itself is already proven (it's
the same logic already built once for the TS Model Lab workspace) — this is mostly porting +
wiring to real on-chain reads instead of simulated state.

### 5. Fee layer (S)

`Config.fee_bps` is validated (`MAX_FEE_PRIMARY_BPS = 50`) but never charged — `place_bet` moves
the full `deposit_amount` with no cut taken. Work: take the fee inside `place_bet` (compute
`fee = deposit_amount * fee_bps / 10_000`, route it to a fee-destination account, reduce what
actually enters the curve/vault accounting by that amount), and make sure `total_collected`
reflects the post-fee amount that actually lands in the vault — this is the one place the
"snapshot must exactly match the real transfer" rule from earlier in this session actually
changes shape: today `total_collected += deposit_amount` and the transfer moves `deposit_amount`
1:1; once a fee is skimmed, those two numbers need to split into "what the curve saw" (post-fee)
vs. "what left the bettor's wallet" (gross), and vault solvency accounting has to be re-checked
against the new split. Smallest scoped item on this list, but touches an invariant the whole
settlement audit depends on, so it needs its own test pass, not just an inline change.

### 6. `MarketStatus::Cancelled` + refund path (S)

The enum variant exists (`amm/state.rs`) but nothing ever sets it or pays anyone out from it — if
an event is postponed indefinitely, ruled ambiguous, or the oracle never reports, funds sit in
the vault permanently with no way back to bettors. Work:
- A `cancel_market` instruction — authority/multisig-gated, and/or auto-triggerable once some
  deadline well past `bet_close_ts` passes with the market still `Open` (no `settle_market` ever
  called). Sets `MarketStatus::Cancelled`.
- A refund variant of `claim` (or a branch inside it, gated on `status == Cancelled`) that pays
  each bettor back their pro-rata share of `total_collected` by outcome-token balance, rather than
  running the winner-take-pool `compute_payout` math — everyone gets refunded proportionally to
  what they put in, not what they'd have won.
- Depends conceptually on (2): a real oracle system's dispute/timeout behavior may make "auto-
  cancel after deadline" the natural trigger rather than a manual admin call — worth deciding
  alongside oracle plumbing rather than fully independently, even though the on-chain work here is
  small either way.

### 7. Extensive, intense litesvm test work — make the whole program solid and strong (L)

Current coverage is thin relative to what's now built. `happy_path.rs` is a single golden-path
integration test (init config → market → one bet → settle → claim) — it was never run end-to-end
this session either, since the SBF build (`cargo build-sbf`) failed on a Windows privilege error
installing platform-tools; only the pure-math unit tests (`curve.rs`, `payout.rs`) got verified.
The file's own comment already flags this gap as deliberate-but-deferred: *"Edge-case coverage
lives back in per-instruction test files once the MVP logic (virtual liquidity, the real
trade-cap mechanism) is implemented and worth pinning down in detail."* That condition is now
true. This item is about closing it properly, not adding a few more tests:

- **Get `happy_path.rs` actually running** in whatever environment normally builds this repo
  (native Linux/WSL with the privilege `cargo-build-sbf` needs) — it hasn't been confirmed green
  since the `min_shares_out`/virtual-liquidity/hard-impact-ceiling changes landed.
- **A dedicated `place_bet` test file** exercising every rejection path this batch added, each as
  its own litesvm scenario, not just success cases: trade-cap breach (`TradeCapExceeded`),
  stale-quote rejection (`min_shares_out` under `SlippageExceeded`), hard-impact-ceiling breach
  (`PriceImpactExceeded`), zero-delta rounding (`PaymentTooSmall`), wrong outcome index, betting
  after `primary_end`, betting on a non-`Open` market — and confirm each one leaves the vault/mint
  state completely untouched on rejection, not partially applied.
- **Multi-trade sequences**, not just one bet per market: repeated buys into the same market to
  confirm virtual liquidity, the priced trade-cap, and price impact all behave correctly as the
  book goes lopsided over several real trades — the litesvm equivalent of the CSV-driven scenarios
  already validated in the TS Model Lab, but against the actual deployed program instead of a
  simulation.
- **`initialize_market`/`initialize_config` edge cases**: fee/seed below minimums, outcome counts
  at and past `MAX_OUTCOMES`, `virtual_liquidity_multiple` at and past its ceiling, kickoff times
  too close to now.
- **Settlement + claim edge cases**: double-claim rejection, claiming before settlement, solvency
  check tripping on an underfunded vault, settlement by the wrong authority.
- Once items 1–6 above land, each one needs this same treatment before being trusted — a multisig
  swap-in, oracle dispute flow, CLOB fills, fee skimming, and cancel/refund all need their own
  exhaustive litesvm coverage the same way `place_bet` does here, not folded in as an afterthought.

Sized **L** on its own merits — this isn't "add a few tests," it's bringing every instruction up
to the same rigor the curve math already has (41 passing unit tests, several proptests, two
caught-and-corrected false claims along the way). Surfpool (a heavier, mainnet/devnet-fork-style
local validator) is explicitly NOT needed for this — litesvm already loads the real Token/ATA/
System program bytecode this program CPIs into, so it gives accurate coverage of everything this
program actually touches. Surfpool's value is realistic *cluster* rehearsal (real USDC mint, live
account states, external programs this repo doesn't control) — worth adding later, as a final
pre-mainnet rehearsal layer, not as a substitute for the litesvm work here.

---

## Grouped by volume

| Size | Items |
| --- | --- |
| **S** | Fee layer (5), Cancel/refund path (6) |
| **M** | Multisig (1), SDK work (4) |
| **L** | Oracle plumbing (2), litesvm test work (7) |
| **XL** | CLOB (3) |

Dependency notes: (2) oracle plumbing leans on (1) multisig being done first (a dispute mechanism
guarded by a single key defeats its own purpose). (6) cancel/refund is small in isolation but its
trigger design is cleaner decided alongside (2). (3) CLOB and (5) fee layer are both independent
of the others and could be pulled forward or parallelized if priorities shift. (7) test work
applies to every other item as it lands — treat it as ongoing scope attached to each of 1–6, not
a one-time item to knock out and forget.
