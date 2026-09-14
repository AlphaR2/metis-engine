# Metis Model A — Research Notes

**Last updated:** 2026-09-10
**Migrated from the TS research lab** (`solana/engine/docs/research.md`) — this is now the
authoritative copy. The lab (`lib/amm/workspace.ts` + `lib/__tests__/workspace.test.ts`) remains
where these decisions get reproduced and stress-tested; this document remains where they get
recorded as agreed.

## How to read this document

- **Decisions** — things concretely agreed. Nothing goes here that was not explicitly confirmed.
- **Open** — under active research. Candidates are listed; nothing is locked.
- **Findings** — what the experiments showed. Informs the decisions; is not itself a decision.

If something you expect to see is missing, it is because it was not concretely agreed.

---

## 1. Scope

Model A is the self-contained DPM / AMM. This document covers the **cold-start / bootstrap
problem**: keeping early-market price impact acceptable at small seeds, without an order book.

All analysis so far is **buy-only** and uses the **continuous reference curve** unless noted.
Production currently ships `r = 2` with exact integer arithmetic.

---

## 2. The curve (context, not a decision)

```
cost:        C_r(q) = ( Σ qᵢ^r )^(1/r)
probability: P_i    = qᵢ^r / Σ qⱼ^r
```

Single-trade impact, from a state where the bought side has probability `P_i`:

```
P_i' = 1 − (1 − P_i) · (1 + D/C)^(−r)
```

Settlement is **parimutuel**:

```
payout = my_winning_shares × total_collected / all_winning_shares
```

---

## 3. Decisions (concretely agreed)

### 3.1 Share-side virtual liquidity — the V1 model

The mechanism under development is **V1**: virtual shares added to the pricing state only.

- Pricing state = real + virtual:  `Y_p = Y_r + Y_v`,  `N_p = N_r + N_v`
- **The pricing engine sees real + virtual. Settlement sees real shares only.**
- Virtual shares are: symmetric (`V_yes = V_no`), **fixed for the life of the market**,
  not transferable, not redeemable, not claimable, and **excluded from payout and from the
  winning-share denominator**.
- The vault holds only real collateral (seed + deposits). Virtual depth never adds redeemable
  backing.
- **V2** — virtual depth participating dynamically in curve state, or decaying as real capital
  accumulates — is **explicitly deferred**. Prove V1 first.

### 3.2 Minimum seed

`MIN_SEED = 20 USDC`. Markets may open with more; **20 is the worst case all analysis is
designed against.**

### 3.3 A per-trade size cap applies — mechanism and value locked (2026-09-11)

- A per-trade size cap is in force for the whole **AMM phase** (until/if the market graduates
  to an order book).
- **On breach: reject the whole trade.** Do not clamp / partial-fill.
- The allowed maximum must be **shown to the user before they sign** (the frontend previews the
  trade using the same formula the program enforces).
- **Mechanism: relative sizing + a forward-only hard ceiling underneath it, not the impact-guard
  closed form used to size trades directly.** Sizing is `X = 8%` of priced depth (§4.1b). A
  separate, forward-only check — never an inversion, never solving for a deposit — takes the
  size sizing already decided and rejects whole if it would move the odds more than **10pp**.
  See `docs/mvp.md` §1.2 for the full reasoning, including why the closed form alone (§4.1c) is
  unsafe as a *sizing* mechanism specifically (it demands unbounded capital as `Pᵢ → 1`) even
  though its guarantee is exactly right as a *limiting* check.

### 3.4 Acceptable price-impact bands

| Case | Target |
| --- | --- |
| Any trade the cap allows | **≤ 3–5 pp** |
| Hard ceiling — never exceeded by any reachable trade | **10 pp** |
| ~15 pp | **structurally impossible** |

**Accepted exception:** the single worst case — the first cap-sized trade into a bare 20 USDC
seed — may reach **~5.5 pp**. Accepted on the grounds that a brand-new market is inherently
volatile.

### 3.5 Buy-only (known gap)

All current analysis is buy-only. **Sells, and the buy → sell → resolution loop, are not
modelled and not decided.** This is a known gap, not a settled position.

### 3.6 Settlement — parimutuel over the vault

- **The payout pot is the vault balance: real money only** — the opening seed plus every
  deposit traders paid in. Virtual liquidity is never in the pot.
- At **graduation** (primary / AMM market → secondary market) a **snapshot** is taken. It reads
  the on-chain **vault balance** and each holder's real winning-share count. **No token is
  minted** when the primary ends — the snapshot just records balances. Under buy-only with no
  fees, `vault balance == seed + Σ deposits`.
- **Payout, winners only.** If YES wins:
  `holder_payout = holder_YES_shares ÷ Σ all_YES_shares × vault_balance`.
  Floor division; dust stays in the pot. Structurally solvent (pot ≥ Σ payouts).
- Positions are aggregated **per holder** (in the lab, per trader name).
- After graduation the system can quote every holder their **if-YES-wins** and **if-NO-wins**
  amount, drawn from the vault.
- This is `lib/amm/settlement.ts` (`settleMarket`) in the TS lab, a port of
  `programs/core/src/settlement/payout.rs` in this repo — this repo is the authoritative source
  for the actual protocol.

### 3.7 Product-UI display rules

The product UI (not the research lab) **must not show**: the vault balance, the curve cost `C`,
the fact that virtual liquidity exists, or "the maths".

It **shows**: the price of each side; the probability of each side when it differs from the
price; **volume** — real money traded, i.e. money in the vault — split into **primary volume**
and **secondary volume**; the user's shares; and, once the market has graduated, the
**if-your-side-wins earnings estimate** (from the vault).

The research Workspace deliberately shows everything (vault, `C`, virtual depth, backing gap) —
that is the lab, not the product.

---

## 4. Formerly-open items — mechanism/value history

Curvature, the virtual-liquidity multiple, and the trade-cap mechanism are now **locked** — see
`docs/mvp.md` §1.2–§1.4 for the decisions themselves. The candidate history below is kept for
context on how each was reached.

### 4.1 Trade-cap mechanism

The starting requirement was a **flat 50 USDC cap**. Three candidates were evaluated:

**(a) Flat $50** — a fixed dollar ceiling.
Requires a separate "lift / graduation" rule for when the market has matured. A lift at
$500 real backing was tested and found **too early**: post-lift whale trades still moved
10–22 pp.

**(b) Relative — `cap = X% × cost(real + virtual shares)`**, evaluated live, no lift (scales
with the market for the whole AMM phase).
Data pointed at `X ≈ 8%` (≈ $49.60 at open on a $20 seed; ~5.4 pp typical; ~7–8 pp on the
worst counter-trade). `X = 9–10%` pushes the worst counter-trade to ~10 pp. `X = 20%` exceeds
the ceiling (~13.6 pp steady state) and lets trade sizes balloon.

**(c) Impact-guard — `cap = largest trade keeping this trade's odds move ≤ X pp`**, solved
from the current state.
Tested at `X = 10 pp`: holds impact to **exactly ≤ 10 pp from every reachable state**, including
the underdog counter-trade from a lopsided book. Closed-form on-chain (§6), not a runtime search.

**Note (2026-09-10):** the program's `place_bet` already enforces a relative cap
(`Config.max_in_bps_of_vault`), but against the raw **vault balance**, not `cost(real + virtual
shares)` as candidate (b) specifies. Virtual liquidity does not exist in the program yet — once
it does, this cap's denominator needs to change from vault balance to curve cost including
virtual shares, or the cap silently stops meaning what candidate (b) says it means.

### 4.2 Curvature `r` — locked at `r = 2`

Range considered was `1.5 – 2.0`. Lower `r` = gentler impact (`r = 1.5` is `3/2` exactly,
avoiding fractional-power cost) — but only `r = 2` has a proven exact-integer implementation
(`curve.rs`'s `isqrt`) for both the curve itself and the forward impact check §4.1's mechanism
needs. Chosen for MVP on that basis, accepting worse impact numbers than `r = 1.5` would give
(a $50 first trade: 7.18pp at `r=2` vs. 5.5pp at `r=1.5`) in exchange for shipping on math
that's already proven rather than blocking on new exact-integer root work. See `docs/mvp.md`
§1.3.

### 4.3 Virtual-liquidity multiple — locked at `k = 30×`

Range considered was `10 – 30×`. Expressed as a multiple `k` of the opening real shares per
side: `virtual_per_side = k × (seed / 2^(1/r))`. At the locked `MIN_SEED = $20`, `r = 2`,
`k = 30`: `virtual_per_side ≈ 424.26`, pricing the market as if it held **$620**. See
`docs/mvp.md` §1.4 — not yet implemented in the program.

---

## 5. Findings (inform the above; not decisions)

### 5.1 Impact is governed by `D/C` only

Single-trade impact depends only on `D/C` (trade ÷ current priced depth) and `r`. **Absolute
market size never enters.** A trade that is X% of depth produces the same impact at every
market size, forever.

### 5.2 Virtual liquidity is a cold-start tool that self-neutralises

Virtual depth is a fixed additive offset, so as real backing accumulates its share of `C`
→ 0 and its impact-dampening → 0. In every 50-trade test the priced-vs-backed ratio fell from
~7–8× at open to ~1.0× by the end. **No decay logic is needed** — real trading dilutes it. It
does essentially nothing for a mature market.

### 5.3 A flat cap needs a lift; a scaling cap does not

`D ≤ α·C` grows with the market and never has to be removed. A flat cap must graduate, and
choosing the graduation threshold is itself hard (the $500-backing lift left whales at
10–22 pp).

### 5.4 `% of depth` vs `impact-guard`

`α·C` pins impact near a target **only for trades from ~50/50**. A trade buying the underdog
side of a lopsided book runs ~40% hotter — measured at ~10.1–10.5 pp for `α = 9–10%`, ~7–8 pp
for `α = 8%`. The impact-guard bounds `Δp` directly, so it holds from any state (measured:
exactly ≤ 10 pp on all 50 trades of the worst-state sequence).

### 5.5 Reference numbers (a test configuration, NOT a locked parameter set)

`seed $20, r 1.5, VL 30×` → priced depth `$620` at open:

| Trade | Impact |
| --- | --- |
| $1  | 0.1 pp |
| $5  | 0.6 pp |
| $10 | 1.2 pp |
| $25 | 2.9 pp |
| $50 | 5.5 pp — the accepted worst-case exception (§3.4) |

---

## 6. On-chain implementation notes (for whichever cap is chosen)

- **Relative cap (candidate 4.1b):** one line — `require(deposit ≤ C * α)`, where `C` is the
  curve cost the program already computes on every trade. `C` includes virtual shares, because
  the pool's stored share counts include them. One multiply, one compare.

- **Impact-guard (candidate 4.1c):** closed form —

  ```
  max_deposit = C × ( (1 − limit / (1 − P_i))^(−1/r) − 1 )
  ```

  `P_i` = current probability of the bought side, `limit` = the pp bound as a fraction. At
  `r = 2` the exponent is `1/√·`, using the program's existing integer sqrt — a handful of
  fixed-point ops, no loop. If `(1 − P_i) ≤ limit`, the side cannot move `limit` further, so no
  cap applies.

- **The program enforces the cap regardless of the frontend.** The frontend preview (same
  formula, run client-side against public pool state) is UX only: it shows the max and the
  projected odds move before the user signs, and caps the input so a normal user never submits
  an over-limit trade.

- **Slippage:** the trade also carries a `min_shares_out` (or `max_odds_after`) bound, so a
  race that moves the price past the user's tolerance reverts rather than filling badly.
  Separate from the cap.

---

## 7. Accepted-impact reference (external; informed §3.4)

- **Uniswap** interface thresholds: 1% low / 3% medium / 5% high (warning) / **15% blocked**
  for non-expert users.
- **pump.fun:** ~30 virtual SOL of virtual reserve on a $0 real seed; **no per-trade cap**;
  graduates at ~85 real SOL. Tolerates double-digit early price impact by design.
- **Polymarket:** abandoned the AMM for a CLOB, citing wide slippage in volatile markets.
- Prediction markets sit at the **stricter end** of the spectrum: a pp move is a change in the
  implied probability (information), not just a fill price — hence the tighter bands here.

Sources:
- <https://raw.githubusercontent.com/Uniswap/interface/main/apps/web/src/constants/misc.ts>
- <https://support.uniswap.org/hc/en-us/articles/8671539602317-What-is-price-impact>
- <https://pump.fun/docs/bonding-curve>
- <https://github.com/nirholas/pump-fun-sdk/blob/main/docs/bonding-curve-math.md>
- <https://www.chainup.com/blog/why-prediction-markets-need-order-books/>

---

## 8. Explicitly NOT settled

- Sell mechanics and buy → sell → resolution safety (§3.5).
- Graduation criteria from the AMM to an order book.
- Exact-integer / on-chain arithmetic for virtual liquidity (needed for §1.4/§4.3's `k = 30×` to
  actually be implemented).

Locked since 2026-09-11 (see `docs/mvp.md` §1 for the full parameter set): trade-cap mechanism
and value (§3.3, §4.1), curvature `r` (§4.2), virtual-liquidity multiple (§4.3), and the
min-shares-out mechanism replacing the global slippage check (`docs/mvp.md` §1.5).

---

## 9. Where to reproduce

**Model A → Workspace** tab (`/model-a/workspace`) in the TS research lab (`solana/engine`):
live dials for seed, `r`, virtual-liquidity multiple, and the trade cap (three bases — % of
priced depth, max odds move, flat $ — with reject or clamp on breach). Presets "Sheet 1"–"Sheet
12" cover the single-trade sweeps, reachable-state attacks, capital accumulation, and the
at-cap stress runs. Any sheet can then be resolved — **Resolve & pay out** (YES wins / NO wins)
settles the run parimutuel over the vault (§3.6) and shows per-trader payout, profit, and the
if-YES / if-NO estimate. Every table exports to CSV. Engine: `lib/amm/workspace.ts` +
`lib/amm/settlement.ts`; tests: `lib/__tests__/workspace.test.ts`.
