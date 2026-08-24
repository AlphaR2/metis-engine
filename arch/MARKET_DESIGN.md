# OPENSTAKE — How the Market Actually Works

*Corrected pass. Math was already right throughout — every worked number
in the original checks out exactly. What needed fixing was status: this
doc mixed what's actually running (`engine/src`, V0.1) with what's
planned, under headings that implied it was all built. Every section
below is now tagged. See `arch/USER_FLOWS.md` for the code-accurate
version of ✅ sections only.*

**Legend**
- ✅ **Built** — exists in `engine/src` today, verified against the real code
- 🔜 **Planned** — real design, not implemented; described here as the
  target, not the current state

---

## Preface — What We're Actually Doing Here

We've been going round this in circles across a few sessions. This doc
exists to put things down plainly, in the terms we actually understand,
so there's one place we can point to instead of re-deriving it every time.

The goal is simple: explain the market logic from scratch, with the
diagrams and the worked numbers included, so anyone reading it (including
future us) can follow the reasoning without needing the whiteboard session
that produced it.

---

## 1. The Market ✅

Let's ground this in an actual market instead of talking abstractly. Say
the question is:

> "Will SOL be above $200 on December 10th?"

There are two positions:

- Position A: YES
- Position B: NO

Based on our design, we're not actually limited to two outcomes. The
formula we use generalizes to N outcomes without changing the underlying
logic. We're starting with binary (YES/NO) because it's the simplest case
to reason about, but nothing about the math forces us to stop there.
`MAX_OUTCOMES` in `constants.rs` is currently set to `2` — the code
supports raising it without changing the formula.

---

## 2. Representing a Position ✅

Imagine User A deposits 100 USDC into this market. They need to walk away
with something that represents their verdict. We have two options for
what that "something" is:

- **A** — Give them Solana tokens (an actual SPL token representing their share)
- **B** — Represent their position purely as a number of shares, tracked in program state

**Decided: B.** This is what's built — `Position.outcome_shares` is a
plain `[u64; N]` in program state, no SPL token. For the on-chain CLOB to
work (🔜), we'll need tokens as the representation, so users can place
orders on-chain and have them matched against other buy/sell orders. For
the MVP, we're keeping it as shares and can move to tokenized positions
once the CLOB needs it.

---

## 3. What YES/NO Shares Actually Mean ✅

In plain terms: if YES wins, every share held on the YES side becomes
redeemable for some amount, say n USDC. The NO side becomes worthless.
`claim.rs`'s payout is exactly this — `winning_shares × total_collected ÷
all_winning_shares` for the winning side, zero for the losing side. (The
"tokens are burned" framing is a 🔜 detail — under B above, there's no
token to burn; a losing `Position` simply resolves to a zero payout and
closes.)

---

## 4. The Big Formula ✅

If you remember Pythagoras' theorem, this part is easy. It says:

```
r² = a² + b²
```

Our formula follows the same shape:

```
r = √(a² + b²)
```

- `r` = market radius (called `cost` in the code — same value, different name)
- `a` = total shares on the YES side (`outcome_shares[0]`)
- `b` = total shares on the NO side (`outcome_shares[1]`)

For N outcomes, this extends the same way — we're not boxed into two
sides. The radius formula scales to as many outcomes as we want to
support.

> **Note:** The market radius is a derived mathematical value used to keep
> the pool's state in check. It is not the amount of USDC in the pool, and
> it is not simply the total number of YES and NO shares added together.
> It's calculated from the shares in the pool and acts as the reference
> point for the market's pricing mechanism.

**Worked example** (verified against `curve.rs`'s `cost()`):

Suppose YES shares = 3 and NO shares = 4. Then:

```
r = √(3² + 4²) = √(9 + 16) = √25 = 5
```

We keep r = 5 as our reference point going forward.

---

## 5. Price ✅

Before we talk about probability, we need price. Price only tells us the
current cost of the next unit — how much a small amount of YES or NO will
cost right now, given where the pool is.

From our worked example above:

```
P(yes) = a / r = 3/5 = 0.6
P(no)  = b / r = 4/5 = 0.8
```

**Verified:** these do not add up to 1 — that would actually be an error
if they did. Instead, they get close to √2, approximately 1.414. The
prices don't add to 1; instead, their **squares** add to 1, and that comes
directly from the Pythagorean formula behind the market radius. For
YES = 3 and NO = 4, the prices are 0.6 and 0.8. Their ordinary sum is 1.4,
while the sum of their squares is 0.6² + 0.8² = 1. The maximum possible
ordinary sum for two outcomes is √2 ≈ 1.414, which happens when both sides
are exactly equal, at roughly 0.7071 each — exactly the state a freshly
seeded market opens in.

---

## 6. Probability ✅

Probability is the square of price:

```
Pr(yes) = P(yes)²          Pr(no) = P(no)²
```

Using P(yes) = 0.6 and P(no) = 0.8:

```
Pr(yes) = 0.36 → 36%          Pr(no) = 0.64 → 64%
```

And as expected, Pr(yes) + Pr(no) = 1. The squared normalized coordinates
give us the actual implied probability, while the plain ratios (A/R, B/R)
remain the tradable primary quote.

**Implementation note, worth flagging:** `curve.rs`'s `padded_price()`
divides by `cost()`, which is an *integer* square root (rounds down). At
small pool sizes this introduces real error — e.g. a fresh (7,7) market
computes as ~60.5%/60.5% instead of the true 50%/50%, and the two don't
even sum to 100%. The error shrinks as the pool grows and is negligible at
the real `MARKET_INIT_FEE` scale, but any UI reading `padded_price`
directly for a small or freshly-opened market should be aware of it. Worth
fixing by computing that specific division in floating point rather than
reusing the integer `cost()`.

---

## 7. High-Level Architecture 🔜 (partially ✅)

With the pricing model out of the way, here's how the pieces of the
market fit together in the **target** design. Each market is fully
self-contained: its own USDC vault, its own share state, its own primary
curve, and its own secondary CLOB. There is no shared liquidity pool
across markets, and no external liquidity provider.

✅ **Built today:** the vault, the share state, and the primary curve —
one `Market` + one `Vault` + N `Position`s per market, exactly as
self-contained as described.

🔜 **Not built:** the secondary CLOB. There is no order book, no CLOB
state, no secondary market at all yet — Phase 1 (primary) is the entire
program right now.

Four things are worth separating clearly in your head:

- **Real money** — the actual USDC (currently: native SOL lamports, see
  the open question in `arch/USER_FLOWS.md`) sitting in the market vault ✅
- **Real positions** — the YES/NO shares a user actually owns ✅
- **Curve state** — A, B, R, used purely for primary-market pricing ✅
- **Secondary state** — the CLOB's open orders and reserved assets 🔜

---

## 8. Primary Market — Buy Only ✅ (fee is 🔜)

The primary market is characterized by buys only. A user deposits USDC,
picks YES or NO, and the curve mints new shares on the side they chose.
The other side is untouched.

```
R' = R + D
```

Where D is the net amount applied to the curve **after the primary fee is
taken out** — 🔜. **As built today, `place_bet` applies the entire
deposit directly, `D = payment`, no fee deduction anywhere.** If a fee is
wanted, it needs to be added to `place_bet.rs` before this section is
accurate as a description of the running code.

If the user is buying YES:

```
A' = √(R'² − B²)          new YES shares = A' − A
```

Where A' is the new number of A shares, R is the previous market radius.
This part **is** built — matches `buy_shares()` in `curve.rs` exactly.

The derivative of the cost function is what makes A/R the marginal quote
in the first place:

```
∂C/∂A = A/R          ∂C/∂B = B/R
```

This just says: the cost of the next YES share is A/R, and the cost of
the next NO share is B/R. That's why A/R and B/R work as live, moving
prices rather than fixed numbers.

---

## 9. Creator Seed ✅

Creating a market is its own operation, separate from a normal buy. The
creator deposits real USDC and receives real YES/NO positions in return,
split evenly across both sides via `symmetric_seed()`. This isn't a
phantom or placeholder position — it's real capital, and it settles like
any other real position when the market resolves. Confirmed directly:
`initialize_market` creates the creator's own `Position` with the same
seed shares as the market itself, no special-casing anywhere else in the
codebase.

---

## 10. Secondary Market — CLOB 🔜

**Not built.** Everything below in this section is target design, not
running code. Once shares exist, a holder who wants out doesn't have to
sell back into the curve — they'll be able to sell on the CLOB instead.
Two options planned:

- **Market sell** — no custom price, executes immediately against
  available buyers at the current execution price (from the curve)
- **Limit sell** — the seller sets their own price and the order rests on
  the book until it's matched

A secondary sale only transfers existing shares between users. It never
mints new shares, and it never touches A, B, or R — that separation is
the one piece of this section worth internalizing now, before the CLOB
exists, since it constrains how it gets built.

### 10.1 Secondary Buy Routing 🔜

When a buyer wants shares, the plan is to check the CLOB before the
primary curve. If there's no compatible resting sell order, or only part
of the order can be filled from the book, the remainder falls through to
the primary curve. This whole execution needs to be atomic — the buyer
shouldn't end up in a half-filled, inconsistent state.

---

## 11. CLOB Order Lifecycle 🔜

Not built. Both sides of an order will reserve something upfront and
release or transfer it depending on how the order ends:

| Side | Reserves on placement | Releases on cancel / expiry | Transfers on fill |
|---|---|---|---|
| Buy-side | Required USDC | Unused USDC | USDC → seller, shares → buyer |
| Sell-side | YES/NO shares being offered | Unsold shares | Shares → buyer, USDC → seller |

---

## 12. Settlement Model ✅

OpenStake uses proportional, winner-takes-the-net-pool settlement. A
share isn't a fixed $1 claim — its final value depends on the size of the
net settlement vault and how many real winning shares are splitting it.

```
Payout per winning share = Net settlement from vault / Real winning shares
```

**Verified:** this is exactly `claim.rs`'s payout formula.

**Worked example:**

Vault = $10,000, YES wins, real YES shares = 5,000:

```
Payout per YES share = $10,000 / 5,000 = $2.00
5,000 shares × $2.00 = $10,000 — the full vault, fully distributed
```

> **Note:** Settlement doesn't depend on R at all. R only ever controls
> primary pricing. Settlement is purely about dividing the net vault
> across the real winning shares.

**One thing worth knowing that the code enforces and this section doesn't
mention:** `settle_market` checks the vault's actual lamport balance
against `rent_minimum + total_collected` at the moment of settlement,
before any payout math runs — a hard stop if the two have drifted, rather
than a soft warning. And `claim`'s division floors, always toward the
pool — a market with many claimants can leave a few lamports of dust in
the vault after everyone's claimed, by design, not as a bug.

---

## 13. Complete Execution Flow 🔜 (primary-only path is ✅)

The routing logic below — CLOB checked before the primary curve — is
target design. As built today, there is no router: every `place_bet`
goes straight to the primary curve, because there's nothing else for it
to check.

| Venue | Creates shares? | Changes curve state? | Status |
|---|---|---|---|
| Primary curve | Yes | Yes | ✅ built |
| CLOB secondary | No | No | 🔜 planned |
| Settlement | No | No — it distributes the vault instead | ✅ built |

---

## 14. Slippage ✅

Primary purchases move the curve. That means the quote you see before you
trade is not necessarily the quote you get for the whole trade — larger
trades push the price further as they execute.

```
Before: R = √(A² + B²)          After buying: R' = R + D
```

**Verified, exact:** here's a concrete before/after for a primary YES buy.

| Item | Before | After | Meaning |
|---|---|---|---|
| YES shares | 300 | 447.21 | YES curve state increases |
| NO shares | 400 | 400 | Opposite side is untouched |
| R | 500 | 600 | Radius grows by the net curve payment |
| YES quote (A/R) | 0.60 | ≈ 0.745 | Primary quote moves upward |
| Implied probability | 36% | ≈ 55.56% | Squared normalized coordinate |

Checked against the real formula: R before = √(300²+400²) = 500 ✓.
D = 100 (R goes 500→600). A' = √(600² − 400²) = √200000 = 447.21 ✓.
Price after = 447.21/600 = 0.7454 ≈ 0.745 ✓. Probability = 0.7454² =
55.56% ✓. Every number here checks out.

The gap between the quote you started with and the quote the trade
actually clears at, as it moves through the curve, is the price impact,
or slippage. This part of the mechanism is built and running — the
35-bettor scenario test in `src/tests/scenario_bulk_bettors.rs` proves it
on real transactions, not just on paper.

### 14.1 The Problem, and What Actually Solves It 🔜 (one row is ✅)

| Protection | Protects | How it works | Status |
|---|---|---|---|
| User `min_shares_out` | The trader | Transaction fails outright if the buyer would receive fewer shares than the minimum they signed for | 🔜 not built — no such argument exists in `PlaceBetArgs` yet |
| Maximum primary price impact | Market + all users | Rejects a primary trade if the simulated curve movement exceeds a configured limit | 🔜 not built |
| Maximum primary trade size | Market depth | Rejects oversized primary trades that would eat too much of the permitted primary depth in one shot | 🔜 not built |
| Quote simulation | The trader | Lets us calculate shares out, end quote, fees, new R, and price impact before anything is signed | 🔜 not built as an on-chain instruction — `curve.rs`'s functions are callable off-chain for this today, just not exposed as a dedicated quote instruction |
| CLOB-first routing | The trader | Routes through compatible secondary liquidity before minting new shares, whenever that liquidity exists | 🔜 not built — depends on §10 existing |
| Virtual liquidity (not in original doc) | Early bettors specifically | Pads both sides of the pool before pricing math, softening the steep early-market slippage curve | ✅ **built** — `VIRTUAL_LIQUIDITY` in `curve.rs`, currently `100` |

**Recommendation carried over from earlier discussion, unchanged:**
`min_shares_out` is the one row here worth pulling forward independent of
the CLOB — it protects an individual buyer today, needs no new
infrastructure, and is just one more field on `PlaceBetArgs` plus a check
after `buy_shares_padded` returns.

> Slippage tolerance is a user-level execution guarantee. Price-impact and
> size limits are protocol-level risk controls. They look similar but
> they're solving different problems — one is about what a specific user
> is willing to accept, the other is about what the market itself should
> allow.

### 14.2 Quote Simulation, in Full 🔜

Not built as a dedicated instruction. Every primary buy should eventually
be fully quoteable before it's signed, deterministically, given current
market state:

| Quote output | What it tells us |
|---|---|
| Gross input | Total USDC the user intends to spend |
| Primary fee | Fee deducted per the market's fee configuration (🔜 — no fee exists yet, see §8) |
| Net curve amount D | The amount actually applied to the curve |
| A/B before and after | The state transition on the curve |
| R before and after | How much the curve actually moved |
| Shares out | Exact expected primary shares received |
| Starting / ending quote | Price movement across the execution |
| Price impact | How much curve movement this trade caused |
| Minimum shares out | The user's protection, baked into the signed transaction |

---

## 15. Accounting Separation 🔜 (Vault, Curve, Positions rows are ✅)

| Layer | What it contains | Changes when | Doesn't change when | Status |
|---|---|---|---|---|
| Vault | Real USDC (lamports today) | Primary contributions | Ordinary CLOB ownership transfer | ✅ built |
| Curve | A, B, R | Primary buys | CLOB secondary transfer | ✅ built |
| Positions | User-owned YES/NO | Primary issuance, CLOB transfer | Quote calculation alone | ✅ built (CLOB transfer path is 🔜) |
| CLOB | Orders / reservations | Place, match, cancel | Primary curve calculation | 🔜 doesn't exist yet |
| Settlement | Net vault + winning shares | Resolution / claim | Ordinary secondary transfer | ✅ built |

---

## 16. The Mental Model, Compressed

- **PRIMARY = CREATE A POSITION.** ✅ The user pays USDC, the curve
  determines how many new YES/NO shares come out.
- **SECONDARY = TRANSFER A POSITION.** 🔜 A holder sells existing shares
  to another trader through the CLOB. No new shares, no curve movement.
- **SETTLEMENT = DISTRIBUTE THE MARKET'S MONEY.** ✅ After resolution,
  the net vault is split across real winning shares, pro-rata.
- **SLIPPAGE = PRIMARY CURVE MOVEMENT.** ✅ the mechanism itself; 🔜 the
  guards. Large primary buys move A/B/R and change the quote
  mid-execution — that part runs today. The user and protocol guards
  meant to keep that movement inside acceptable bounds (§14.1) don't
  exist yet.

---

## 17. Target Architecture (was: "Current Agreed Architecture") 🔜

**Renamed from the original.** This section described the full system —
CLOB included — under a heading that said "current." It isn't current;
it's the target. What's actually running today is §1–§9 and §12, minus
the fee in §8 — see `arch/USER_FLOWS.md` for that subset described
without any 🔜 material mixed in.

Putting the whole *target* system together end to end, this is where the
design currently stands:

**Core invariant** (target, matches what's built for the pieces that
exist): the secondary CLOB transfers ownership but never alters primary
curve state. Primary purchases mint new shares and move A/B/R.
Settlement distributes the available net vault across real winning
shares. Nothing else touches these three independently — that separation
is what keeps the whole system reasoning about itself correctly, and it's
already true of the ✅ portion running today.
