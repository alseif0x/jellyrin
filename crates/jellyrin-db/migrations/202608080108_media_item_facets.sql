CREATE TABLE media_item_facets (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    facet_kind TEXT NOT NULL CHECK (facet_kind IN (
        'genre', 'music_genre', 'music_artist', 'music_album_artist', 'music_album',
        'person', 'studio', 'tag', 'year'
    )),
    normalized_value TEXT NOT NULL,
    display_value TEXT NOT NULL,
    stable_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    payload_json TEXT NOT NULL,
    PRIMARY KEY (item_id, facet_kind, normalized_value)
);

CREATE INDEX media_item_facets_value_idx
    ON media_item_facets (facet_kind, normalized_value, item_id);
CREATE INDEX media_item_facets_stable_id_idx
    ON media_item_facets (facet_kind, stable_id, item_id);

CREATE TABLE media_item_facet_aliases (
    item_id TEXT NOT NULL,
    facet_kind TEXT NOT NULL,
    normalized_value TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    PRIMARY KEY (item_id, facet_kind, normalized_value, entity_id),
    FOREIGN KEY (item_id, facet_kind, normalized_value)
        REFERENCES media_item_facets(item_id, facet_kind, normalized_value)
        ON DELETE CASCADE
);

CREATE INDEX media_item_facet_aliases_entity_idx
    ON media_item_facet_aliases (facet_kind, entity_id, item_id);
