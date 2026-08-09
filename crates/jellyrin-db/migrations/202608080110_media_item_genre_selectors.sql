CREATE TABLE media_item_genre_selectors (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    selector TEXT NOT NULL,
    PRIMARY KEY (item_id, selector)
);

CREATE INDEX media_item_genre_selectors_lookup_idx
    ON media_item_genre_selectors (selector, item_id);

-- SQLite remains a migration/test backend, but persistent legacy files still need a durable
-- extractor marker so the application can rebuild this non-SQL projection exactly once.
CREATE TABLE jellyrin_derived_projection_versions (
    projection_name TEXT PRIMARY KEY,
    extractor_version INTEGER NOT NULL CHECK (extractor_version > 0),
    completed_at TEXT NOT NULL,
    source_item_count INTEGER NOT NULL CHECK (source_item_count >= 0),
    projected_facet_count INTEGER NOT NULL CHECK (projected_facet_count >= 0),
    projected_alias_count INTEGER NOT NULL CHECK (projected_alias_count >= 0)
);
