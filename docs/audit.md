# Metis — Codebase Audit

**Date:** 2026-09-24
**Scope:** the whole `engine/` workspace, with a deep pass on `aggregator/` (Batch 1).
**Method:** manual code read plus the test suites and clippy. No fixes applied yet; this is the
work list.

Severity: **High** = wrong answers, data loss, or abuse reachable by an anonymous caller.
**Medium** = incorrect under realistic conditions, or a meaningful gap. **Low** = hygiene / drift.

---

## Batch 1 — Aggregator (`aggregator/`)

The overall shape is good: clean adapter trait, pure `maths/` engines, fail-closed operator
routes and money-moving flags, `Decimal` money, non-custodial execution, and 121 passing unit
tests with clean clippy. The problems are in the refresh lifecycle, request-path concurrency,
the public surface area, and ranking semantics.

### High

**A1. A venue outage wipes that venue's catalog, in memory and in Postgres.**
`aggregator.rs:247` — `collect_pages` `break`s on a page failure and returns whatever it has
(often nothing). `refresh_category` (`aggregator.rs:147-148`) then replaces the bucket
wholesale with that result and persists it, and `replace_bucket` deletes the key on empty. One
bad refresh (429 storm, key expiry, Jupiter blocking the host's IP) empties `/markets` for that
venue and deletes its rows from Postgres, so the warm-start safety net is gone too. A failure on
page 4 of 10 silently truncates the bucket to 3 pages.
*Fix:* make `collect_pages` return `Result<Vec<_>, _>` (or a `complete: bool`). Only replace
the bucket on a complete chain; on failure keep the previous bucket and mark it stale.
Consider a per-bucket `refreshed_at` instead of the single store-wide `last_refreshed_at`.

**A2. `GET /markets?quote_size=` holds the store's read lock across live network quotes.**
`api/v1/markets/routes.rs:135` takes `markets.read()`, and the guard lives through
`rank_with_quotes(...).await` (`:158`), which is up to 20 venue calls with a 30s timeout each.
`tokio::sync::RwLock` is write-preferring, so as soon as the refresh queues a `write()`, every
other `/markets` request blocks behind it until the slow quote request finishes. One slow venue
plus one refresh stalls the whole read API.
*Fix:* clone the shortlist (`Vec<VenueMarket>`) and drop the guard before any `.await`.

**A3. Public raw proxy lends our venue API keys to anyone, for any path.**
`api/v1/route.rs:20` mounts `/venues/{venue_id}/raw/{*path}` on the **public** router.
`jupiter/client.rs:196-198` (and Panta's equivalent at `panta/client.rs:142`) does
`format!("{base_url}/{path}")` with the caller's path. Axum percent-decodes the wildcard, so
`%2e%2e/` segments walk out of `/prediction/v1` to any other endpoint on `api.jup.ag` under our
`x-api-key`. Even without traversal, anonymous callers can burn our rate limit (which then feeds
A5) and read any account-scoped GET endpoint the keys can reach.
*Fix:* move it behind `require_admin_token`, reject `..`/encoded dots, and ideally allow-list
path prefixes (`markets`, `events`, `orderbook`). Pass the upstream status code through.

**A4. `/markets` "best price" / "best overall" compares prices across *different* markets.**
`services/maths/markets.rs:29` sorts by the lowest YES price across unrelated markets. A $0.02
long shot is not a "better price" than a $0.60 favourite, it's a different bet. The result is
that `sort=best_price` and `sort=best_overall` (`-price × 10` dominates the blend) mostly mean
"least likely outcomes first". "Best price" only means something **across venues for the same
outcome**, which is what `/find-best-market` does.
*Fix (product call):* on `/markets`, drop or rename these sorts (e.g. `lowest_probability`),
or rank on spread/depth/volume. Keep price-based ranking for same-outcome venue comparison only.

### Medium

**A5. Anonymous callers can knock a venue out of routing.** Every `/markets?quote_size=`
request fans out up to 20 outbound quotes (`services/markets/service.rs`), with no rate limiting
on any public route. A 429 isn't a "business outcome" (`errors/adapter_error.rs`), so it counts
toward `HealthRegistry`'s 3-strike rule and flips the venue to unroutable. The README itself
notes a 429 looks the same as an outage.
*Fix:* add a per-IP rate limit (tower-governor or similar) and a short-TTL quote cache keyed on
`(venue, market, side, size bucket)`. Treat 429 as backpressure, not a health failure.

**A6. `quote_size` pagination drops markets.** `api/v1/markets/routes.rs:157` quotes
`limit.min(20)` markets from `offset`, so `limit=50&offset=0` returns ≤20. The next page starts
at 50, so items 20–49 are never shown. `total` also still counts the unquoted set.
*Fix:* clamp `limit` to `MAX_LIVE_QUOTES` when `quote_size` is set (and say so in the
response), or advance by the number actually considered.

**A7. The event graph grows forever.** `services/matching/service.rs:113` only inserts;
`CanonicalGraph` has no removal or pruning. Every refresh adds the whole catalog (4000+ Jupiter
markets per category), so memory grows until restart and `/events` serves long-dead markets.
The market store's own docs call this out as a known failure mode.
*Fix:* rebuild the graph from the current store at the end of each refresh, or prune by
`resolution_time` / last-seen.

**A8. Panta status polling always reports "Failed".** `api/v1/execution/routes.rs:238` polls
with an empty signature, which for Panta calls `/markets/register/` with `signature: ""`
(`panta/create.rs:251`). Panta can only reject that, so the non-2xx branch maps it to `Failed`
(or `Expired`). A pending or confirmed creation shows as failed. `RATE_LIMITED` also maps to
`Failed`.
*Fix:* give the adapter trait a separate `status()` that never registers; return `Unsupported`
for Panta until it has a read-only status endpoint. Map `RATE_LIMITED` to a retryable status.

**A9. User-controlled IDs are spliced into venue URL paths.** `jupiter/execution.rs:215`
(`/orders/status/{order_pubkey}` from the route's `venue_ref`), plus `get_market`/`orderbook`
with `venue_market_id`. Same traversal class as A3, on a narrower surface.
*Fix:* validate format (base58 pubkey / known market-id pattern) or build URLs with
`Url::path_segments_mut().push()`, which escapes `/` and `..`.

**A10. `/positions` is an unauthenticated in-memory write.** Anyone can record positions for
any wallet, reads are public by wallet, nothing is persisted (the `positions` table exists but
is unused), and the index grows without bound.
*Fix:* either remove it until it's backed by on-chain/venue data, or require a wallet signature
and persist it with a cap.

**A11. Live quotes ignore fees.** Jupiter's `quote()` hard-codes `fee_usdc: 0`
(`jupiter/service.rs:160`) even though its order builder reports protocol and venue fees, and
none of the scorers include `fee_usdc` anyway. Rankings can therefore prefer a venue that's more
expensive after fees, and the quoted price won't match the built order.
*Fix:* rank on all-in cost (`avg_price + fee / size`) and populate fees where the venue exposes
them.

**A12. The refresh timer bursts after a long pass.** `app.rs:87` uses `tokio::time::interval`
with the default `MissedTickBehavior::Burst`. With a 3–4 minute refresh and a small interval
(minimum 30s), missed ticks fire back-to-back, so refreshes run continuously.
*Fix:* `ticker.set_missed_tick_behavior(MissedTickBehavior::Delay)`.

### Low

- **A13.** `strategy_by_name` (`services/routing/service.rs:78`) silently falls back to
  `best_overall` on a typo, while `/markets` returns 400. Make them consistent.
- **A14.** A market that changes category shows in both buckets until the old category refreshes;
  uncategorised venues never clear categories that disappear entirely.
- **A15.** No graceful shutdown (`axum::serve` without `with_graceful_shutdown`, even though
  tokio's `signal` feature is enabled). No request-level timeout layer on public routes.
- **A16.** Metrics are thin: no request latency/status, refresh duration, per-venue quote error
  counters, or bucket staleness. `DB_POOL_IN_USE` is only updated when `/metrics` is scraped.
  The `metrics.rs:4` comment says unauthenticated, but it's actually gated.
- **A17.** Unused schema: the `events`, `outcomes`, `venue_market_refs`, `venue_health` and
  `positions` tables are never read or written.
- **A18.** Doc drift: README says the refresh default is 300s (config uses 600) and that the
  service is "read-only" (it now builds orders, market creations and claims behind flags).
- **A19.** Test gaps: nothing covers refresh failure handling (A1), lock behaviour (A2), the
  HTTP layer (no axum route tests), or DB round-trips. Adapters are only tested on parsing.

### Proposed Batch 1 order

1. **Correctness:** A1 (keep last good bucket), A2 (drop lock before await), A6, A12.
2. **Lock down the public surface:** A3 (gate raw proxy), A9 (URL-safe IDs), A5 (rate limit +
   quote cache, 429 ≠ unhealthy), A10 (remove or authenticate positions).
3. **Ranking semantics:** A4 (a product decision is needed on what `/markets` sorts mean), A11
   (fee-inclusive scoring), A13.
4. **Lifecycle and ops:** A7 (graph pruning), A8 (status vs confirm), A15, A16.
5. **Tests for every item above**, plus axum route tests via `tower::ServiceExt::oneshot`.
6. **Cleanup:** A14, A17, A18.

---

## Later batches — On-chain programs (`programs/`, `authority/`)

Recorded now so they aren't lost; not in Batch 1.

### High

- **P1. Settlement is fully trusted and allowed at kickoff.** `settle_market.rs` takes
  `winning_outcome` as a bare argument, never reads `market.oracle`, and allows settlement once
  `now >= bet_close_ts` (kickoff, not full time). Any one root key or the market's delegate can
  settle, with no dispute window or undo.
- **P2. A revoked delegate can still settle.** `settle_market` compares against the stored
  `market_state.delegate` without re-checking the `DelegatedAuthority` account, so
  `remove_authority` doesn't reach markets that delegate was already assigned to.
- **P3. Funds can be locked forever.** `MarketStatus::Cancelled` exists, but
  `can_transition_to` only allows Open → Settled (`amm/state.rs`). An abandoned match, or no
  settlement at all, leaves USDC stuck in the vault.
- **P4. Admin is 1-of-3, and pause does nothing.** `update_config` accepts any single root
  signer (`is_root`). `RootAuthority.paused` is never checked by `programs/core`.

### Medium / Low

- **P5.** The trade cap and 10pp impact ceiling apply per instruction, so splitting one bet into
  many `place_bet` instructions in one transaction gets around both.
  `max_in_bps_of_vault = 0` disables the cap entirely.
- **P6.** `fee_bps` is validated but never charged (already known, `plan.md` item 5).
- **P7.** Oracle: any attested updater can update *any* oracle account;
  `football/state.rs` uses `.expect()`/`.unwrap()` on the clock and on state transitions.
- **P8.** The displayed price (curve over real + virtual shares) isn't the payout (pari-mutuel
  over real shares). The UI and SDK need to explain this.
- **P9.** A `bet_close_ts` of exactly now + 5h creates a market that's closed at birth.
  `place_bet`/`claim` use `from_bytes`, which aborts on a wrong-sized account rather than
  returning an error (still safe because the owner check runs first).
- **P10.** Doc drift: the core README still describes a single hardcoded authority; `mvp.md`
  lists features as unimplemented that now exist.

### Test status (2026-09-24)

| Suite | Result |
| --- | --- |
| `metis-aggregator` unit tests | 121 passed; clippy clean |
| `metis-core --lib` | 42 passed |
| `metis-authority` | 8 passed |
| `metis-oracle` | 7 passed, 5 failed (all because `target/deploy/metis-oracle.so` wasn't built, not logic failures) |
| `metis-core` litesvm integration | not run (needs `cargo build-sbf`); only one happy-path test exists |

### Hygiene

The workspace isn't a git repository yet, so `.gitignore` protects nothing today.
`aggregator/.env` and `programs/core/keys/authority.json` exist on disk; keep them out of the
first commit. Root, guardian and oracle addresses in `constants.rs` are placeholders.
