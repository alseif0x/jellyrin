CREATE TABLE media_item_filter_selectors (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    selector_kind TEXT NOT NULL CHECK (selector_kind IN ('person', 'studio', 'tag')),
    selector TEXT NOT NULL,
    PRIMARY KEY (item_id, selector_kind, selector)
);

CREATE INDEX media_item_filter_selectors_lookup_idx
    ON media_item_filter_selectors (selector_kind, selector, item_id);
