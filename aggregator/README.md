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
| `GET /v1/markets/{venue}/{id}` | One market with its event context, from the last refresh |
| `GET /v1/markets/{venue}/{id}/live` | The same market read from the venue right now (`held` says whether the catalog also has it; `fetched_at` is when). Costs a venue call, so it is rate-limited like a live quote |
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

## Positions, claims and sells

| Endpoint | What it does |
|---|---|
| `GET /v1/positions?wallet=` | A wallet's holdings across venues, normalised: side, shares, cost, value, pnl where the venue reports them, `state` (`open`, `claimable`, `claimed`, `lost`, `cancelled`), a `claim_ref` when claimable, and `can_sell`. |
| `POST /execution/claims` | An unsigned claim transaction. `venue_ref` is the position's `claim_ref`. |
| `POST /execution/sells` | An unsigned sell (Jupiter). Opens an order that a keeper fills, tracked like a buy. Panta has no sell: its API trades the primary phase only, so a holder waits for resolution and claims. |

**A venue that could not be read is not a venue where the wallet holds nothing.** Each venue is read
on its own with its own timeout. A failure goes in `venues_failed` and the list is then incomplete;
a client must say so and never present it as everything the wallet holds. Only complete results are
cached (8 seconds); a partial one never is. A wallet with more positions than a venue's page cap
(Jupiter 1000, Panta about 200) is reported as a failure, not silently truncated. A row that names a
different owner than the one asked for fails the whole read.

`value_usdc` is the venue's mark where it gives one (Jupiter). Where it does not (Panta), an open
position is valued from the catalog price and flagged `value_is_estimate: true`: a spot price is not
what a sale fetches, and the catalog is minutes old.

**Panta buys and claims** return instructions and a blockhash, not a transaction. The adapter
compiles them into one unsigned v0 transaction (`src/utils/solana_tx.rs`). It refuses, and builds
nothing, if an instruction invokes a program outside the allowlist (Panta's program plus system,
token, associated-token, compute-budget and memo), asks for a signer other than the wallet, uses an
address lookup table, or would not fit a packet. The encoder is cross-checked byte for byte against
the official Solana crate in its tests.

**Jupiter transactions are inspected, not just forwarded.** Jupiter returns a finished transaction, so the
adapter reads it (`utils/solana_tx.rs`, `verify_venue_transaction`) before it leaves this service, for every
buy, sell and claim. It is refused unless the wallet is the fee payer and the only signer, every program it
invokes is Jupiter's issuer program (`2sVcg2dBSUzXkmdZ8M5cp1LbnzDrWJmr6hktkHwB8nY3`, from Jupiter's developer
docs) or one of the common supporting programs, and it contains no top-level token transfer, delegate approval,
authority change or account reassignment. SOL may move only in rent-sized amounts (at most 5,000,000
lamports per System instruction), the same bound the web client enforces. A refusal is logged at error level with the reason. If
Jupiter starts using another program, add it with `JUPITER_EXTRA_PROGRAMS` (comma-separated) rather than
loosening the check. **This has not yet been run against a live Jupiter order.** Before enabling execution,
build one with `scripts/dry-run-order.sh` and confirm it passes; the guard fails closed, so a wrong
assumption shows up as a refused build, not a bad transaction. It cannot prove Jupiter's own program is
honest: the cost and payout preview stays what the user approves.

**Two different truths.** The chain says whether the money moved; the venue says whether it
registered the action. A venue-side `failed` on `POST /execution/confirm` can be a registration
problem while the transaction landed, so a client decides money outcomes from the signature on-chain
and uses the venue's status for the rest. A transport error or a 5xx on a Panta submit is returned as
an error, never as `failed`, because it says nothing about the transaction (submit is idempotent on
`orderId` + signature, so retrying is safe).

## Errors

Every error is `{ "error": "text", "code": "CODE", "retryable": bool, "venue_code": "..."? }`. Route
on `code`; `error` is for display and its wording may change. Codes are additive and never renamed:

| Code | Meaning | Client action |
|---|---|---|
| `INVALID_REQUEST` | The request is malformed or out of range | Fix it |
| `NOT_FOUND`, `MARKET_NOT_FOUND` | It does not exist | Stop |
| `UNKNOWN_VENUE` | No such venue is enabled | Stop |
| `UNSUPPORTED` | The venue does not offer this action | Hide the action |
| `EXECUTION_DISABLED`, `MARKET_CREATION_DISABLED` | Switched off on this deployment | Show trading as unavailable |
| `PRICE_MOVED` | Below the caller's `min_shares_out` | Re-quote, ask the user again |
| `QUOTE_STALE` | The venue's quote expired or its curve moved past the tolerance | Re-quote |
| `NO_LIQUIDITY` | No depth at this size | Ask for less |
| `ORDER_MIN_SIZE` | Under the venue's minimum order | Raise the amount |
| `MARKET_NOT_TRADABLE` | Open but not tradable through the API (Panta past its primary phase) | View only |
| `NOT_CLAIMABLE` | Nothing to claim | Refresh positions |
| `TRANSACTION_EXPIRED` | The venue's window closed | Rebuild |
| `RATE_LIMITED` | Too many requests | Wait `Retry-After` |
| `VENUE_UNAVAILABLE` | The venue failed | Retry later |

`retryable` is true only where sending the identical request later can succeed. `QUOTE_STALE` and
`PRICE_MOVED` are not: a blind resend is the wrong response, a re-quote is the right one. When a venue
names its own failure (`QUOTE_EXPIRED`, `NOT_CLAIMABLE`, `TX_FEE_MISMATCH`), that word is passed
through verbatim as `venue_code`.

## CORS and rate limits

`CORS_ORIGIN` takes one origin or a comma-separated list, each exactly `scheme://host[:port]`. A
wildcard, a path or a trailing slash is refused at startup. Methods are `GET`, `POST`, `OPTIONS` only;
`Retry-After` and `ETag` are exposed so a browser client can read them.

Requests cost tokens from a per-IP bucket: memory reads 1, an action status poll 2, and anything that
calls a venue (`POST /v1/route`, `/live`, `POST /execution/*`, `/v1/positions`) 10. Behind a proxy set
`TRUST_FORWARDED_FOR=true` and `TRUSTED_PROXY_HOPS` to the number of proxies that append to
`X-Forwarded-For`. The client address is that many entries from the **right**; the leftmost entry is
client-supplied and is never used.

## Market times

Venues report three distinct timestamps and conflating them produces wrong answers, so the model
keeps them separate:

| Field | Meaning | Panta | Jupiter |
|---|---|---|---|
| `start_time` | market opened | `startTime` | `openTime` |
| `close_time` | **stops accepting bets** | `endTime` | `closeTime` |
| `resolution_time` | outcome settled | `resolutionTime` | `resolveAt` |

`within_days` filters on **`close_time`** - "stops taking bets within N days". Close and resolution
are genuinely different moments: on a 50-market Panta sample they differed on 38 of them, by
anything from hours to five days. A market whose betting has closed but which hasn't settled yet is
not tradeable, so it is excluded.

A `close_time` of `0` means the venue reports no cutoff at all (Jupiter does this for open-ended
markets - "closes after a winner is declared", 28 of 1063 on a live sports page). Those are
excluded from a bounded window rather than padding results with markets we can't prove belong
there.

The earlier `/markets`, `/categories`, `/events`, `/events/{id}/markets` and `/find-best-market`
routes, and the title-similarity graph behind them, were removed. `/v1` replaces each of them;
`POST /v1/route` replaces `/find-best-market` and takes a `question_id` rather than an `event_id`.

## Venues

| | Jupiter | Panta |
|---|---|---|
| Status | carries most of the catalog | markets found on-chain, described by the API |
| Categories | 11 filter names in the adapter (no categories endpoint) | `GET /categories/` (8) plus what markets report (11 seen) |
| Sort | `volume`, `volume24hr`, `beginAt` | *ignored* |
| Pagination | offset, works | cursor, advances |
| Depth signal | `pricing.volume` | `volumeUsdc` |

Notes from verifying these live:

- **Panta's list is a registry, not the whole chain.** As of 2026-09-24 mainnet held 191 `Event`
  accounts and the API reached 99, including 5 of the 8 live ones. So discovery reads the chain
  (`getProgramAccounts` on the Panta program, `PANTA_RPC_URL`; `PANTA_DISCOVERY=api` opts back
  into the API list): the chain decides which markets exist and their state, and the API's record
  by id supplies category, images and text. For a market outside the registry the API returns a
  placeholder (blank text, a stale `primary`/`open`), so the chain's question, rules, state and
  last price win.
- **Panta's list rows can't be trusted alone.** They carry no text and their state lags: rows
  listed `primary`/`open` are often `cancelled` or `resolved` on the market's own record. The
  adapter reconciles every open-looking row with `GET /markets/{id}/`, adopting its question,
  rules, sources and state.
- **Panta's cursor pagination works** (fixed venue-side 2026-09-28; before that `nextCursor` never
  advanced and the list was stuck on page 1). The refresh dedupes by market id and still stops on
  a non-advancing cursor, so it was safe before and after.
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

`GET /v1/venues` reports each venue's market count and whether its catalog is a last good copy (`stale`),
and `GET /ready` answers 503 until the first catalog is held, so an empty catalog and an unprimed one
are told apart.

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
  disables). A request that takes live quotes (`POST /v1/route`) or reads a market live costs 10. Over the limit is a `429` with
  `Retry-After`. Behind a proxy that overwrites `X-Forwarded-For`, set `TRUST_FORWARDED_FOR=true`; behind one
  that does not, every client shares the proxy's address and one bucket.
- **Redaction:** venue and RPC error text is scrubbed of URLs (`errors/adapter_error.rs`) before it
  reaches logs, `/venues/health` or a response, because a private RPC URL carries its key.
- **Slippage floor:** a buy may carry `min_shares_out`. The build is refused when the venue's
  build-time preview is below it, or gives no share count to compare. It is a build-time check,
  not an on-chain bound.
- **Timeout:** any request running longer than `REQUEST_TIMEOUT_SECS` (default 45) gets a `408`.
- **Venue health:** a venue's `429` is backpressure, not an outage — it never counts toward the
  failure streak that takes a venue out of routing.
- **Shutdown:** `SIGTERM`/Ctrl-C drains in-flight requests before exit.

## Deploying

```bash
docker build -t metis-api .          # from the workspace root (engine/)
sqlx migrate run --source aggregator/migrations   # deploy step, never at startup
docker run --env-file aggregator/.env -p 8080:8080 metis-api
```

- **Configuration** is environment only; the image bakes in no `.env`. `.env.example` lists every
  variable. `ADMIN_TOKEN` unset disables the operator routes, and `EXECUTION_ENABLED` /
  `MARKET_CREATION_ENABLED` are off unless exactly `true`.
- **Region:** Jupiter blocks US and South Korea IPs, so run somewhere else.
- **Postgres** is optional (unset means in-memory, nothing survives a restart). Use the direct
  connection, not a transaction-mode pooler. Size `DATABASE_POOL_SIZE` so instances × pool stays
  under the database's `max_connections`.
- **Panta discovery** needs a private `PANTA_RPC_URL`; the public RPC rate-limits
  `getProgramAccounts`.
- **Probes:** `GET /health` is liveness and is always 200 (it reports database state in the body but never fails
  on it: every read is served from memory, so a database outage should not restart the process and drop
  the catalog). `GET /ready` is readiness: 503 until a catalog is held. Route traffic on `/ready`. `SIGTERM`
  drains in-flight requests. Logs are JSON
  (`LOG_FORMAT=json`, the image default).
- One instance is assumed: the refresh lock and rate-limit buckets are in-process. Running several
  works, but each refreshes and limits on its own.

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
  maths/        pure numeric engines, no I/O (routing, orderbook)
engine/         the events engine: text, dictionary, extract, build, matching, rating, graph
repositories/   storage - market store (+ its Postgres snapshot), overrides, health, quote and positions caches
models/         data shapes only, no behavior; venue wire formats under models/venue/
errors/         AdapterError, RoutingError, and the HTTP-facing ApiError they map onto
```

`AdapterError::is_business_outcome()` is the seam that keeps "this market has no depth" from being
counted as a venue failure — without it, a run of thin markets drops a healthy venue out of routing.
