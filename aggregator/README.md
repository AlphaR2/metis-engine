# metis-aggregator

The off-chain aggregation API (`metis-api`). Discovers prediction markets across a curated venue
network, serves them by category, and ranks them by price, liquidity and execution quality.

It discovers, prices and recommends, and can **build** (never sign or broadcast) buy, claim and
market-creation transactions for the caller's wallet. Building is off unless `EXECUTION_ENABLED` /
`MARKET_CREATION_ENABLED` are explicitly `true`.

## The `/v1` API — events first

`/v1` serves the canonical **event graph**: Event → Question → Outcome → Listing, rebuilt from
the market store after every refresh (`src/engine/`) and swapped in whole.

| Endpoint | What it returns |
|---|---|
| `GET /v1/events` | Event cards: metadata, and a headline question with each outcome's best price and venue. Filters: `category`, `domain`, `status` (`open` by default), `within_days`, `q`, `venue`, `min_venues`. Sort: `best`, `closing_soonest`, `volume`. |
| `GET /v1/events/{id}` | The full event: every question as a sentence, every outcome, every venue listing rated best-first with its breakdown |
| `GET /v1/events/{id}/questions/{qid}` | One question in full |
| `GET /v1/markets` | Every venue market, flat, each carrying `event_id`, `event_title`, `question_id` and `question_text` |
| `GET /v1/markets/{venue}/{id}` | One market with its event context |
| `GET /v1/categories` | Categories as the venues report them, with each venue's own names and event/market counts |
| `GET /v1/venues` | Enabled venues, their capabilities, health and staleness |
| `POST /v1/route` | The best venue to back one outcome at a size, from live quotes, with fees included |

Lists use opaque `cursor`s, and responses carry an `ETag` (`If-None-Match` gets a 304) and
`Cache-Control: max-age=30`. Ids are hashes of venue identifiers (`evt_…`, `q_…`, `o_…`), so they
stay the same across rebuilds and restarts. When a merge folds one venue's event into another,
the folded event's id still resolves.

**How events are built** (deterministic; no model calls):

- **Structure.** A matchup is one *winner* question (NO on one side backs the other). Exclusive
  options (candidates, ranges, temperature buckets; YES prices sum to about 1) are one *multi*
  question. Independent thresholds and deadlines are one yes/no question each.
- **Extraction.** Entities come from a seeded dictionary (assets, stocks, commodities, countries)
  plus participants the venues attach (Jupiter teams, with nicknames and abbreviations). Numbers
  are read with magnitudes, dates in any written form, and direction from wording. Questions are
  written as full sentences: "Will Bitcoin reach $90,000 by Dec 31, 2026?",
  "Bitcoin above 82,000 on September 24?".
- **Matching.** Exact canonical keys first (`px:bitcoin:above:90000:2026-12-31:touch`,
  `win:a|b:date`). Otherwise candidates are grouped by shared entity, then:
  - hard constraints are applied: different threshold, direction, subject, date or settlement
    style (hit-at-any-point vs price-on-the-date) means *different*;
  - a weighted score decides the rest: a pair merges only at 0.90 or above.

  A binary about one option ("Marco Rubio to win the nomination") folds into that option of a
  multi-choice question. Scored merges are marked `rules_differ` and shown side by side.
  Decisions are reviewable at `GET /admin/matches`; `POST /admin/matches/merge|split` overrides
  persist and apply on every rebuild.
- **Rating.** Only listings of the same outcome are compared: price (40), activity (10) and
  venue reliability (10), renormalised over the components a venue can provide.

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

Live-quote rankings use the **all-in** price per contract (fill price plus fee per contract). Panta
reports its fee in the quote; Jupiter's fee is only known once an order is built, so its quotes
carry a fee of 0 for now.

With `quote_size`, every market on the page is quoted, so the page is capped at 20. The response's
`limit` is the size actually applied — page on that. Quotes are cached for 5 seconds per
`(venue, market, side, size)`.

## The events path

`/events` and the canonical event graph (`services/matching`, `repositories/graph_repository`) are
retained and still work. They group markets across venues by title similarity into real-world
events, which pays off only once several venues quote the same event — not the situation today, so
it is no longer the primary path.

## Venues

| | Jupiter | Panta | Pascal |
|---|---|---|---|
| Status | **carries the catalog** | wired, near-empty | not enabled by default |
| Categories | 11 filter names in the adapter (no categories endpoint) | `GET /categories/` (8) plus what markets report (11 seen) | — |
| Sort | `volume`, `volume24hr`, `beginAt` | *ignored* | — |
| Pagination | offset, works | **cursor never advances** | none |
| Depth signal | `pricing.volume` | `volumeUsdc` | none |

Notes from verifying these live:

- **Panta's list rows can't be trusted alone.** They carry no text (`title` blank on 84 of 84
  rows) and their state lags: rows listed `primary`/`open` are often `cancelled` or `resolved` on
  the market's own record. The adapter reconciles every open-looking row with
  `GET /markets/{id}/`, adopting its question, rules, sources and state. At the Phase 0 capture
  that left **no** bettable Panta market. Its `nextCursor` is also a constant (six requests
  returned 300 rows holding 50 distinct markets); the refresh detects a non-advancing cursor and
  stops.
- **Both venues sometimes send timestamps as ISO-8601 strings** where they document integers.
  `models/venue/timestamp.rs` accepts either; a Jupiter category was silently failing to parse
  before that was handled.
- **The two category vocabularies don't match**, and Panta's categories are loose (Bitcoin price
  and rain markets filed under `sports`). Categories are kept as the venues send them and are for
  browsing only; cross-venue matching does not rely on them.
- **Jupiter rate-limits aggressively.** Category chains run one at a time per venue, paced
  (`JUPITER_MAX_CONCURRENT_CHAINS`, `JUPITER_MIN_REQUEST_GAP_MS`). A 429 is treated as
  backpressure and never marks the venue unhealthy. Venues refresh concurrently with each other.

## Venue integration

Venues are configured, not coded into the service: `ENABLED_VENUES` lists them and each reads its
own `<ID>_*` settings (key, base URL, page cap, pacing, circuit-breaker cooldown).
`services/registry.rs` is the only place that maps a venue kind to its adapter. Each adapter must
pass the venue contract tests (`src/venue_contract.rs`), which replay recorded responses from
`fixtures/<venue>/` over real HTTP through the real refresh. To add one, see
`../docs/adding-a-venue.md`. For what each venue actually sends, see `../docs/venue-fields.md`.

**Categories come from the venues.** Each venue reports the category names its filter accepts
(Panta reads `GET /categories/`; Jupiter, which has no such endpoint, keeps its list in the
adapter). The refresh runs one chain per name plus one **unfiltered sweep**, which finds
categories a venue doesn't advertise — Panta's markets report 11 categories against the 8 its
endpoint lists. Every market keeps its venue's category verbatim (`venue_category`), and
`category` is the shared name after aliasing (`CATEGORY_ALIASES`; built in:
`climate & science`→`science`, `tech`→`technology`, `pop-culture`→`culture`).
`GET /categories` lists what is actually held, with each venue's own names and counts.

## Refresh

A background timer (`REFRESH_INTERVAL_SECS`, default 600) re-pulls every venue and category and
replaces each `(venue, category)` bucket **wholesale** — so a market the venue stops listing
disappears rather than accumulating. `POST /refresh` triggers one manually; overlapping passes are
prevented by a lock, and a pass that overruns the interval delays the next tick rather than
stacking them.

A venue's catalog is only replaced when **every** chain completed. If any page fails (after
retries), the venue's previous catalog is kept — in memory and in Postgres — and marked stale, so
an outage never empties it. A category filter the venue rejects (a 4xx) is skipped, not treated
as an outage; its markets still arrive through the sweep. After repeated failures a venue's
circuit breaker skips it for `<ID>_BREAKER_COOLDOWN_SECS`, keeping its last good catalog.

Postgres keeps each venue's full catalog as one zstd-compressed JSON snapshot (`market_snapshots`),
so everything a venue sends — including fields not yet modelled, in `extra` — survives a restart.
One blob per venue rather than a row per market: nothing queries markets in SQL, and the catalog
only compresses well as a whole (~67 MB of rows vs a few MB).

`GET /markets` reports `last_refreshed_at` so a UI can show data age and tell an empty catalog from
an unprimed one, and `stale_venues` naming any venue whose markets are a last good copy.

## Persistence

Postgres is a durability layer, not the read path — every request is served from memory. Refresh
writes each venue's snapshot through; boot reads it back in the background (`warm_start`), so a restart serves the previous
catalog immediately instead of an empty list.

`DATABASE_URL` is optional: unset means in-memory only. Queries use the runtime `sqlx::query` API
rather than the `query!` macros deliberately, so `cargo build` never requires database access.

```bash
sqlx migrate run     # apply migrations as an explicit deploy step, not at startup
```

## Operator routes

`POST /refresh`, `GET /metrics` and the raw venue proxy `GET /venues/{venue_id}/raw/{path}` require
an `X-Admin-Token` header matching `ADMIN_TOKEN`. **They fail closed**: with `ADMIN_TOKEN` unset,
they refuse every request. The raw proxy is further limited to each venue's read-only catalog
paths (Jupiter `events`, `markets`, `orderbook`; Panta `markets`), rejects dot segments, and
returns the venue's own status code.

## Protecting the public routes

- **Rate limit:** each client IP gets `RATE_LIMIT_PER_MINUTE` tokens a minute (default 300, `0`
  disables). A request that takes live quotes costs 10. Over the limit is a `429` with
  `Retry-After`. Behind a proxy that overwrites `X-Forwarded-For`, set `TRUST_FORWARDED_FOR=true`.
- **Timeout:** any request running longer than `REQUEST_TIMEOUT_SECS` (default 45) gets a `408`.
- **Venue health:** a venue's `429` is backpressure, not an outage — it never counts toward the
  failure streak that takes a venue out of routing.
- **Shutdown:** `SIGTERM`/Ctrl-C drains in-flight requests before exit.

## Metrics

`GET /metrics` (admin): request latency by route and status, refresh duration, venue errors by
kind, per-bucket age and staleness, and DB pool use.

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
engine/         the events engine: text, dictionary, extract, build, matching, rating, graph
repositories/   storage - market store (+ its Postgres mirror), overrides, legacy graph, health, quote cache
models/         data shapes only, no behavior; venue wire formats under models/venue/
errors/         AdapterError, RoutingError, and the HTTP-facing ApiError they map onto
```

`AdapterError::is_business_outcome()` is the seam that keeps "this market has no depth" from being
counted as a venue failure — without it, a run of thin markets drops a healthy venue out of routing.
