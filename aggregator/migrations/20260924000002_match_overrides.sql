-- Operator decisions about cross-venue matching, honoured by every rebuild of the event graph.
--
-- Everything else in the graph is derived: it is rebuilt from the persisted markets (whose full
-- venue payload is kept in `markets.payload`) and its ids are hashes of venue identifiers, so a
-- restart reproduces the same events with the same ids without storing the graph itself. These
-- rows are the one input that can't be re-derived.
--
-- Keyed by `venue:market_id` pairs rather than event or question ids, so an override keeps
-- applying however the rest of the graph changes around it. Each pair is stored with
-- `left_listing < right_listing`, so a pair has exactly one row whichever way it was submitted.

CREATE TABLE match_overrides (
    left_listing   TEXT        NOT NULL,
    right_listing  TEXT        NOT NULL,
    decision       TEXT        NOT NULL CHECK (decision IN ('merge', 'split')),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (left_listing, right_listing),
    CHECK (left_listing < right_listing)
);
