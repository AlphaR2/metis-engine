-- Initial persistence schema for the canonical event graph, venue health, and positions.
-- Mirrors the in-memory shapes in models::event / models::venue / models::position - this is a
-- persistence layer under the existing models, not a redesign of them.

CREATE TABLE events (
    event_id            TEXT PRIMARY KEY,
    title               TEXT NOT NULL,
    category            TEXT NOT NULL,
    start_time          BIGINT NOT NULL,
    resolution_time     BIGINT NOT NULL,
    needs_confirmation  BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX events_start_time_idx ON events (start_time);
CREATE INDEX events_category_idx ON events (category);

-- One row per outcome of an event's (currently always singular) proposition.
CREATE TABLE outcomes (
    outcome_id      TEXT PRIMARY KEY,
    event_id        TEXT NOT NULL REFERENCES events (event_id) ON DELETE CASCADE,
    proposition_id  TEXT NOT NULL,
    label           TEXT NOT NULL
);

CREATE INDEX outcomes_event_id_idx ON outcomes (event_id);

-- One row per venue's copy of an outcome - the venue<->event join, plus enough venue-native
-- state (phase, dead, prices) to reconstruct tradeability without re-fetching from the venue.
CREATE TABLE venue_market_refs (
    id                TEXT PRIMARY KEY,
    outcome_id        TEXT NOT NULL REFERENCES outcomes (outcome_id) ON DELETE CASCADE,
    venue_id          TEXT NOT NULL,
    venue_market_id   TEXT NOT NULL,
    venue_side        TEXT NOT NULL,
    phase             TEXT NOT NULL,
    dead              BOOLEAN NOT NULL DEFAULT FALSE,
    yes_price         TEXT,
    no_price          TEXT,
    last_synced_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (venue_id, venue_market_id, outcome_id)
);

CREATE INDEX venue_market_refs_outcome_id_idx ON venue_market_refs (outcome_id);
CREATE INDEX venue_market_refs_venue_id_idx ON venue_market_refs (venue_id);

-- Per-venue reachability state, replacing the in-memory HealthRegistry.
CREATE TABLE venue_health (
    venue_id                TEXT PRIMARY KEY,
    routable                BOOLEAN NOT NULL DEFAULT TRUE,
    consecutive_failures     INTEGER NOT NULL DEFAULT 0,
    last_success_at          TIMESTAMPTZ,
    last_failure_at          TIMESTAMPTZ,
    last_error               TEXT
);

-- Cross-venue positions, replacing the in-memory PositionIndex.
CREATE TABLE positions (
    position_id          TEXT PRIMARY KEY,
    canonical_market_id  TEXT NOT NULL,
    canonical_outcome_id TEXT NOT NULL,
    wallet                TEXT NOT NULL,
    venue_id              TEXT NOT NULL,
    venue_position_id     TEXT NOT NULL,
    quantity               TEXT NOT NULL,
    entry_price             TEXT NOT NULL,
    status                   TEXT NOT NULL,
    recorded_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX positions_wallet_idx ON positions (wallet);
