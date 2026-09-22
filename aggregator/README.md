# metis-aggregator

The off-chain aggregation API (`metis-api`). Discovers prediction markets across a curated venue
network, serves them by category, and ranks them by price, liquidity and execution quality.

Read-only today: it discovers, prices and recommends. It does not execute trades.

## The markets path

`GET /markets` is the primary endpoint. It serves **raw venue markets, untransformed** — what the
venue reported is what a caller gets, plus a `venue_id` saying where it came from.

```
GET /markets?category=sports          # required
             &within_days=7           # default 7; filters on close_time
             &sort=best_overall       # closing_soonest (default) | best_price
                                      #   | best_liquidity | best_execution | best_overall
             &side=yes                # default yes
             &venue=jupiter           # optional
             &quote_size=100          # optional: live-quote the top candidates
             &limit=50&offset=0
```

`GET /categories` lists the valid `category` values, since it's a required filter.

### Three different times

Venues report three distinct timestamps and conflating them produces wrong answers, so the model
keeps them separate:

| Field | Meaning | Panta | Jupiter |
|---|---|---|---|
| `start_time` | market opened | `startTime` | `openTime` |
| `close_time` | **stops accepting bets** | `endTime` | `closeTime` |
| `resolution_time` | outcome settled | `resolutionTime` | `resolveAt` |

`within_days` filters on **`close_time`** — "stops taking bets within N days". Close and resolution
are genuinely different moments: on a 50-market Panta sample they differed on 38 of them, by
anything from hours to five days. A market whose betting has closed but which hasn't settled yet is
not tradeable, so it is excluded.

A `close_time` of `0` means the venue reports no cutoff at all (Jupiter does this for open-ended
markets — "closes after a winner is declared", 28 of 1063 on a live sports page). Those are
excluded from a bounded window rather than padding results with markets we can't prove belong
there.

### Ranking

Two tiers, so a list view costs nothing but a ranked trade decision is still accurate:

- **Catalog (default)** — ranks on what the venue's listing already gave us (price, reported
  volume). Pure in-memory, no network calls. `services/maths/markets.rs`.
- **Live quotes (`quote_size=`)** — re-prices the shortlist the catalog sort produced by quoting
  each market for real, concurrently, then ranks with the *same* scorers `/find-best-market` uses
  (`services/maths/routing.rs`). The response says which was used via `scored_with`.

Markets missing the field a sort ranks on always sort **last**, never as zero — an unpriced market
is not free, and a market with no reported volume is not empty.

## The events path

`/events` and the canonical event graph (`services/matching`, `repositories/graph_repository`) are
retained and still work. They group markets across venues by title similarity into real-world
events, which pays off only once several venues quote the same event — not the situation today, so
it is no longer the primary path.

## Venues

| | Jupiter | Panta | Pascal |
|---|---|---|---|
| Status | **carries the catalog** | wired, near-empty | not enabled by default |
| Categories | crypto, sports, politics, esports, culture, economics, tech, finance, climate & science, weather | sports, crypto, politics, entertainment, finance, science, world | — |
| Sort | `volume`, `volume24hr`, `beginAt` | *ignored* | — |
| Pagination | offset, works | **cursor never advances** | none |
| Depth signal | `pricing.volume` | `volumeUsdc` | none |

Notes from verifying these live:

- **Panta's catalog is currently near-empty** — 69 markets across all 8 categories, of which about
  one is live, with no title, price or volume. It is wired correctly and will serve data the moment
  Panta fills up. Its list endpoint also ignores every filter and sort param (two identical
  `status=primary` calls returned different results), and its `nextCursor` is a constant — six
  requests returned 300 rows containing 50 distinct markets. The refresh detects a non-advancing
  cursor generically and stops.
- **Both venues sometimes send timestamps as ISO-8601 strings** where they document integers.
  `models/venue/timestamp.rs` accepts either; a Jupiter category was silently failing to parse
  before that was handled.
- **The two category vocabularies don't match** — only sports/crypto/politics/finance exist on
  both. `Category` is the canonical superset; each venue's model translates it and returns `None`
  for categories it lacks, so a category one venue doesn't have never breaks the other.
- **Jupiter rate-limits aggressively.** Category chains run one at a time per venue, paced, because
  a 429 is indistinguishable from an outage to `HealthRegistry` — an over-eager refresh could mark
  a healthy venue unroutable purely by hammering it. Venues still refresh concurrently with each
  other.

## Refresh

A background timer (`REFRESH_INTERVAL_SECS`, default 300) re-pulls every venue and category and
replaces each `(venue, category)` bucket **wholesale** — so a market the venue stops listing
disappears rather than accumulating. `POST /refresh` triggers one manually; overlapping passes are
prevented by a lock.

`GET /markets` reports `last_refreshed_at` so a UI can show data age and tell an empty catalog from
an unprimed one.

## Persistence

Postgres is a durability layer, not the read path — every request is served from memory. Refresh
writes each bucket through; boot reads it back (`warm_start`), so a restart serves the previous
catalog immediately instead of an empty list.

`DATABASE_URL` is optional: unset means in-memory only. Queries use the runtime `sqlx::query` API
rather than the `query!` macros deliberately, so `cargo build` never requires database access.

```bash
sqlx migrate run     # apply migrations as an explicit deploy step, not at startup
```

## Operator routes

`POST /refresh` and `GET /metrics` require an `X-Admin-Token` header matching `ADMIN_TOKEN`. A
refresh is a multi-minute fan-out across every venue, so it isn't something an anonymous caller
should trigger. **They fail closed**: with `ADMIN_TOKEN` unset, both refuse every request.

## Running it

```bash
cp .env.example .env    # then fill in PANTA_API_KEY / JUPITER_API_KEY
cargo run -p metis-aggregator --bin metis-api
```

Swagger UI at `/swagger-ui`. Note Jupiter blocks US and South Korea IPs entirely, regardless of key.

```bash
cargo test -p metis-aggregator
cargo clippy -p metis-aggregator --all-targets
```

## Layout

```
api/v1/         axum routes, one module per feature, mirroring the OpenAPI tags
services/       business logic
  <vendor>/     service.rs = trading decisions, client.rs = raw HTTP only
  maths/        pure numeric engines, no I/O (markets, routing, orderbook, matching)
repositories/   storage - market store (+ its Postgres mirror), event graph, health, positions
models/         data shapes only, no behavior; venue wire formats under models/venue/
errors/         AdapterError, RoutingError, and the HTTP-facing ApiError they map onto
```

`AdapterError::is_business_outcome()` is the seam that keeps "this market has no depth" from being
counted as a venue failure — without it, a run of thin markets drops a healthy venue out of routing.
