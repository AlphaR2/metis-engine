-- A model's decision about which event a lone market belongs to, cached so each market is asked
-- about once. Keyed by `venue:market_id`; `content_hash` covers the text the decision was based on,
-- so an edited market is asked again. The decisions are derived data: losing them only costs a few
-- model calls on the next refresh.

CREATE TABLE market_subjects (
    market_key    TEXT        PRIMARY KEY,
    content_hash  TEXT        NOT NULL,
    join_key      TEXT,
    group_key     TEXT,
    title         TEXT,
    model         TEXT        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
