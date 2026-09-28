-- Keeps everything the venue sent, not just the columns above.
--
-- Phase 2 of the aggregator plan (docs/aggregator-plan.md): no venue metadata may be lost between
-- the wire and the store. The typed columns stay for querying; `payload` holds the full
-- `VenueMarket` as JSON (event tags, league, teams, market kind, the venue's untouched `extra`
-- fields, ...), so a field added to the model round-trips without another migration.
--
-- `venue_category` is the venue's own category name, verbatim; `category` is the shared name
-- after aliasing (e.g. Jupiter `climate & science` -> `science`).

ALTER TABLE markets
    ADD COLUMN venue_category TEXT,
    ADD COLUMN payload        JSONB;

-- A venue's whole catalog is replaced at once now, not per category.
CREATE INDEX markets_venue_idx ON markets (venue_id);
