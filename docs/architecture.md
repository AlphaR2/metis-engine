# Metis — how it works now

This is the current design. [mvp.md](mvp.md) and [plan.md](plan.md) are the earlier plans and are
kept for their reasoning, but where they disagree with this file, this file is right.

## What it is

A football prediction-market engine on Solana, made of:

| Piece | What it does |
|---|---|
| `programs/core` (`metis-core`) | The market program: a pari-mutuel AMM that mints outcome shares from a bonding curve, then settles from the oracle and pays winners from the real pot. |
| `programs/oracle` (`metis-oracle`) | Match results on chain. A result only exists once two independent reporters agree. |
| `authority` | Shared trust code: three root signers, one guardian, delegated attestations, pause/unpause. Both programs use it. |
| `tools/oracle-keeper` | Off-chain: the **keeper** reads sports-data providers and reports to the oracle; the **crank** moves every market through its life. |
| `tools/metis-ops` | Setup and operator commands: trust accounts, attestations, market creation, claims, pause/unpause. |
| `aggregator` | The HTTP service. `metis` is one venue next to Panta and Jupiter: catalog, quotes, unsigned buy/sell/claim/create transactions. |
| Manifest (external) | The order books where holders trade shares after betting closes. Not ours; the real, audited program. |

## One market's life

```
create ──► Open ──(primary_end)──► Closed ──(oracle final + dispute window)──► Settled ──► closed away
                │                     │                                          │
                └─────────────────────┴──► Cancelled (void / no result / two roots)┘
```

1. **Create.** The creator picks an existing oracle fixture, a condition on the 90-minute score
   (home win, away win, draw, total goals over N, both teams score) and seeds the pot with at
   least 30 USDC (`MIN_SEED`). `initialize_market` takes the kickoff from the oracle, which must
   be a real, still-scheduled match at least `PRIMARY_WINDOW_SECS` (5 h) ahead. The question,
   the YES/NO rubrics and the labels are written by `set_metadata`, a second transaction (the text
   doesn't fit beside the market). Binary only; NO means "not YES".
2. **Bet** until `primary_end` (kickoff − 5 h). `place_bet` takes the trade fee first, then mints
   shares off the curve `cost = sqrt(Σ q²)` with virtual liquidity (k = 30, ceiling 100). The
   quote the aggregator shows is the honest one: the break-even probability from what the pot
   actually pays, not the curve's price.
3. **Books open.** `close_primary` (operator) revokes mint and freeze authority on both outcome
   mints and creates two Manifest order books by CPI (Team A/USDC, Team B/USDC) at addresses
   derived from the market. Holders trade there; the aggregator buys and sells through Manifest
   swaps.
4. **Result.** The oracle finalises when two reporters agree on the 90-minute score. It is
   immutable after that, except for a two-root override inside 24 h.
5. **Settle.** After the market's own dispute window, `settle_market` works out the winner from
   the oracle and the market's condition (nobody passes a winner), records the rake and what each
   party is owed, and sets the payout pool. Winners `claim`: `shares × payout_pool / winning
   shares`.
6. **Or cancel.** A voided oracle, or no result `CANCEL_AFTER_SECS` after kickoff, cancels; two
   roots can also cancel any unsettled market. Everyone is refunded pro rata over all shares,
   fees included (`claim_refund`).
7. **Wind down.** The protocol's share is swept to the treasury (operator). The creator claims
   their own fees (anyone-signed, only ever to the creator). After `CLAIM_WINDOW_SECS` (90 d)
   `close_market` returns the rent and sends any remaining dust, **including winnings nobody
   claimed**, to the treasury.

## What is hardcoded and what lives in Config

The rule: **hardcoded = who to trust and every limit; Config = business dials, each checked
against a hardcoded bound in both `initialize_config` and `update_config`.** A compromised root
key can move a dial inside its bounds; it can never move a bound, the roots, the guardian, a
program id or a seed.

| Hardcoded (compile time, per network) | Value |
|---|---|
| Root signers (3) and guardian | `authority/src/network.rs`; build features `devnet`, `mainnet`, `localnet` |
| Program ids, seeds, discriminators | in `constants.rs` |
| `MIN_SEED` | 30 USDC |
| `MAX_VIRTUAL_LIQUIDITY_MULTIPLE` | 100 |
| `MAX_FEE_PRIMARY_BPS` / `MAX_RAKE_BPS` | 50 (0.5%) / 300 (3%) |
| `MAX_PROTOCOL_SHARE_BPS` | 5,000 |
| `MAX_PRICE_IMPACT_BPS` | 1,000 |
| `PRIMARY_WINDOW_SECS` | 5 h before kickoff |
| `MIN_DISPUTE_SECS` / `MAX_DISPUTE_SECS` | 15 min / 24 h |
| `CANCEL_AFTER_SECS` | 7 d after kickoff |
| `OPERATOR_GRACE_SECS` | 7 d |
| `CREATOR_CLAIM_SECS` | 30 d |
| `CLAIM_WINDOW_SECS` | 90 d |
| Oracle: reporters needed / kickoff lead / override window / close grace | 2 / 1 h / 24 h / 30 d |

| Config (dials, roots update within bounds) | |
|---|---|
| `market_init_fee` (the seed), `virtual_liquidity_multiple`, `dispute_secs`, `fee_bps`, `rake_bps`, `max_in_bps_of_vault`, fee split (protocol / backers / creator, default 50/30/20), `max_outcomes`, `dbc_backing_enabled` (off) | each snapshotted onto a market when it is created, so a change only affects later markets |
| `treasury`, `quote_mint`, `dbc_config` | high friction: `update_config_critical` needs **two distinct roots** |

The `localnet` build shortens the timers (primary window 120 s, dispute 20 s, grace 120 s,
creator claim 180 s, claim window 300 s, cancel after 600 s; oracle lead 30 s, override 120 s) so
a whole life fits in minutes. It also uses throwaway keys. It is never deployed anywhere real.

## Who may do what

| Step | Who |
|---|---|
| Create a market, bet, claim, refund, creator claim | anyone (their own money only) |
| `close_primary`, protocol `sweep_fees`, `close_market` | a root or an **attested operator** |
| `settle_market`, `cancel_market` (void / overdue) | an operator, **or anyone after `OPERATOR_GRACE_SECS`** |
| Cancel any unsettled market | two distinct roots (works while paused) |
| `forfeit_creator_fees` | two distinct roots, ≥ 30 d after settlement |
| `update_config` | one root, inside the bounds |
| `update_config_critical`, `initialize_config` | two distinct roots |
| `pause` | one root. `unpause` | guardian + two distinct roots, one transaction |
| Oracle: report a result | an attested reporter (two roots attested it) |
| Oracle: override or void | two distinct roots, within 24 h of the result |

An **operator** is a key with a completed `DelegatedAuthority` account (two roots attested it). The
crank runs on one. It is never a root key. Pausing stops creating, betting, settling, sweeping and
closing; it never stops claims, refunds or the two-root cancel, so nobody's money is trapped.

**Forfeit.** A creator who has not collected their fees 30 days after settlement can lose them to
the treasury, but only by two roots, and the amount and time stay on the market
(`forfeited_creator_fees`, `forfeited_at`) so a manual settlement is possible later. Nothing
automatic ever forfeits; the crank only lists candidates (`oracle-keeper forfeitable`).

## Fees

`fee_bps` is taken on every bet before the curve. `rake_bps` is taken from the pot at settlement.
Both are split protocol / backers / creator, 50/30/20 by default; while the DBC backer raise is
off, the backers' share goes to the creator. The protocol's share is swept by the operator; the
creator's stays in the vault until they claim it (so `close_market` waits for it, or for a forfeit).

## The aggregator's `metis` venue

Reads the program's accounts straight from chain (no API), quotes in payout terms, builds the
unsigned transactions: buy (`place_bet`), sell (a Manifest swap), claim or refund (with the maker's
Manifest seat balances withdrawn first), creator-fee claim, and market creation in two steps
(`POST /execution/markets` with `venue_params.step` = `create`, then `metadata`). The wallet is
the only signer; the aggregator never signs or broadcasts. Catalog prices are break-even
probabilities for a stated bet size; on the books they come from the cheapest ask, and are absent
when nobody is selling.

## Running it

- Local rehearsal on a real validator: [localnet/README.md](../localnet/README.md) and `just localnet-*`.
- Devnet / mainnet: [deploy/deploy.sh](../deploy/deploy.sh) (build, deploy, init, attest, verify).
- Keeper and crank: [runbook.md](runbook.md).
- Review notes and open decisions: [security-review.md](security-review.md).
- The crank costs real SOL: [fee-layer-proposal.md](fee-layer-proposal.md).
