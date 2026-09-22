-- Carries each market's parent event and resolution terms.
--
-- Jupiter nests markets under events: the event holds the question ("Atlanta Dream vs New York
-- Liberty") while the market holds only its outcome ("Atlanta Dream"). Persisting just the market
-- is what left the stored catalog full of bare labels — "Up", "Down", "160-179", "September 21" —
-- with nothing recording what was being predicted.
--
-- This is the venue's *own* event grouping, not the canonical cross-venue event graph in the
-- `events`/`outcomes` tables from the initial migration. Those are unchanged and still serve
-- /events; nothing here is matched or merged across venues.
--
-- All nullable: Panta publishes the whole question on the market itself and has no event layer,
-- so these stay NULL for its rows.

ALTER TABLE markets
    ADD COLUMN event_id         TEXT,
    ADD COLUMN event_title      TEXT,
    ADD COLUMN event_subtitle   TEXT,
    ADD COLUMN event_image_url  TEXT,
    -- The venue's full resolution terms: what counts as a win, and from which source.
    ADD COLUMN rules            TEXT;

-- Markets of the same event are read together on a detail view.
CREATE INDEX markets_event_id_idx ON markets (event_id) WHERE event_id IS NOT NULL;
