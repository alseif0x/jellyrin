CREATE TABLE media_item_filter_selectors (
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    selector_kind text NOT NULL CHECK (selector_kind IN ('person', 'studio', 'tag')),
    selector text NOT NULL,
    PRIMARY KEY (item_id, selector_kind, selector)
);

CREATE INDEX media_item_filter_selectors_lookup_idx
    ON media_item_filter_selectors (selector_kind, selector, item_id);
