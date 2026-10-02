# Adding a Venue

The aggregator is built so a new venue is **one adapter + fixtures + config**. Nothing outside
`aggregator/src/services/<venue>/`, `services/registry.rs` and configuration may know a venue's
name. Target size: 2–4 days.

## 1. Capture fixtures

Add the venue to `aggregator/scripts/capture-fixtures.sh`: its category list (if it has one),
the first page of its catalog per category and unfiltered, one market, and one order book or
quote. Run it. Bodies are saved as sent, and `fixtures/<venue>/_index.tsv` records each request
and status.

Then write the venue's section of `docs/venue-fields.md`: every field, how often it's filled,
and which of our fields it feeds. That is where surprises show up. Panta's list rows turned out
to carry no text and a stale status, so its adapter reconciles each open row with the
single-market endpoint.

## 2. Write the adapter

Create `services/<venue>/` with `client.rs` (HTTP and wire parsing only) and `service.rs`
(`impl VenueAdapter`). Put the wire models in `models/venue/<venue>.rs`.

**Required:**
- `id()`, a short stable id matching the config id
- `capabilities()`
- `list_markets(ListParams)`, returning pages of `VenueMarket` and a cursor
- `get_market(id)`
- `quote(market, side, size)`

**Optional (defaults are sensible):**
- `filter_categories()`: the category names the venue's list filter accepts, **verbatim**. Read
  them from the venue if it publishes them; otherwise keep the list in the adapter. Default:
  none, and one unfiltered sweep fetches everything.
- `raw_path_prefixes()` and `raw_get()`: operator raw inspection, limited to read-only catalog
  paths.
- `build_buy`, `build_create_market`, `build_claim`, `status`, `confirm`: execution. The
  defaults return `Unsupported`.

**The adapter normalises at the boundary:**
- Prices are decimal strings in [0, 1]. Volumes are USDC. Times are UTC Unix seconds (use
  `models/venue/timestamp.rs`, which accepts integers, ISO 8601 and numeric strings).
- `venue_category` is the venue's category **exactly as sent**. The aggregator maps it to the
  shared name.
- Group markets under a `VenueEvent`. A venue without events gets one event per market.
- Fill what the venue gives: `tags`, `subcategory`, `league`, `start_time`, `entity` (team or
  player), `market_kind`, `outcome_labels`.
- Put every unmodelled field in `extra` with `#[serde(flatten)]`, so nothing is lost.
- Build URLs containing ids or paths with `utils::http::venue_url*`, never `format!`.
- Map HTTP failures to `AdapterError`. The refresh and health tracking rely on these
  classifications:
  - `is_auth_failure()`: 401/403, never retried
  - `is_rate_limited()`: 429, backpressure; it never marks the venue unhealthy
  - `is_client_rejection()`: other 4xx, e.g. a category the filter won't take
  - `is_business_outcome()`: no liquidity, unknown market; never counted as a failure

## 3. Register it

Add one arm to `build_adapter` in `services/registry.rs`. That is the only venue switch in the
service.

## 4. Configure it

```bash
ENABLED_VENUES=panta,jupiter,<id>
<ID>_API_KEY=...
<ID>_BASE_URL=...              # optional, the adapter has a default
<ID>_PAGE_CAP=10               # optional refresh settings, per venue
<ID>_MIN_REQUEST_GAP_MS=250
<ID>_MAX_CONCURRENT_CHAINS=1
<ID>_BREAKER_COOLDOWN_SECS=300
```

Adapter-specific options use the same prefix (Panta reads `PANTA_QUOTE_WALLET`).

If the venue's category names differ in wording from existing ones, add aliases with
`CATEGORY_ALIASES=from=to,...`. Categories without an alias are still served under their own
name.

## 5. Pass the contract

Add a row to `VENUES` in `src/venue_contract.rs`: the fixture directory, the API base path, and
a factory. Then run:

```bash
cargo test -p metis-aggregator venue_contract
```

This replays the fixtures over real HTTP and runs the adapter through the real refresh. It
checks that:
- the refresh completes cleanly
- pages parse to rows with ids and categories
- the venue lists its categories if it claims a filter
- ids are unique and stable
- prices are in [0, 1]
- open markets close before they resolve
- unmodelled fields are kept
- an unreachable venue is an error, not an empty catalog

`print_what_each_venue_produced` (run it with `--ignored --nocapture`) shows what the venue
produced per category, as a sanity check.

## 6. Check matching

Once matching v2 lands (Phase 4 of `aggregator-plan.md`), add labelled pairs for the new venue
to the evaluation set and confirm precision stays at or above 99%.
