# Aggregator Plan — Events-First MVP

**Date:** 2026-09-24
**Goal:** the best MVP-scale prediction-market aggregator that other products can build on.
Users start from **events** with rich metadata, open an event, and see every market from every
venue, grouped by question and sorted by our **rating**. A plain **markets-by-category** path
stays available for callers who want markets without events.

Companion to [audit.md](audit.md); finding IDs (A1…A19) refer to it.

**Decided (2026-09-24):**
- **Venues:** Jupiter and Panta for now. The system must accept a new venue as one adapter
  plus config, with no changes anywhere else (see *Venue contract*).
- **No AI in the MVP:** enrichment and matching are deterministic: rules, dictionaries and
  scoring. The engine has a hook where a model can be added later without redesign (see
  *Matching engine*).
- **Categories:** come from the venues and are stored as sent. There is no hand-written
  canonical category list (see Phase 2).
- **No human review queue:** an uncertain match stays unmerged. Admin overrides remain for
  corrections.

Sizes: **S** ≈ ≤1 day, **M** ≈ 2–4 days, **L** ≈ 1–2 weeks.

## Status (2026-09-24)

| Phase | State | Notes |
|---|---|---|
| 0 Ground truth | **Done** | Fixtures under `aggregator/fixtures/`, `docs/venue-fields.md`. Panta list rows have no text and a stale status, and there was **no bettable Panta market** at capture. |
| 1 Audit fixes | **Done** | A1, A2, A3, A5, A6, A8, A9, A10 (`/positions` removed), A11, A12, A13, A15, A16, A18. A7 fixed by rebuilding. A4 answered by `/v1` rating per outcome (legacy `/markets` sorts unchanged). A14 fixed by whole-venue replace. A17 (unused tables) left in place: dropping tables needs a go-ahead. |
| 2 Venue contract | **Done, one deviation** | Config registry, per-venue settings and circuit breaker, categories from venues + aliases, full payload persisted, contract test kit over fixtures, `docs/adding-a-venue.md`. **Deviation:** the adapter trait was not split into Source/Pricing/Execution. Its optional methods already default to `Unsupported`, and the registry plus contract kit gave the robustness the split was for, without the churn. |
| 3 Extraction | **Done** | `src/engine/` (named `engine`, not `core`, which would shadow Rust's `core` crate). |
| 4 Matching | **Done; test set pending** | Exact keys, blocking, hard constraints, score, one-per-venue sets, decisions, overrides. The 200-pair test set can't be built yet: the fixtures hold no live cross-venue overlap. Unit tests cover the matching rules. |
| 5 Persistence | **Done, differently** | The graph is rebuilt deterministically from persisted markets with hash ids, so graph tables weren't needed. `match_overrides` holds the one non-derivable input. **The new SQL has not run against a real Postgres yet.** |
| 6 Rating | **Catalog part done** | Price, activity and reliability per outcome, with breakdown. Live fillability/depth via `?quote_size=` on event detail is not done; `POST /v1/route` covers live ranking. |
| 7 API v1 | **Core done** | Events, event, question, markets, market, categories, venues, route; cursors, ETags, OpenAPI. **Not done:** API keys with per-key limits (per-IP limits exist), TS SDK (the Rust `sdk/` stub was deleted 2026-09-29), removing legacy routes. `GET /v1/markets/{venue}/{id}/live` added 2026-09-29. |

---

## Why event details are poor today (root cause)

1. **Events are built from the wrong field.** `services/matching/service.rs` creates an event
   with `title: market.title`. For Jupiter that is the *outcome label* ("Up", "Atlanta Dream",
   "160-179", "September 21"). The actual question is on `market.event.title`, which matching
   ignores. Hence events that are "numbers and single words".
2. **Unrelated markets merge.** Matching compares those labels with Levenshtein ≥ 0.72 and
   resolution time within 1h, so every "Up" market resolving in the same hour becomes one event.
3. **Venue metadata is dropped at deserialisation.** Jupiter events are parsed to
   `title/subtitle/image_url` only. Panta's `images`, `region`, `marketType` are parsed then
   discarded. Whatever else the venues send never reaches us.
4. **The model has no room for metadata.** `Event` is `title, category, times, one binary
   proposition`. No description, entities, tags, image, status, volume, or question structure.

Fixing this is not just "add fields": it needs a proper Event → Question → Listing model, an
extraction step, and matching on structured keys instead of titles.

---

## Target model

```
Event                      "Atlanta Dream vs New York Liberty — WNBA, 25 Sep"
 ├─ Question               "Who wins?"            (type: winner)
 │   ├─ Outcome            "Atlanta Dream"
 │   │   ├─ Listing        jupiter  · POLY-123 · yes  · price, volume, rules, rating
 │   │   └─ Listing        panta    · 9xF…     · yes  · …
 │   └─ Outcome            "New York Liberty"
 ├─ Question               "Total points over 160.5?" (type: total, line: 160.5)
 └─ …
```

**Event**: `id` (stable, derived from the canonical key), `slug`, `title`, `subtitle`,
`description`, `category` + `venue_categories[]` (as the venues send them), `subcategory`
(league / asset class), `tags[]`, `entities[]`
(`{name, kind: team|player|asset|person|org, role: home|away|subject}`), `image_url`,
`start_time` (kickoff / event start), `close_time` (earliest betting close), `resolution_time`,
`status` (`upcoming|live|closed|resolved`), `venue_count`, `question_count`,
`total_volume_usdc`, `headline` (the main question with its best price), `sources[]` (venue
event ids), `metadata_confidence`.

**Question**: `id`, `event_id`, `type` (`winner|binary|total|spread|threshold|multi`),
`label` (short), `text` (full, human sentence), `line`/`threshold` when relevant,
`outcomes[]`, `rules_summary`, `rules_differ` (true if listings resolve differently).

**Listing**: `venue_id`, `venue_market_id`, `venue_event_id`, `side` mapping, `price_yes/no`,
`volume`, `close_time`, `rules` (verbatim), `deeplink`, `rating`, `rating_breakdown`,
`last_synced_at`.

Nothing in these types names a venue. `venue_id` is a string taken from config.

---

## Venue contract (built into every phase)

**Rule:** nothing outside `services/<venue>/` and venue config may know a venue's name.
Refresh, extraction, matching, rating and API work only on normalised snapshots plus
capability flags. Adding a venue means one adapter, fixtures, and a config entry.

**1. Split the adapter trait by what a venue can do**

| Trait | Required? | Methods |
|---|---|---|
| `VenueSource` | yes | `id`, `capabilities`, `list_events(cursor, category?) → Page<VenueEventSnapshot>`, `list_categories() → Vec<VenueCategory>` (default: derived from snapshots), `get_market` |
| `VenuePricing` | optional | `quote(market, side, size)`, `orderbook(market)` |
| `VenueExecution` | optional | `build_buy`, `build_claim`, `status`, `confirm`; the current builders move here |

Today's single `VenueAdapter` (`services/mod.rs`) mixes all three and makes every venue stub the
methods it can't support. A registry holds `Arc<dyn VenueSource>` and looks up the optional
traits per venue.

**2. The adapter normalises everything at the boundary**
- Prices are decimals in [0, 1], sizes and volume are in USDC, times are UTC unix seconds.
- Ids are namespaced as `venue_id:venue_id_for_it`.
- Venues without an event layer emit one snapshot per market.
- Anything the venue sends that we don't model yet goes in `extra` (`serde(flatten)`), so it is
  never lost.

**3. Capabilities replace venue checks.** Downstream code branches on flags, never on a
venue id. Flags: `has_events`, `category_filter`, `pagination: cursor|offset|none`,
`pricing: orderbook|amm|none`, `fees`, `execution`, `max_page_size`, `rate_limit`.

**4. Venues are registered in config.** A `venues:` list gives each venue `id`, `kind`
(adapter), `base_url`, `api_key_env`, `enabled`, `rate_limit`, `page_cap`, `timeout`. A
factory builds adapters from `kind`, so enabling or disabling a venue is a config change.
This replaces the hard-coded pushes in `app.rs:34-45`.

**5. Each venue is isolated from the others**
- Each venue has its own rate limiter, request timeout, circuit breaker (after N consecutive
  failures, back off and mark the venue unhealthy) and bucket staleness.
- A slow or broken venue never delays another venue's refresh or any read.

**6. One shared test kit.** Every adapter must pass the same generic suite (`tests/venue_contract.rs`),
run against its recorded fixtures. It checks that:
- pagination terminates
- ids are stable across two fetches
- prices are in [0, 1]
- times parse and `close ≤ resolution`
- categories are non-empty
- unknown fields land in `extra`
- a 429 or 5xx is surfaced as the right error kind

A new venue is "done" when this suite passes.

**7. Venue-specific knowledge is data, not code.** Category aliases, entity aliases and
title-parsing patterns are keyed by `venue_id` in Postgres or config files. A new venue adds
rows, not `match` arms.

**8. An onboarding checklist** (`docs/adding-a-venue.md`): capture fixtures, write the
adapter, pass the test kit, add a config entry, seed its category aliases and parsing
patterns, then check its match precision against the eval set. Target size: **M** per venue.

---

## Matching engine (deterministic, no AI)

One module (`aggregator/src/engine/`) turns snapshots into the event graph. Every step is
deterministic, explainable and incremental: only new or changed snapshots are processed.

```
snapshot ─► 1. normalise ─► 2. extract ─► 3. canonical key ─► 4. exact join
                                                   │               │ unmatched
                                                   ▼               ▼
                              graph ◄─ 7. overrides ◄─ 6. decide ◄─ 5. candidate scoring
```

**1. Normalise text.** Unicode fold, lowercase, strip punctuation, and expand common
abbreviations ("vs."/"@"/"v" → `vs`). Canonicalise numbers ("$100k" → `100000`,
"100,000" → `100000`), dates ("Sep 25", "25/09" → `2026-09-25`) and percentages. Drop
stopwords for similarity only; titles stay as sent.

**2. Extract with per-category extractors.** These are pluggable (`trait Extractor`) and
chosen by category, with a generic fallback:

| Extractor | Pulls out | Question type |
|---|---|---|
| Sports | teams/players (via alias dictionary), league, kickoff date, market kind (winner/total/spread) and line | winner / total / spread |
| Crypto / finance | asset (alias dictionary), direction (above/below/up/down), threshold, target date/time, price source | threshold / binary |
| Range buckets | numeric ranges from outcome labels ("160-179", "<150") | multi |
| Date buckets | "by Sep 21", "September 21" outcome labels | multi |
| Generic | named entities by capitalisation + dictionary hits, all numbers, all dates, key-term set | binary |

The output is a structured `Extraction { entities, numbers, dates, kind, polarity, terms,
confidence }`. Parsing patterns are data (point 7 of *Venue contract*), so tuning one needs
no code change.

**3. Build a canonical key.** Built only when an extraction is complete and
high-confidence:
- `sports:{league}:{team_a}|{team_b}:{date}` (teams sorted, aliases resolved)
- `crypto:{asset}:{direction}:{threshold}:{date}`
- Questions: `{event_key}:{type}:{line}`

**4. Exact join.** Hash map from key to events, so the join is O(n). Equal keys merge
automatically. This covers most sports and crypto events.

**5. Score the candidates** for anything that missed the exact join:
- **Blocking:** only compare within `(category, date bucket ±1 day)`, and use MinHash-LSH on
  the term set to pick candidates, so the cost stays near-linear rather than all-pairs.
- **Hard constraints** (any failure means *different*, with no score):
  - thresholds or lines not equal
  - dates outside tolerance
  - different asset or entity sets
  - opposite polarity. "Above X" vs "below X" is flagged as the same question with the sides
    inverted, not merged as-is.
- **Weighted score:**
  - entity overlap: 0.35
  - TF-IDF weighted term similarity: 0.25
  - date proximity: 0.15
  - number agreement: 0.15
  - outcome-label overlap: 0.10

  The weights live in config.

**6. Decide.** A pair merges automatically only when the score is at least 0.90 and every
hard constraint passes. **Everything else stays separate.** The decision is stored in
`match_decisions` with its feature vector, score and failed constraints. Every merge and
non-merge is explainable from that row.

**7. Overrides:**
- `POST /admin/events/merge` and `/split` write to `match_overrides`, which every rebuild
  honours.
- `GET /admin/matches?score=0.7..0.9` lists near-misses with their feature breakdown, for
  tuning weights and dictionaries.

**Dictionaries grow automatically.** When two venues' events join on a strong key, their
differing raw names ("NY Liberty" vs "New York Liberty") are recorded as alias candidates.
One confirmed alias fixes that spelling everywhere.

**Evaluation set:**
- 200+ hand-labelled cross-venue pairs from the fixtures.
- CI reports precision and recall per category and fails when precision drops below 99%.
- Every change to weights, patterns or dictionaries runs against it.

**Where AI can be added later (not in the MVP):**
- `trait Enricher` and `trait Adjudicator` sit at steps 2 and 6, with the rule-based
  implementations as defaults.
- A model-backed version can be added later, either to fill fields the rules leave empty or
  to decide pairs in the 0.7–0.9 band. That needs no change to the graph, the storage or the
  API.

---

## Phase 0 — Ground truth on venue data (S)

Before designing extraction, know exactly what each venue gives us.

- Pull full raw payloads (events, markets, orderbooks, categories) for every venue and category
  via the raw proxy (after it is gated — Phase 1), save as fixtures under
  `aggregator/fixtures/<venue>/`.
- Write a field inventory per venue: every field, fill rate (the raw route already computes
  coverage), and which canonical field it feeds. Check whether each venue has a categories
  endpoint.
- Hand-label the first 200 cross-venue pairs for the eval set.
- Output: `docs/venue-fields.md`, fixtures, `fixtures/eval/pairs.jsonl`.

**Exit:** we know what metadata is free from venues, and we have a baseline to measure against.

---

## Phase 1 — Bug fixes (Batch 1)

Must land first; everything else sits on this.

| # | Fix | Size | Acceptance test |
|---|---|---|---|
| A1 | `collect_pages` returns `Result`/`complete`; only replace a bucket on a complete chain; keep last good bucket + mark stale; per-bucket `refreshed_at` | M | Mock adapter failing on page 1 and page 4 → bucket and DB rows unchanged |
| A2 | Clone the shortlist and drop the store read lock before any `.await` in `/markets` | S | Test holding a slow quote while a refresh writes; other reads don't block |
| A3 | Raw proxy behind admin token; reject `..`/encoded dots; allow-list path prefixes; pass upstream status | S | Route test: anonymous → 401; `%2e%2e` → 400 |
| A9 | Build venue URLs with `path_segments_mut().push()`; validate ids (base58 / market-id pattern) | S | Unit test: `../x` id is escaped / rejected |
| A6 | With `quote_size`, clamp `limit` to `MAX_LIVE_QUOTES` and report it | S | Page 2 starts where page 1 ended |
| A12 | `MissedTickBehavior::Delay` on the refresh timer | S | — |
| A5 | Per-IP (later per-API-key) rate limit; short-TTL quote cache; 429 → backoff, not a health failure | M | 429 from venue doesn't flip `routable` |
| A8 | Separate `status()` from `confirm()`; Panta status → `Unsupported` until it has a read endpoint | S | Polling never calls register |
| A10 | Remove `/positions` for now (or require wallet signature); drop the unused in-memory index | S | — |
| A11 | Rank on all-in cost (price + fee per contract); populate fees where venues expose them | S | Scorer test with fees flips order |
| A13 | Unknown strategy/sort → 400 everywhere | S | — |
| A15/16 | Graceful shutdown, request timeout layer, metrics: request latency/status, refresh duration, per-venue errors, bucket age | M | — |
| A18 | README drift | S | — |

A7 (graph grows forever) and A4 (cross-market price sorting) are fixed structurally by Phases
3–5 rather than patched.

**Exit:** refresh can't lose data, reads can't stall, public surface can't be abused, tests
cover each fix.

---

## Phase 2 — Venue contract and ingestion (M–L)

Implements *Venue contract*: the rule that no code outside a venue's folder or config knows
its name.

- Split `VenueAdapter` into `VenueSource` / `VenuePricing` / `VenueExecution`. Add the config
  registry and factory, and isolate each venue (rate limiter, timeout, circuit breaker).
  Port Jupiter and Panta. Pascal was later removed entirely (2026-09-29): it needs a deposit and signed off-chain permits, which the unsigned-tx-only MVP rules out.
- Introduce `VenueEventSnapshot { venue, venue_event_id, title, subtitle, description, images,
  tags, venue_category, venue_subcategory, extra, markets: Vec<VenueMarket> }` as the unit of
  ingestion.
- Extend the wire models to keep all the metadata found in Phase 0, plus `extra` for the rest.
- `VenueMarket.display_title`: `"{event.title} — {market.title}"` when the market title is
  only an outcome label. This alone fixes the bare labels on `/markets`.
- Write the contract test kit and make both adapters pass it.

**Categories from the venues.** Today `Category` is a hand-written enum with a translation
table per venue (`models/venue/{jupiter,panta}.rs::category_to_native`). It drifts, and it has
already caused bugs (the Panta `other` collision).
- **Discover:** call `list_categories()`. It uses the venue's endpoint where there is one,
  otherwise the distinct values its snapshots report on every refresh. Keep subcategory,
  series or league too, where the venue sends them.
- **Store as sent:** a `venue_categories` table holding `venue_id`, `native` (exact string),
  `label`, `parent`, `first_seen`, `last_seen` and `market_count`. Every listing keeps its
  `venue_category` verbatim.
- **Refresh:** iterate over the discovered native categories instead of `Category::all()`.
  The server-side filter always uses the venue's own string, so nothing is skipped or rejected.
- **Cross-venue view:** a `category` is the normalised native name (lowercase, trimmed), so
  `sports` on both venues is one category. Equivalences that differ by name are kept in a
  small, data-driven alias table (for example, Jupiter `climate & science` ↔ Panta
  `science`), editable through an admin endpoint. An unaliased category is still served under
  its own name and is never dropped. A new venue's categories appear automatically.
- `GET /v1/categories` returns each category with its per-venue native names and counts.
- Remove the `Category` enum and both `category_to_native` tables.

**Exit:** no venue metadata is lost, every category shows up without a code change, and both
adapters pass the test kit.

---

## Phase 3 — Metadata extraction, no AI (M–L)

Turn snapshots into canonical Events/Questions with readable metadata using the *Matching
engine*'s steps 1–2.

- **Extractors:** sports, crypto/finance, range buckets, date buckets, generic.
- **Dictionaries:**
  - Teams and leagues for the sports leagues the venues list, seeded from fixtures and
    public league rosters.
  - Assets (tickers, names).
  - All stored in Postgres (`entities`, `entity_aliases`).
- **Question text from templates:** for example, "Will BTC be above $100,000 on 25 Sep?" or
  "Who wins Atlanta Dream vs New York Liberty?". There is always a full sentence, never a bare
  label.
- **Description:** the venue's description when present. Otherwise the first sentence of the
  rules, or a template built from the extraction.
- **Presentation:** canonical title, slug, and image in priority order (venue event image,
  then entity logo, then category default). Tags come from venue tags plus entities. Status
  is computed from times.
- `metadata_confidence` is based on extraction completeness. A low-confidence event is served
  with venue-native fields as they are.

**Exit:** every event has a readable title, description, image, status and questions written
as full sentences. Entities are filled wherever a dictionary or extractor covers them.

---

## Phase 4 — Matching v2, no AI (L)

The *Matching engine*'s steps 3–7: canonical keys, exact join, LSH-blocked candidate scoring,
hard constraints, threshold decisions, `match_decisions`, overrides, alias learning, and
the eval set in CI.

- Question matching inside an event uses the same `type`, the same `line`/`threshold` and
  the same outcome entities. If the rules differ (overtime, price source, cutoff), set
  `rules_differ` and never rank those listings as interchangeable.
- **Scope:** every category that more than one venue offers (currently sports, crypto,
  politics, finance and science). Other categories are served as single-venue events, fully
  extracted.
- **Performance target:** a full rebuild of the current catalog (~10k markets) in under 1 s;
  an incremental update proportional to what changed.

**Exit:** Jupiter↔Panta events merge at 99% precision or better on the eval set, and every
decision is explainable.

---

## Phase 5 — Persistence and stable IDs (M)

- New migration: `events`, `questions`, `outcomes`, `listings`, `entities`, `entity_aliases`,
  `venue_categories`, `category_aliases`, `match_decisions`, `match_overrides`. Replace the
  unused initial tables (A17).
- Event/question IDs derived from canonical keys (hashed, prefixed: `evt_…`, `q_…`) so they're
  stable across restarts and refreshes. When a merge changes an event's key, keep an alias row
  so old IDs still resolve (redirect).
- Graph is **rebuilt** each refresh from the market store + dictionaries + overrides (fixes
  A7), swapped in atomically (`ArcSwap`), then persisted. Reads never wait on the rebuild.

**Exit:** integrators can store event IDs; restarts serve the full graph immediately.

---

## Phase 6 — The rating (M)

Rating only compares listings **of the same question outcome** — never across unrelated
markets (fixes A4).

Per listing, normalised within its outcome group to 0–100:

| Component | Signal | Default weight |
|---|---|---|
| Price | all-in cost per contract at the reference size ($100), fees included | 40% |
| Fillability | share of reference size that fills | 20% |
| Depth / spread | book depth near top, or AMM depth; bid-ask spread | 15% |
| Activity | 24h / total volume | 10% |
| Venue reliability | health history, settlement track record | 10% |
| Freshness | age of price data | 5% |

- Catalog rating from refreshed data; `?quote_size=` recomputes price/fillability live
  (cached, capped).
- A component a venue can't provide (per its capability flags, e.g. no orderbook) is scored
  from what it does provide, not zeroed, so a new venue isn't penalised for its API's shape.
- Listings with `rules_differ` are shown separately with the difference called out.
- Event-level sort (`sort=best` on `/events`) uses the headline question's best rating,
  volume, and time to close.
- Weights in config, returned in `rating_breakdown` for transparency.

**Exit:** the event page's default order is explainable and consistent.

---

## Phase 7 — API v1 for integrators (M)

All under `/v1`; old routes kept for one release then removed.

| Endpoint | Purpose |
|---|---|
| `GET /v1/events?category=&status=&within_days=&q=&sort=best\|closing_soonest\|volume&cursor=&limit=` | Event cards: rich metadata + headline question, best price, venue count, volume |
| `GET /v1/events/{id}` | Full event: metadata, questions → outcomes → listings sorted by rating; `?quote_size=` for live prices |
| `GET /v1/events/{id}/questions/{qid}` | One question, all listings, rating breakdown |
| `GET /v1/markets?category=&venue=&within_days=&sort=&cursor=&limit=` | **Markets-only path** — every listing flat, filtered by category, each carrying `event_id`, `event_title`, `question_text` so it's never a bare label |
| `GET /v1/markets/{venue}/{venue_market_id}` | One market with its event context |
| `GET /v1/categories` | Categories with per-venue native names and event/market counts |
| `GET /v1/venues` | Enabled venues, their capabilities and health |
| `POST /v1/route` | Best listing for a `question_id` + outcome + size (replaces `/find-best-market`) |
| `POST /v1/execution/*` | Unchanged builders, behind flags; available only for venues with `VenueExecution` |

Integrator basics: API keys with per-key rate limits, cursor pagination everywhere, `ETag` /
`Cache-Control` on list endpoints, consistent error shape, OpenAPI published, a thin TS SDK
generated from it. `venue` values are open strings from `/v1/venues`, never an enum in the
OpenAPI spec, so a new venue needs no SDK release.

**Exit:** a product can build an events UI and a markets UI from the public API alone.

---

## Out of scope

- Integrating more venues (Polymarket, Kalshi direct) and, post-MVP, revisiting Pascal (its adapter was deleted 2026-09-29; recover it from history). The *Venue
  contract* keeps each of these a contained **M**-sized job when wanted.
- AI enrichment and adjudication. The hook is in place (`Enricher` / `Adjudicator` traits).

---

## Sequencing

| Milestone | Phases | Result |
|---|---|---|
| **M1 — Stable base** | 0, 1 | Audit fixes landed, fixtures and eval set captured |
| **M2 — Venue contract + rich single-venue events** | 2, 3, 5 (partial), 7 (`/v1/events`, `/v1/markets`, `/v1/categories`) | Pluggable venues; readable events on each venue's own grouping; markets carry event context |
| **M3 — Cross-venue + rating** | 4, 5 (full), 6 | Jupiter↔Panta events merged in every shared category; rated listings |
| **M4 — Integrator-ready** | 7 (keys, SDK, caching) | External products can integrate |

M2 is the key shortcut: most events are single-venue today, so the events-first experience
does not have to wait for cross-venue matching.

---

## Decisions needed

1. **Rating weights:** accept the defaults above for the MVP?
2. **Match threshold:** start auto-merging at a score of 0.90 or above with all hard
   constraints passing, then tune it from the eval set?
