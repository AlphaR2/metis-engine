# OpenStake MVP — Implementation Plan

Locked decisions and the concrete work to get there.

---

## 1. Locked decisions

| Decision | Value |
|---|---|
| Primary window closes | **5 hours before kickoff** |
| `bet_close_ts` means | Kickoff = market trading end time |
| Phase | **Derived from timestamps**, not stored |
| AMM sell | **Does not exist** — buy-only, forever |
| Creator's payment | **Creation stake** — creator receives seed shares |
| Settlement blocked by Phoenix | **No** — decoupled |
| Phoenix market funding | **Creator pays at market creation** |
| Outcome shares | **Standard SPL mints**, one per outcome |
| Vault | **USDC token account** |
| Token-2022 | Out of scope |
| Permissionless | Everything except resolution |

---

## 2. The timeline

```
  create market          primary ends           kickoff            result known
        │                     │                    │                    │
        ▼                     ▼                    ▼                    ▼
        ├─────── PRIMARY ─────┼───── SECONDARY ────┼──── CLOSING ───────┼─── RESOLVED ──▶
        │      (AMM only)     │   (Phoenix only)   │   (no trading)     │   (claims open)
        │                     │                    │
        │                     │◀─── 5 hours ──────▶│
```

Two timestamps define everything:

- `bet_close_ts` — kickoff. Set by the creator at market creation.
- `primary_end` = `bet_close_ts − 5 hours`. Computed at creation, **stored on the market**.

Storing `primary_end` (rather than recomputing it from a config value on every read) means no admin action can ever shift a live market's phase boundary.

### Deriving the phase

Nothing stores a phase. Anyone — program, SDK, frontend — computes it the same way:

```
if resolved              -> Resolved
else if now >= kickoff   -> Closing
else if now >= primary_end -> Secondary
else                     -> Primary
```

**This is why no crank is needed.** There is no instruction anyone must remember to call, and no window where a stale flag leaves the AMM open. Each instruction checks the clock itself and refuses if it's out of phase.

---

## 3. Important limitation to accept up front

**With standard SPL Token, we cannot globally stop token transfers at kickoff.**

SPL's freeze operation works on *one token account at a time*. There is no "freeze the whole mint" instruction. So at kickoff we can:

- ✅ Close the Phoenix market — stops all order book trading
- ❌ Stop direct wallet-to-wallet transfers

**Why this is acceptable for MVP:** with no order book there is no price discovery and no counterparty matching. Trading during the match is reduced to manual OTC deals — high friction, no liquidity, negligible volume. The exploit we actually care about (someone systematically picking off stale prices during a live event) requires a venue, and the venue is closed.

**The proper fix is a Token-2022 transfer hook**, which lets our program reject transfers after kickoff automatically. Deferred, and the reason to eventually adopt Token-2022.

---

## 4. What's permissionless

| Action | Who can do it |
|---|---|
| Create a market | Anyone (pays creation stake + Phoenix rent) |
| Buy in primary | Anyone |
| Primary closes | **Nobody — it's a timestamp** |
| Trade in secondary | Anyone, via Phoenix |
| Close Phoenix at kickoff | **Anyone** (instruction is time-gated, so it's safe to expose) |
| Cancel your own stuck order | The order owner |
| Resolve the market | ⚠️ **Oracle authority only** |
| Claim winnings | The token holder |
| Burn losing shares | The token holder |

Resolution is the single non-permissionless step. For MVP that's a Squads multisig. Everything else can be called by anyone, because every instruction is gated by the clock rather than by a role.

---

## 5. Work stages

### Stage 0 — Close out current state
- [ ] Re-run the test suite. The three audit fixes (zero-share deposit, admin lockout guard, bps bounds) compile clean but were **never test-verified** — the run was killed.

### Stage 1 — SPL foundation

Add dependencies:
```toml
pinocchio-token = "0.7.0"
pinocchio-associated-token-account = "0.4.0"
```

**Vault → USDC token account.** Today the vault is a program-owned account whose *lamport balance* is the money. Split those roles: a small `Vault` PDA acts as authority, and a separate token account holds USDC.

*This simplifies solvency.* Right now rent and real money share one balance, which is why the check has to add `rent_minimum`. With a token account, rent is paid in SOL and the balance is pure USDC:

```
old:  vault.lamports() >= rent_minimum + total_collected
new:  vault_token.amount >= total_collected
```

**One SPL mint per outcome.**
- Seeds: `[b"outcome_mint", market, outcome_index]`
- Mint authority: the market PDA
- **6 decimals** — matches USDC and the existing share scale (`MARKET_INIT_FEE = 10_000_000` = $10 already assumes this, so nothing needs rescaling)

**`Position` mostly dissolves.** Once shares are tokens, the `outcome_shares` array and the `claimed` flag are both redundant — balances replace the array, burning replaces the flag. This kills the double-claim bug class entirely: you can't burn the same token twice.

**Keep `Market.outcome_shares`** — that's AMM curve state, not user balances.

**Instruction changes:**

| Instruction | Change |
|---|---|
| `initialize_market` | Create N mints + vault token account; pull USDC creation stake; mint seed shares to creator |
| `place_bet` | `TransferChecked` USDC in; **mint** `delta` outcome tokens to bettor's ATA (create ATA if absent) |
| `claim` | **Burn** caller's winning tokens; `TransferChecked` pro-rata USDC out, vault PDA signing |
| `close_losing_position` | *(new)* Burn worthless tokens, close the token account, return its rent |

> **The one detail that will silently break everything:** payout must divide by the winning-side total **snapshotted at settlement**, never by live mint supply. Every burn shrinks supply, so later claimers would get progressively larger (wrong) payouts. Current code already reads `market_state.outcome_shares[winning_outcome]`, which is frozen at settle — keep it that way. Write a dedicated test with 3+ sequential claimers.

### Stage 2 — Timestamps and phases
- [ ] Add `primary_end: i64` to `Market`, set at creation to `bet_close_ts − 5h` (mind struct alignment)
- [ ] Add `PRIMARY_WINDOW_SECS: i64 = 5 * 60 * 60` to constants
- [ ] `place_bet` rejects when `now >= primary_end`
- [ ] Reject market creation where `bet_close_ts` isn't at least 5h in the future (otherwise the primary window is empty or negative)
- [ ] Remove any stored phase/status transition for primary close

### Stage 3 — Phoenix adapter
- [ ] Pin the exact Phoenix program version; integration-test against its real instruction surface
- [ ] `initialize_market` creates N Phoenix markets (outcome/USDC), creator funds the rent
- [ ] Phoenix market authority = market PDA
- [ ] Permissionless `close_secondary` instruction: time-gated on `now >= bet_close_ts`, CPIs into Phoenix to close and force-cancel
- [ ] Users reclaim escrowed assets from cancelled orders

**Cost check before committing:** N outcomes = N Phoenix markets = N × rent, paid by the creator on top of their creation stake. Measure this early — if it's expensive, market creation may be priced out of reach.

### Stage 4 — Settlement
- [ ] `settle_market` records `winning_outcome` only; no Phoenix dependency
- [ ] Claims open the moment resolution lands
- [ ] Decide the claim window policy and what happens to unclaimed funds afterward

### Stage 5 — SDK + frontend
- [ ] Port instruction builders from `tests/common/mod.rs` to TypeScript
- [ ] SDK exposes OpenStake concepts (`market.phase`, `market.outcomes`, positions, orders) — never raw Phoenix instructions
- [ ] Frontend: create / buy / view / trade / claim
- [ ] Phase computed client-side from timestamps, same formula as the program

---

## 6. Invariants to test

| Invariant | Why it matters |
|---|---|
| `total_collected == C(q_T)` (within floor-rounding dust) | Makes settlement value per share exactly `1/p_k` |
| Mint supply == `Market.outcome_shares[i]` after every trade | Detects mint/curve drift immediately |
| Sum of all payouts ≤ `total_collected` | Solvency. Already tested — must survive migration |
| Sequential claims all get the same per-share value | Catches the live-supply-vs-snapshot bug |
| `place_bet` rejects at `now >= primary_end` | The core deadline guarantee |
| No mint possible after `primary_end` | Fixed-supply invariant |

---

## 7. A property worth stating publicly

Because supply and pool are both fixed at primary close, **the settlement value per winning share is fully known before secondary trading begins**:

```
V_w(outcome k) = total_pool / q_k = 1 / p_k
```

Secondary trading is therefore not discovering the payout — it's trading *probability* against a **known, fixed prize**. Fair value of a share is simply `P(outcome) × V_w`.

This is unusual and genuinely useful: order book participants can compute exactly what they stand to win before placing an order.

---

## 8. Still open

- Claim window length, and policy for unclaimed funds
- Phoenix market rent cost — measure, then decide if the creator can bear it
- Oracle beyond the MVP multisig
- Token-2022 transfer hook (the real fix for kickoff transfer enforcement)
