# OpenStake Settlement Engine — User Flows (V0.1)

What the code actually does, walked as a sequence of real actions. Matches
the state of `src/` as of this writing — regenerate this by hand if the
instructions change shape.

---

## The shape of the system, in one paragraph

A market is two outcomes (A / B, `MAX_OUTCOMES = 2` for now), priced by a
single formula: `cost = sqrt(sum of each side's shares squared)`. Money
only ever enters through `place_bet` and the creator's seed in
`initialize_market`; it only ever leaves through `claim`. Nobody can sell
back into the pool — there's no instruction for it. Every account
(`Market`, `Position`, `Vault`) carries an 8-byte discriminator, checked
before any other field is trusted, so a wrong-typed account can never be
silently misread.

---

## Flow 1 — Joe creates a market

**Instruction: `initialize_market`**

1. Joe picks a `nonce` (his choice — this is what makes his market's
   address unique; nothing else about it is stored on-chain).
2. He sends one transaction with four accounts: the not-yet-existing
   `market`, `vault`, and `position` addresses (all PDAs he derives
   client-side), and himself as `creator`.
3. On-chain, the program:
   - Re-derives all three PDAs itself from the same seeds
     (`[b"market", creator, nonce]`, `[b"vault", market]`,
     `[b"position", market, creator]`) and checks they match what Joe
     passed in. If they don't, the whole thing stops.
   - Creates all three accounts via `CreateAccount` CPI, each signed by
     the program itself using the derived seeds + bump — Joe pays the
     rent for all three.
   - Computes the opening seed: `symmetric_seed(MARKET_INIT_FEE,
     num_outcomes)` — the protocol's fixed fee, split evenly across both
     sides, so the market opens perfectly balanced. At the current fee
     (`10_000_000`, unit still unconfirmed — see open questions), both
     sides get equal, non-zero shares.
   - Writes `Market` — seed shares, `total_collected = MARKET_INIT_FEE`,
     `status = Open`, `winning_outcome = OUTCOME_UNRESOLVED`, `authority`
     = the fixed protocol constant, `vault` = the vault's own address.
   - Writes `Vault` — just a discriminator, a back-reference to `market`,
     and a bump. Its lamport balance (rent-exempt minimum + the fee) *is*
     its balance — no separate accounting field.
   - Writes `Position` for Joe — the exact same seed shares as the
     market, on both sides. He's not special after this: he's just the
     first bettor.

**End state:** one balanced, open market; one funded vault; one position
belonging to the creator.

---

## Flow 2 — Someone bets

**Instruction: `place_bet`**

1. A bettor picks a side (`outcome_index`) and an amount
   (`deposit_amount`), and sends `market`, `vault`, `position`, and
   themselves as `bettor`.
2. On-chain, the program:
   - Rejects a zero-value bet outright (`PaymentTooSmall`).
   - Reads `Market`, checks its discriminator, checks `status == Open`
     (rejects otherwise — `MarketNotOpen`), checks the passed-in `vault`
     actually matches `market.vault`.
   - Checks `outcome_index` is within this specific market's
     `num_outcomes` (not just the global `MAX_OUTCOMES`).
   - Calls `buy_shares_padded` — the real pricing math, run against a
     virtually-padded pool (see "Virtual liquidity" below) — and gets
     back the exact number of new shares this payment earns.
   - Writes the updated `outcome_shares` and `total_collected` back into
     `Market`.
   - **Branches on whether this is the bettor's first bet in this
     market**, detected by `position.is_data_empty()`:
     - First time: derives and creates a fresh `Position` PDA (same
       `CreateAccount`-signed-by-program pattern as `initialize_market`),
       seeded with just this bet's shares.
     - Repeat bettor: loads the existing `Position`, checks its
       discriminator and that it actually belongs to this market and
       this bettor, and adds the new shares on top.
   - Transfers the real payment from the bettor into the vault — a plain
     `Transfer` CPI, signed normally by the bettor (not the program;
     the bettor is a real wallet, not a PDA).

**End state:** the pool has moved, the bettor holds shares, real money
sits in the vault.

---

## Flow 3 — The match ends, the market settles

**Instruction: `settle_market`**

1. The authority (currently the one fixed protocol pubkey stored on the
   market — not a real oracle, a stand-in for one) sends `market`,
   `vault`, and signs as `authority`, along with the `winning_outcome`.
2. On-chain, the program:
   - Checks the signer is literally the pubkey the market itself
     recorded as its authority — not just "someone signed."
   - Checks `status == Open` (one check that rejects both an
     already-settled market and a market that somehow never opened).
   - Checks `winning_outcome` is a real outcome for this market.
   - Checks `vault`'s discriminator and back-reference, then checks its
     **actual lamport balance** matches `rent_minimum + total_collected`
     exactly. This is the one moment that check can hold true — before
     settlement, no claim has paid anyone yet, so the equality is exact.
     Any drift trips `VaultBalanceMismatch` and stops before the market
     is ever locked in for payouts. (This check used to live in `claim`;
     it moved here, since a vault legitimately shrinks with every
     payout — checking for exact equality *after* claims have started
     would reject every claim past the first one on every market.)
   - Flips `status` to `Settled`, records `winning_outcome`.

**No money moves in this instruction.** It only decides who's right, and
confirms the books balance before anyone can claim against them.

---

## Flow 4 — Winners get paid

**Instruction: `claim`**

1. A bettor sends `market` (read-only — nothing here changes it),
   `vault`, their own `position`, and themselves.
2. On-chain, the program:
   - Reads `Market` once: must be `Settled`, and this is where
     `winning_outcome`, `total_collected`, and the winning side's total
     share count all come from, in a single borrow.
   - Reads `Vault`, checks its discriminator and back-reference only —
     the balance sanity check now lives in `settle_market` (see Flow 3),
     since it's only ever true before any payouts have happened.
   - Reads `Position`: must belong to this market and this bettor
     specifically, and must not already be `claimed`.
   - Computes `payout = (my winning shares × total_collected) ÷ (all
     winning shares)` — floor division, in `u128` so the multiply can't
     overflow. Zero if this position never held the winning side; still
     lets them close out and reclaim rent either way.
   - Marks the position `claimed` **before** moving any money.
   - Pays out of the vault directly — `vault.set_lamports(...)` /
     `bettor.set_lamports(...)`, no CPI, because both accounts are owned
     by this program and a program can freely debit lamports from
     anything it owns.
   - Closes the position by draining it to zero lamports — Solana
     automatically removes any account that ends a transaction at zero,
     so that's the entire "close" operation. Rent comes back to the
     bettor as part of the same drain.

**End state:** the bettor is paid (or just gets rent back, if they lost),
their `Position` no longer exists, the vault has that much less in it.

---

## The pricing engine underneath all of this

`curve.rs` is what `initialize_market` and `place_bet` actually call into.

- **`isqrt`** — integer square root, rounds down, exact on perfect
  squares. Everything downstream depends on this; there's no floating
  point anywhere in the real math.
- **`cost`** — `sqrt(sum of each side squared)`. Not called directly by
  any instruction; the building block for everything else.
- **`symmetric_seed`** — splits the creator's fee evenly across every
  outcome, used once, at market creation.
- **`buy_shares`** — the core formula:
  `new_qi = sqrt((payment + cost_before)² − sum of the other sides squared)`.
  Given how a market's shares looked before a bet, returns what that side's
  total looks like after — the caller subtracts to find out how many
  shares were actually earned.
- **Virtual liquidity (`VIRTUAL_LIQUIDITY = 100`, `padded_*` functions)**
  — every real call from `place_bet` goes through the *padded* versions,
  not the bare ones. A constant is added to both sides before any pricing
  math runs, making the pool behave as if it's deeper than it really is —
  which matters most right after a market opens, when the real pool is
  tiny and a single early bet can otherwise swing price hard. The padding
  is never stored, never paid out, and cancels out exactly in the delta
  math — real share counts come out identical either way. Settlement and
  payout math never touch it.
- **`padded_price`'s rounding caveat.** This function divides by `cost()`,
  an *integer* square root that rounds down. At small pool sizes that's a
  real source of error — a fresh (7,7) market computes as ~60.5%/60.5%
  instead of the true 50%/50%, and the two don't even sum to 100%. Shrinks
  to negligible at the real `MARKET_INIT_FEE` scale, but anything reading
  this function directly for a small or freshly-opened market should know
  about it — worth fixing by computing that one division in floating
  point rather than reusing the integer `cost()`.

---

## Cross-cutting: how every instruction protects itself

The same handful of checks repeat across all four instructions, in this
rough order, every time an account is trusted:

1. **Is the required signer actually signing?**
2. **Is the account marked writable** if this instruction is going to
   change it?
3. **Is the account owned by this program** (`owned_by(&crate::ID)`) —
   never trust data from an account some other program controls.
4. **Does the account's discriminator match what's expected?** — the
   actual defense against a `Vault` being handed in where a `Position`
   was expected. Checked before any other field on that account is read.
5. **Do the account's own stored back-references check out** (a
   `Position`'s `market` field actually matches the `market` account
   passed in; a `Vault`'s balance matches what `Market` thinks was
   collected)?

Only after all of that does an instruction touch the actual business
logic (pricing, status transitions, payouts).

---

## What's deliberately not built yet

- No selling back into the pool — Phase 2 / CLOB doesn't exist.
- No fee on `place_bet` — the full deposit becomes `D` directly. (Some
  design docs describe a primary fee; if that's wanted, it needs adding.)
- No creator royalty split.
- `authority` is one fixed protocol pubkey, not a real oracle.
- No batching — every `place_bet` is priced against whatever the pool
  looked like the instant before it, one bet at a time. Virtual liquidity
  softens this; it doesn't remove the underlying order-dependence.
- No `min_shares_out` or any other user-facing slippage guard on
  `place_bet` — worth adding independent of the CLOB, since it protects
  an individual buyer today and needs no new infrastructure.

**Status update:** all 19 tests pass end-to-end (`cargo test`), including
a 35-bettor scenario proving solvency at scale — the earlier `litesvm`
version conflict is resolved (now `0.16.0` on crates.io, no git
workaround needed).

## Open questions worth resolving before this goes further

- **`MARKET_INIT_FEE`'s actual unit.** The constant is `10_000_000`, and
  the vault holds native SOL lamports (confirmed earlier), but the
  comment trail in the code doesn't currently pin this down as "0.01 SOL"
  versus some other intended size — worth a deliberate decision, not an
  assumed one.
- Whether `VIRTUAL_LIQUIDITY = 100` is the right size, or just what was
  last tested — see the earlier padding experiments for the actual
  tradeoff (bigger softens slippage further but widens the window an
  early large bettor could exploit).
