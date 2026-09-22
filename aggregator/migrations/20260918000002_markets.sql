-- Raw venue markets, the backing store for `GET /markets`.
--
-- Deliberately a mirror of `models::venue::VenueMarket`, not a normalized redesign: the markets
-- path serves venue data untransformed, so the table that persists it has the same shape. The
-- canonical event graph tables from the initial migration are unchanged and still serve /events.
--
-- Money and prices are TEXT, matching the existing schema's convention - these are exact decimal
-- strings from the venues, and routing them through a float type would introduce rounding error
-- the rest of the service works hard to avoid.

CREATE TABLE markets (
    venue_id          TEXT   NOT NULL,
    venue_market_id   TEXT   NOT NULL,
    title             TEXT   NOT NULL,
    category          TEXT   NOT NULL,
    phase             TEXT   NOT NULL,
    start_time        BIGINT NOT NULL,
    -- When betting closes. Distinct from resolution_time on both integrated venues (Panta's
    -- `endTime`, Jupiter's `closeTime`) and the column every window query filters on.
    close_time        BIGINT NOT NULL,
    resolution_time   BIGINT NOT NULL,
    yes_price         TEXT,
    no_price          TEXT,
    -- Venue-reported traded volume, the depth signal `sort=best_liquidity` ranks on. NULL means
    -- the venue reports none - distinct from 0, and ordered last rather than as zero-volume.
    liquidity         TEXT,
    dead              BOOLEAN NOT NULL DEFAULT FALSE,
    last_synced_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (venue_id, venue_market_id)
);

-- The exact access path `GET /markets` uses: filter by category, bound by close_time.
CREATE INDEX markets_category_close_time_idx ON markets (category, close_time);

-- Supports pruning a (venue, category) bucket on refresh.
CREATE INDEX markets_venue_category_idx ON markets (venue_id, category);
