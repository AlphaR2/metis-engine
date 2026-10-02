# Venue Field Inventory

**Captured:** 2026-09-24 with `aggregator/scripts/capture-fixtures.sh`. The raw bodies are in
`aggregator/fixtures/<venue>/`, and each file's HTTP status is in `_index.tsv`.
**Scope:** Panta (8 category pages + 84 market details) and Jupiter (11 category pages, 50
events each, most-traded first: 519 events, 6,090 markets).

This is Phase 0 of [aggregator-plan.md](aggregator-plan.md). It records what each venue gives
us for free, and what the extraction step has to produce.

---

## Panta

**Endpoints**
- `GET /categories/` returns `{"categories": [...]}`: `sports, crypto, politics, entertainment,
  finance, science, world, other`.
- `GET /markets/?category=&limit=` returns `{items, nextCursor}`. The cursor did not advance when this was captured (2026-09-24); Panta fixed it venue-side on 2026-09-28 and pages are now disjoint. The list is still a registry, not a chain scan.
- `GET /markets/{id}/` returns about 90 fields, including the question and rules.

**List rows carry no text.** Across all 84 markets, `title` is blank on every row and
`description` is filled on only 9. The question (`title` / `question`), `resolutionRule`,
`sources` and `oracle` are only on the single-market endpoint, and even there only 38 of 84 are
filled. **Ingestion must fetch details** for every market that stays in the store.

**The categories endpoint is not the full list.** Markets report 11 category values:
- **Also in the endpoint:** sports, crypto, politics, finance, world.
- **Not in the endpoint:** `stocks`, `commodities`, `pop-culture`, `gaming`, `weather`,
  `business`.

Category discovery must combine the endpoint with the values that markets report.

**Categories are unreliable for matching.** Examples filed under `sports`:
- "Will Bitcoin (BTC) be priced at $81,000.00 or higher…"
- "Will U.S. Initial Jobless Claims be reported at or above 206,000…"
- "Rain to fall in Lisbon in next 10 minutes"
- "Marco Rubio to win the 2028 Presidential nomination…"

Keep the venue's category for display and filtering. Group match candidates by extracted domain
and date, not by venue category.

| Field (list row) | Filled | Feeds |
|---|---|---|
| `marketId` | 84/84 | `venue_market_id` |
| `category` | 84/84 | `venue_category` (as sent) |
| `title` | **0/84** | detail `title`/`question` → question text |
| `description` | 9/84 | description |
| `images` | 27/84 | image |
| `phase` (`primary`/`secondary`/`resolved`/`cancelled`) | 84/84 | status |
| `marketType` (`standard`/`breaking`) | 84/84 | tags |
| `startTime` / `endTime` / `resolutionTime` | 84/84 | start / close / resolution |
| `region` | 84/84 | tags |
| `yesPrice`/`noPrice` | 64/84 | price (primary phase uses `primaryYes/NoPrice`) |
| `volumeUsdc`, `totalVolumeUsdc` | 84/84, 70/84 | volume |

| Field (detail only) | Filled | Feeds |
|---|---|---|
| `title` / `question` | 38/84 | question text |
| `resolutionRule` | 38/84 | rules |
| `sources`, `oracle` | — | rules / price source |
| `totalTrades`, `totalYes/NoShares`, AMM reserves | — | depth for the rating |

**Live catalog at capture time:** 2 markets were bettable. The rest were resolved, cancelled or
in the secondary phase, which Panta can't trade through its API.

---

## Jupiter

**Endpoints**
- `GET /events?includeMarkets=true&start=&end=&sortBy=&category=` returns
  `{data, pagination: {start, end, hasNext}}`.
- `GET /markets/{id}` and `GET /orderbook/{id}`.
- There is **no categories endpoint** (`/categories` returns 404). Categories are discovered from
  events. The filter accepts `crypto, sports, politics, esports, culture, economics, tech,
  finance, climate & science, weather, mentions`.

Every captured event came from `provider: polymarket` (the capture takes the most-traded events
first). Kalshi markets exist but did not make the top 50 of any category.

| Event field | Filled | Feeds |
|---|---|---|
| `eventId` | 519/519 | `venue_event_id` |
| `category` | 519/519 | `venue_category` |
| `subcategory` | 227/519 | subcategory / league (e.g. `mlb`, `cs2`, `btc`, `nfl`) |
| `tags[]` | 519/519 | tags (e.g. `elections`, `middle-east`) |
| `metadata.title` | 519/519 | event title (the question) |
| `metadata.slug` | 519/519 | slug |
| `metadata.imageUrl` | 519/519 | image |
| `metadata.closeTime` | 515/519 | close time |
| `metadata.league` | 52/519 | league |
| `metadata.series` | 519/519 | source series |
| `metadata.subtitle` | **0/519** | — |
| `beginAt` | 342/519 | start time (kickoff) |
| `closeCondition` | 519/519 | rules summary |
| `volumeUsd`, `volume24hr` | 519/519 | volume, activity |
| `isLive`, `isActive` | 519/519 | status |

| Market field | Filled | Feeds |
|---|---|---|
| `marketId`, `provider` | 6090/6090 | listing id, source |
| `title` | 6090/6090 | **outcome label** ("Marco Rubio", "Up", "160-179") |
| `status`, `result` | all, 1162 | status |
| `openTime`, `closeTime`, `resolveAt` | all, all, 1162 | times |
| `rulesPrimary` | 6090/6090 | rules |
| `pricing.buyYes/buyNo/sellYes/sellNoPriceUsd`, `volume` | all | price, **spread**, volume |
| `outcomes`, `marketOptions` | all | outcome labels |
| `isTeamMarket`, `team{name,imageUrl}` | all, 104 | entities |
| `sportsMarketType` (`moneyline`, …) | 105 | question type |
| `imageUrl` | 1007 | image |
| `sportsLine`, `periodType`, `periodNumber`, `gameNumber` | 0 | — |

---

## Consequences for the plan

1. **Phase 2:** the Panta adapter fetches details for markets that stay in the store (only a
   handful are live, so this is cheap). Category discovery combines each venue's endpoint, if
   any, with the values its snapshots report.
2. **Phase 3:** Jupiter supplies title, slug, image, tags, league and teams, so extraction mostly
   builds question text and entities. Panta supplies only a question sentence (when it has one),
   so its extraction has to parse everything from that sentence.
3. **Phase 4:** a Panta binary market usually corresponds to one **outcome** of a Jupiter
   multi-outcome event ("Marco Rubio to win the 2028 Republican nomination" ↔ event "Republican
   Presidential Nominee 2028", outcome "Marco Rubio"). Matching therefore works at the question
   and outcome level, not only event to event.
4. **Phase 4 test set:** real cross-venue overlap is small today. Most Panta markets are short
   props (goals in a match, rain in 10 minutes). The first test set is built from what does
   overlap (crypto and stock price thresholds, politics outcomes) plus synthetic variants. It
   grows as Panta's catalog does.
5. **Rating:** Jupiter bid/ask gives a real spread. Panta is an AMM with reserves, so depth comes
   from the quote endpoint.
