-- One compressed snapshot per venue replaces the row-per-market `markets` table.
--
-- Nothing ever queried `markets` by row: the API serves from memory, and the table was only
-- written in full on every refresh and read in full at boot. Row by row that was ~67 MB each way
-- (every Jupiter market repeats its event and rules text), which took 15-110 s against the hosted
-- database. The repetition is between markets, so compressing a venue's catalog as one blob gets
-- it to a few MB; per-row compression only halves it.
--
-- `payload` is the venue's `[(category, [VenueMarket])]` as JSON, zstd-compressed. `encoding`
-- names that format so a future change can be told apart rather than misread. The markets are
-- derived data - the next refresh rewrites them - so a snapshot that can't be read is skipped,
-- not an error.

CREATE TABLE market_snapshots (
    venue_id      TEXT        PRIMARY KEY,
    encoding      TEXT        NOT NULL,
    market_count  INTEGER     NOT NULL,
    payload       BYTEA       NOT NULL,
    written_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

DROP TABLE markets;
