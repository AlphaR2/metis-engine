# Metis — Formula Reference

Every formula referenced in `research.md` and `mvp.md`, in one place, with variables defined
once. This is a reference document, not a decision record — see `research.md` for what's
actually agreed and `mvp.md` for what's locked for implementation.

## Where this document sits relative to codegen (Shank / Codama)

Metis is one engine among several a unified SDK will front (alongside other prediction-market
engines). That SDK will use Shank annotations on `metis-core`'s account structs and instruction
args, run through Codama, to auto-generate typed clients — account decoders, instruction
builders, discriminator constants — across whatever languages the SDK targets.

**None of that generates anything in this document.** Shank/Codama only ever sees the *shape*
of an account or instruction — field names, types, byte layout. Every formula below is *derived*
business logic sitting on top of that data (a probability from a share count, a maximum deposit
from a curve cost) — there is no annotation that produces it, and there never will be, because
it isn't wire format, it's math.

Consequences, stated plainly so they don't get lost:

- **Every formula here must be hand-ported**, once per language the SDK's engine-agnostic API
  layer needs to run in. Codama generating a `Market` decoder for TypeScript does not give you
  `max_deposit` in TypeScript — that's a second, separate, deliberate port of §5.3 below.
- **Each port must match its own engine's on-chain arithmetic exactly**, not approximately. A
  floating-point re-implementation of an integer formula (e.g. `Math.pow` instead of exact
  `isqrt`-based squaring at `r=2`) can silently disagree with what the program actually enforces
  — this already happened once in this project's own tooling and was fixed by special-casing the
  exact-integer path. Any new port needs the same discipline: exact match to the on-chain method,
  not a numerically-close approximation.
- **This is a per-engine cost, not a one-time cost.** Metis, Panta, World, and whatever else the
  SDK eventually fronts each have their own curve, their own cap mechanism, their own formulas —
  each needs its own hand-verified port. A shared SDK interface (e.g. `get_max_trade`,
  `preview_settlement`) can standardize the *shape* every engine adapter exposes, but never the
  *math* underneath it, since that's specific to each engine's actual program.

**Notation used throughout:**

| Symbol | Meaning |
| --- | --- |
| `q_i` | Real shares outstanding on outcome `i` |
| `r` | Curvature parameter of the cost function |
| `C` | Curve cost (current "depth" of the market) |
| `P_i` | Probability (price) of outcome `i` |
| `D` | Deposit — the dollar amount of a trade |
| `Y_r, N_r` | Real YES / NO shares |
| `Y_v, N_v` | Virtual YES / NO shares |
| `Y_p, N_p` | Pricing-state YES / NO shares (real + virtual) |
| `k` | Virtual-liquidity multiple |
| `α` | Relative-cap fraction |
| `limit` | Impact-guard's allowed odds move, as a fraction (10 pp → `0.10`) |

---

## 1. The cost curve

The core AMM identity everything else is built on.

**Cost function:**

> `C_r(q) = ( Σᵢ qᵢʳ )^(1/r)`

Sum `qᵢʳ` over every outcome `i`, then take the `r`-th root of the total.

**Probability of outcome `i`:**

> `P_i = qᵢʳ ⁄ Σⱼ qⱼʳ`

Outcome `i`'s share of the total, each outcome raised to the `r`-th power first.

At `r = 2` (production's shipped value), for a binary market this is exact integer math:

> `C(q) = √(q_yes² + q_no²)` — via `isqrt`, floored
> `P_yes = q_yes² ⁄ (q_yes² + q_no²)`

---

## 2. Single-trade price impact

How far a trade of size `D` moves the probability of the side it buys, starting from `P_i`.

> `P_i' = 1 − (1 − P_i) · (1 + D ⁄ C)^(−r)`

- `P_i` — probability of the bought side *before* the trade
- `P_i'` — probability *after* the trade
- Impact in percentage points: `impact_pp = (P_i' − P_i) × 100`

**Finding, not formula:** impact depends only on `D/C` and `r` — absolute market size never
enters. A trade that is `X`% of depth produces the same impact at every market size, forever
(`research.md` §5.1).

---

## 3. Symmetric seed split

How a market's opening stake is divided evenly across outcomes at creation, so the market
doesn't open at zero shares (which would divide by zero in the cost function).

> `seed_per_side = seed ⁄ 2^(1/r)`

At `r = 2`: `seed_per_side = seed ⁄ √2`. This is the formula that was originally miscoded as
`seed/√2` even for non-`r=2` curvatures during the payout-calculation work earlier in this
project — the general form above is the correct one for any `r`.

---

## 4. Virtual liquidity (V1 model)

Virtual shares exist only in the pricing state, never in settlement.

**Pricing state = real + virtual:**

> `Y_p = Y_r + Y_v`
> `N_p = N_r + N_v`

**Virtual depth per side**, expressed as a multiple `k` of the opening real shares per side:

> `virtual_per_side = k × (seed ⁄ 2^(1/r))`

Example: `seed = $20`, `r = 1.5`, `k = 30` → `virtual_per_side ≈ 378` → the market *prices* as
if it held **$620**, while the vault (real backing) still only holds $20.

**Backing gap** — how far pricing depth has pulled ahead of real backing:

> `backing_gap = pricing_depth ⁄ real_backing`

Self-neutralises as real trading accumulates (`research.md` §5.2): this ratio falls from
~7–8× at open toward ~1.0× as the market matures. No decay logic is needed — real trading
dilutes the virtual offset's influence on its own.

---

## 5. Trade-cap mechanisms (§4.1's three candidates)

### 5.1 Flat cap (candidate a)

> `D_max = flat_dollar_ceiling`

Fixed, requires a separate lift/graduation rule once the market matures. Tested and found to
need a lift that was itself hard to tune correctly (`research.md` §4.1a, §5.3).

### 5.2 Relative cap (candidate b)

> `D_max = α × C`

`α` = chosen fraction (e.g. `0.08` for 8%). Cheap, one multiply-compare, scales automatically
with the market. **Confirmed by direct testing not to guarantee a hard impact ceiling** — see
§6 below, and `mvp.md` §1.2.

### 5.3 Impact-guard (candidate c) — the chosen mechanism

The largest trade that keeps this trade's own odds move within `limit`, solved directly from
the current state (no search, closed form).

**Derivation**, starting from §2's impact identity and solving for the `D` where
`P_i' = P_i + limit`:

1. `P_i + limit = 1 − (1 − P_i)(1 + D/C)^(−r)` — substitute `P_i' = P_i + limit`
2. `(1 − P_i) − limit = (1 − P_i)(1 + D/C)^(−r)` — multiply through and cancel
3. `1 − limit ⁄ (1 − P_i) = (1 + D/C)^(−r)` — divide both sides by `1 − P_i`
4. `(1 − limit ⁄ (1 − P_i))^(−1/r) = 1 + D/C` — raise both sides to the power `−1/r`

**Result:**

> `D_max = C × [ (1 − limit ⁄ (1 − P_i))^(−1/r) − 1 ]`

At `r = 2`, the `^(−1/r)` power is `^(−1/2)` — an inverse square root, computable exactly
on-chain via the program's existing integer `isqrt`. Any other `r` needs its own proven
exact-integer method for this power before the guard can be trusted on-chain (open — see
`mvp.md`).

If `(1 − P_i) ≤ limit`, the side cannot move `limit` further from its current position at all,
so no cap applies (§6 of `research.md`).

---

## 6. Slippage guard (independent of the trade cap)

Protects a bettor from a price that moved between when they saw a quote and when their
transaction landed — a *separate* check from the trade cap, always active alongside it.

All-integer, avoiding a division that would lose precision, via cross-multiplication. The
comparison you actually want:

> `shortfall ⁄ expected > limit_bps ⁄ 10_000`

is instead checked, with no division at all, as:

> `shortfall × 10_000 > expected × limit_bps`

where `expected` is the naive "price never moved" share estimate for the deposit, and
`shortfall = expected − actual_shares_received` (only checked when positive — doing *better*
than the naive estimate is never a slippage problem).

---

## 7. Settlement — pari-mutuel payout

The formula every winner's payout is computed from. Real shares only — virtual liquidity is
never part of the pot or the denominator.

> `payout = (winning_shares ⁄ all_winning_shares) × total_collected`

- Floor division — dust stays in the pool, never oversubscribes it.
- `total_collected` = the vault balance = opening seed + every real deposit (buy-only, no fees
  today: `vault_balance == seed + Σ deposits`).
- Structurally solvent by construction: `Σ payouts ≤ total_collected`, always (proven in
  `settlement/payout.rs`'s property tests).

---

## 8. Basis-point bounds (protocol-level ceilings, not curve math)

Not derived from the curve — these are policy ceilings bounding what `Config` is ever allowed
to hold.

**Implemented today, in `constants.rs`:**

| Constant | Value | Bounds |
| --- | --- | --- |
| `BPS_DENOMINATOR` | `10_000` (100.00%) | The ceiling every bps field is measured against |
| `MAX_SLIPPAGE_LIMIT_BPS` | `2,000` (20.00%) | `Config.slippage_limit_bps` |
| `MAX_PRICE_IMPACT_BPS` | `1,000` (10.00%) | The trade cap's impact ceiling — mirrors `research.md` §3.4's "10 pp, never exceeded" |

**Decided, not yet implemented** (`mvp.md` §1.1 — needs a `Config` field split before these can
exist in code):

| Constant | Value | Bounds |
| --- | --- | --- |
| `MAX_FEE_PRIMARY_BPS` | `50` (0.50%) | A future `Config.fee_primary_bps` (AMM fee) |
| `MAX_FEE_TAKER_BPS` | `50` (0.50%) | A future secondary-market taker fee |
