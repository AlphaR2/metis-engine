# metis-aggregator

The off-chain aggregation API (`metis-api`). Discovers prediction markets across a curated venue
network, serves them by category, and ranks them by price, liquidity and execution quality.

It discovers, prices and recommends, and can **build** (never sign or broadcast) buy, claim and
market-creation transactions for the caller's wallet. Set `MODE=read` to switch building off and serve a read-only catalog.


### Creating a Metis market

`POST /execution/markets` with `venue_id: "metis"` builds two unsigned transactions, one per call,
because a market's text does not fit beside the market in one packet:

1. `venue_params.step` omitted or `"create"`: `initialize_market`. Needs `question`,
   `resolution_rule` (the YES rubric), `category`, `wallet` (the creator, who pays the seed), and in
   `venue_params`: `oracle` (an existing, scheduled football oracle; also accepted as a
   `sources_of_truth` entry `oracle:<address>`), `condition` (`home_win`, `away_win`, `draw`,
   `both_teams_score`, `total_goals_over:<n>`), `rubric_no`, `label_yes`, `label_no`, optional `league`
   and `nonce`. The kickoff comes from the oracle; `start_time`, `end_time` and `resolution_time` are
   not used. The response's `venue_ref` is `create:<market>`.
2. After the first lands: the same body with `venue_params.step = "metadata"` and
   `venue_params.market = <market>` writes the text (creator only, once).

The text is checked against the program's limits before anyone is asked to sign.

### Prices and the order books

After betting closes a market trades on its Manifest books. Its catalog price is the break-even
probability of the cheapest ask; when nobody is selling a side, that side's price is `null` rather
than a made-up number. A maker's deposited slices and resting asks count in `/v1/positions`
(`open_orders`), and a claim withdraws whatever the maker has parked on the books in the same
transaction (slices still offered in resting orders must be cancelled first).

## The `/v1` API: events

An **event** is our own aggregation of markets about one real-world subject: a match, one season of a
show, a launch, a token sale, an asset on a day, an election. It carries our metadata (title, image,
category, tags, dates, volume, venues, a 0-100 rating, and *why* its markets were grouped). The
**markets** under it are the venues' own markets, **exactly as sent**, each annotated (never rewritten)
with our rating and the identical question on another venue. A market that matches nothing is its own
event. Filtering, sorting and rating happen at event level; inside an event the markets re-sort.

| Endpoint | What it returns |
|---|---|
| `GET /v1/events` | Event cards, each with a preview of its three best markets. Filters: `category`, `domain`, `status` (`open` by default), `within_days`, `q`, `venue`, `min_venues`. Sort: `best` (our event rating), `closing_soonest`, `volume`. |
| `GET /v1/events/{id}` | The event and all its raw markets. `?sort=rating|volume|closing|price`, `?venue=` |
| `GET /v1/markets` | Every venue market, flat, each carrying its `event_id` |
| `GET /v1/markets/{venue}/{id}` | One market with its event, from the last refresh |
| `GET /v1/markets/{venue}/{id}/live` | The same market read from the venue right now |
| `GET /v1/categories` | Categories as the venues report them, with counts |
| `GET /v1/venues` | Enabled venues, capabilities, health and staleness |

Lists use opaque `cursor`s, and responses carry an `ETag` and `Cache-Control: max-age=30`.

**How events are built** (`src/engine/`):

1. **Profile** each market from what the venue sent: title, its venue event's title and tags, outcome
   labels, dates, entities from a seeded dictionary (assets, countries, teams the venues attach), a season
   number and year.
2. **Cluster** with rules, recording the reason: `venue_event` (the venue grouped them), `match` (the same
   two sides on the same day), `price` (one asset on one day), then `terms` (the same rare words, e.g.
   "Tram to finish Top 3 of BBNaija Season 11" and "A female housemate to win Big Brother Naija Season 11").
   Same-venue markets cluster exactly like cross-venue ones. A different season, year, kind of subject or
   a close date more than 90 days apart is a hard conflict. Events are capped at 60 markets (300 for a
   venue's own grouping).
3. **Assist** (optional): markets the rules left alone that resemble something are put to Claude in batches
   (`ANTHROPIC_API_KEY`; `claude-haiku-4-5`), once each, cached in memory and Postgres
   (`market_subjects`). Its answers are validated against the same hard conflicts. No key, or a failed
   call, and the rules' graph stands.
4. **Assemble**: title (a match is "A vs B", a price event "Asset · date", a model-grouped event its
   headline, else shared words or the venue's own title as a headline), the best member image, category,
   tags, dates, volume, rating. Ids are hashes of the cluster key, stable while its markets are.

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
Jupiter starts using another program, add it to `ISSUER_PROGRAM`'s allowlist in `services/jupiter/service.rs` rather than
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
| `EXECUTION_DISABLED`, `MARKET_CREATION_DISABLED` | `MODE=read` on this deployment | Show trading as unavailable |
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

## CORS

`CORS_ORIGIN` is optional. Unset allows any origin (the API holds no cookies or credentials). Set it to one
origin or a comma-separated list, each exactly `scheme://host[:port]`; a wildcard, path or trailing slash is
refused at startup. Methods are `GET`, `POST`, `OPTIONS`; `Retry-After` and `ETag` are exposed. Per-IP rate
limiting exists in the code but is switched off for the MVP.

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

The earlier `/markets`, `/categories`, `/events`, `/events/{id}/markets`, `/find-best-market` and
`POST /v1/route` routes were removed: `/v1` is the only discovery API, and an event page compares its
markets directly.

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

### The Metis venue

`metis` is this project's own market program, and unlike Panta and Jupiter it has **no HTTP API**:
its accounts are the catalog. `services/metis/` reads them over JSON-RPC (`getProgramAccounts` per
account type, filtered by size and discriminator), decoding with the program's own types (`metis-core`
is linked as a library, so the adapter reads exactly the layouts the program writes and prices bets
with exactly the code it runs).

- **Honest prices.** The program mints shares off a curve with virtual liquidity but pays winners
  from the real pot, so the curve's own probability is not the odds. Every price and size the venue
  reports is the **payout-based** one: a quote's `effective_shares_or_size` is what the bet pays if
  it wins and `avg_price` is the break-even probability, so size times price is the amount staked
  and means the same thing as on venues whose every share pays $1. The catalog's `yes_price` /
  `no_price` are quoted for a stated bet size (`METIS_REFERENCE_USDC`, in `extra.price_basis_usdc`).
- **Two phases.** Until betting closes, shares are bought from the program (`place_bet`). After
  that the pot is final and holders trade on Manifest order books, one per outcome, created by the
  program's `close_primary` at addresses derived from the market. The adapter prices buys and sells
  by walking the real books in exact integer arithmetic (rounding against the trader), and builds a
  single Manifest swap for either. A book holding *global* orders isn't routed.
- **Every transaction is checked against the program, not just against our expectations.** The
  adapter's tests execute what it builds against the compiled core and Manifest programs in litesvm:
  a buy delivers exactly the quoted shares, a claim pays exactly the amount shown, a sale returns
  what the book said, and the slippage floor is enforced on chain. They need
  `target/deploy/metis_core.so` and `manifest.so` (`just build-core fetch-manifest`) and skip loudly
  without them.
- **Limits worth knowing.** Shares a maker has deposited into a Manifest seat aren't counted in
  positions (they sit in Manifest's vault until withdrawn). A wallet's cost isn't on chain, so
  `avg_price_usdc` and pnl are `None`. `resolution_time` is an estimate (kickoff + 4 h): the oracle
  decides when a match is over. The venue contract tests replay HTTP fixtures, which don't apply
  here; the adapter's own tests run the same checks (unique ids, prices in [0, 1], close before
  resolve) against a mock RPC node serving real account bytes.

Configuration: `METIS_RPC_URL`, `METIS_PROGRAM_ID`, `METIS_CONFIG`, `METIS_REFERENCE_USDC` (see
`.env.example`). Build with `--features devnet` or `mainnet` so the default Config address matches
the deployed program.

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

`POST /refresh`, `GET /metrics` and `/admin/matches*` are open in the MVP. A refresh is a multi-minute fan-out
but cannot overlap itself, so repeated calls are harmless. Put them behind your own proxy if the service is
exposed publicly.

## Deploying

```bash
docker build -t metis-api .          # from the workspace root (engine/)
sqlx migrate run --source aggregator/migrations   # deploy step, never at startup
docker run --env-file aggregator/.env -p 8080:8080 metis-api
```

- **Configuration** is environment only; the image bakes in no `.env`. `.env.example` lists every
  variable (a handful: venue keys, `MODE`, `PORT`, optional `CORS_ORIGIN`, `DATABASE_URL`, `PANTA_RPC_URL`).
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
