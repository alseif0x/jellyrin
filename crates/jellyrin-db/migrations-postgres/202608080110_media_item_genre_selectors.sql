CREATE TABLE media_item_genre_selectors (
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    selector text NOT NULL,
    PRIMARY KEY (item_id, selector)
);

CREATE INDEX media_item_genre_selectors_lookup_idx
    ON media_item_genre_selectors (selector, item_id);
