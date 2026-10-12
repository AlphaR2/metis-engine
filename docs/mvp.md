# Metis MVP — Implementation Plan

> **Superseded.** The current design is in [architecture.md](architecture.md). This file is kept for the reasoning behind earlier decisions; where it disagrees (fees, settlement, the oracle, the secondary market, who may settle), architecture.md is right.


**Status:** early — the previous plan (Phoenix-based secondary market, token-freeze design) was
retired along with `research/curve-experiment` and the old `docs/research/` notes. See
[research.md](research.md) for the research decisions this plan implements.

This is where the concrete, agreed plan for turning `research.md`'s decisions into the on-chain
program goes.

---

## 1. Locked decisions

### 1.1 Fee ceilings, split by market phase

`Config.fee_bps` is currently a single field (stored, validated, **not yet wired into
`place_bet`** — no fee is actually taken today). Once fees are wired in, the ceiling splits by
phase rather than staying one flat number, since primary (AMM) and secondary (taker/CLOB, not
built yet) are different economic contexts:

| Constant | Value | Applies to |
| --- | --- | --- |
| `MAX_FEE_PRIMARY_BPS` | `50` (0.5%) | Fee taken on `place_bet` (AMM) |
| `MAX_FEE_TAKER_BPS` | `50` (0.5%) | Fee taken on a future secondary-market (CLOB) taker fill |

Both replace the earlier single `MAX_FEE_BPS = 500` (5%) figure — 0.5% is the real ceiling,
not 5%. `Config` will need a `fee_primary_bps` field alongside (or instead of) `fee_bps`, plus a
`fee_taker_bps` field once the CLOB exists; each bounded by its own constant, same pattern as
`MAX_SLIPPAGE_LIMIT_BPS` / `MAX_PRICE_IMPACT_BPS` already enforce in `constants.rs`.

### 1.2 Trade cap: relative-% sizing + a forward-only hard impact ceiling underneath it

**Supersedes the earlier version of this section**, which concluded the impact-guard closed
form should be the *primary* sizing mechanism. That conclusion was correct about the closed
form's guarantee but wrong about where to put it. The closed form sizes a trade by **inverting**
the impact formula — solving "what deposit hits exactly this pp" — and that inversion divides
by `(1 − Pᵢ)`, which explodes toward infinity as probability approaches 100%. Confirmed directly
in Workspace runs: a five-trade one-directional streak toward `Pᵢ ≈ 90%` produced a **$1.8M**
"required" trade, and a longer streak toward the floating-point-saturated edge produced trade
sizes in the **10²¹–10⁵¹** range with `impact_pp` reading a meaningless `0.0000` (the move was
real but too small relative to the astronomical size to register). This is not a contrived
adversarial input — an obvious, heavily-favoured market is completely normal, and the closed
form's own math guarantees this failure mode is reachable by anyone who understands the
formula and has the capital, not just by accident.

**The corrected design, now locked:**

1. **Sizing** — the cap basis is **relative**: `cap = X% × cost(real + virtual shares)`
   (research.md §4.1b). Cheap, bounded by construction (a plain multiply, no inversion,
   no asymptote), scales with the market, never needs a lift.
2. **Limiting** — a **forward-only** hard impact ceiling sits underneath it, independent of
   sizing. It never solves for a deposit; it only takes the size the cap basis already decided
   and asks "how far would this actually move the odds" — a direct plug-in to the existing
   impact formula, not its inverse. If the answer exceeds the ceiling, **reject the whole
   trade** (§3.3's rule — never clamp). Because this never inverts anything, it cannot blow up:
   as a market approaches certainty, trades near the cap basis's own limit simply start getting
   rejected more often (genuinely less room left to move into), instead of the mechanism
   demanding an ever-larger dollar figure to keep hitting a fixed pp target.

Verified in the Workspace engine (`lib/amm/workspace.ts`'s `hardImpactCeilingPp`, forward-check
only): reproduces the exact reversal-off-a-lopsided-book breach a relative cap alone lets
through, catches it, and the same engine confirms trade sizes stay bounded (no blow-up) even
pushed toward the `P → 1` edge, across 40+ trades.

**Chosen values for MVP** (§1.6 below has the full locked parameter set):
`X = 8%` for sizing, hard ceiling `= 10pp` for limiting — mirrors
`constants::MAX_PRICE_IMPACT_BPS = 1000` already in `constants.rs`.

**Rust implementation gap, not yet closed:** `place_bet.rs` today only implements sizing
(`Config.max_in_bps_of_vault`, checked against raw vault balance — see research.md §4.1's
2026-09-10 note on why that denominator is wrong once virtual liquidity lands). **There is no
forward-only hard-impact check in the program yet.** This needs to be added as an unconditional
check after the curve executes and before the transfer, using the closed-form impact formula
(research.md §6) in its *forward* direction only — never solved for `D`.

### 1.3 Curvature `r = 2` — locked

Resolves research.md §4.2 (previously "range 1.5–2.0, not pinned"). Chosen for MVP because the
exact-integer math the impact-guard-style forward check needs (`(1/r)`-power, here `1/√·`) is
already proven in `curve.rs` via the existing integer `isqrt` — any `r < 2` would need a new,
unproven exact-integer root method before either the forward impact check or the curve itself
could be trusted on-chain. Tradeoff accepted knowingly: `r = 2` produces measurably worse price
impact than lower `r` for the same trade size (e.g. a $50 first trade into a $20 seed: **7.18pp
at r=2** vs. **5.5pp at r=1.5** — both figures reproduced directly in the Workspace). No code
change needed — production already implements exact `r = 2` arithmetic exclusively; there is no
generic `r` parameter in `programs/core` to change.

### 1.4 Virtual-liquidity multiple `k = 30×` — locked

Resolves research.md §4.3 (previously "range 10–30×, not pinned"). `virtual_per_side = k ×
(seed / 2^(1/r))` — at `seed = $20 (MIN_SEED)`, `r = 2`, `k = 30`: `virtual_per_side ≈ 30 ×
14.14 ≈ 424.26`, pricing the market as though it held **$620** while the vault genuinely holds
$20. **Not implemented in the program yet** — virtual liquidity (§3.1's V1 model) doesn't exist
in `programs/core` at all; `curve.rs` operates on real shares only. `k = 30` is recorded now so
the eventual implementation has a locked target rather than an open range.

### 1.5 Min-shares-out replaces the global slippage check — locked

Resolves the finding that `Config.slippage_limit_bps` (checked in `place_bet.rs` against a
naive `deposit × cost_before ÷ old_total` estimate) only means anything at `r = 2` — at any
other `r` it reports a large, constant, trade-size-independent "slippage" that has nothing to
do with what actually happened to the trader, and even at `r = 2` it can misfire to ~100% on a
legitimate trade once the book is sufficiently lopsided (confirmed directly: a real, non-`r`-
dependent worked example produced `99.93%` reported "slippage" on a trade that was, in fact,
fine). It also never protects against the actual risk — the market moving between when a trader
was quoted and when their transaction lands — since it re-derives its own "expected" figure from
the same pre-trade state the instruction executes against, with zero elapsed time.

**Decision:** remove `Config.slippage_limit_bps` and its check entirely. Replace with a
**caller-supplied `min_shares_out: u64`** argument on `place_bet` — the trader (via their
wallet/frontend) computes the fewest shares they'll accept for their deposit from the state
they were actually shown, and the program rejects the whole trade if the real, executed share
count falls short. This is `r`-agnostic by construction (it only ever compares shares to
shares, never assumes a price/depth relationship) and protects against the real risk (a stale
quote), not a formula artifact.

**`min_shares_out` is computed off-chain, by the caller — it is not a program constant.** The
values `5` (trades of staleness) and `5.00%` (tolerance) validated in the Workspace engine
(`minSharesOutStaleTrades` / `minSharesOutTolerancePct`) are the **recommended default
parameters for the SDK/wallet** to use when constructing `min_shares_out` from a quote — i.e.
"accept a quote up to 5 trades old, with up to 5% slack" — not values the Rust program itself
stores or enforces. The program's only job is the single unconditional check: `if
delta < min_shares_out { reject }`.

**Rust implementation gap, not yet closed:** `PlaceBetArgs` today has only `deposit_amount` and
`outcome_index` — no `min_shares_out` field. `Config.slippage_limit_bps` and its check in
`place_bet.rs` still exist and need removing as part of this same change, not left in place
alongside the new mechanism.

### 1.6 Full locked MVP parameter set

| Parameter | Value | Status |
| --- | --- | --- |
| Minimum seed | `MIN_SEED = 20 USDC` | Implemented (`constants.rs`), enforced |
| Curvature | `r = 2` | Implemented — the only `r` the program has ever supported |
| Virtual-liquidity multiple | `k = 30×` | **Not implemented** — virtual liquidity doesn't exist on-chain yet |
| Trade-cap sizing | Relative, `X = 8%` of priced depth | Sizing mechanism exists (`Config.max_in_bps_of_vault`); denominator is currently vault balance, needs to become priced depth once virtual liquidity lands |
| Trade-cap limiting | Forward-only hard ceiling, `10pp` | **Not implemented** — no forward impact check exists in `place_bet.rs` yet; `MAX_PRICE_IMPACT_BPS = 1000` already reflects this value |
| Share-count protection | `min_shares_out`, caller-supplied; SDK default `5` trades stale / `5.00%` tolerance | **Not implemented** — needs a new `PlaceBetArgs` field; `slippage_limit_bps` needs removing |
| Primary fee ceiling | `MAX_FEE_PRIMARY_BPS = 50` (0.5%) | Implemented (`constants.rs`), enforced; `fee_bps` itself not wired into `place_bet` |

---

## 2. Still open

`docs/research.md` §8 minus what §1 above has resolved: buy-only vs. sells (§3.5), graduation
criteria from the AMM to an order book. Nothing here is decided until it moves into §1.
