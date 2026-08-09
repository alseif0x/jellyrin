CREATE TABLE media_item_facets (
    item_id uuid NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    facet_kind text NOT NULL CHECK (facet_kind IN (
        'genre', 'music_genre', 'music_artist', 'music_album_artist', 'music_album',
        'person', 'studio', 'tag', 'year'
    )),
    normalized_value text NOT NULL,
    display_value text NOT NULL,
    stable_id text NOT NULL,
    position integer NOT NULL CHECK (position >= 0),
    payload jsonb NOT NULL,
    PRIMARY KEY (item_id, facet_kind, normalized_value)
);

CREATE INDEX media_item_facets_value_idx
    ON media_item_facets (facet_kind, normalized_value, item_id);
CREATE INDEX media_item_facets_stable_id_idx
    ON media_item_facets (facet_kind, stable_id, item_id);

CREATE TABLE media_item_facet_aliases (
    item_id uuid NOT NULL,
    facet_kind text NOT NULL,
    normalized_value text NOT NULL,
    entity_id text NOT NULL,
    PRIMARY KEY (item_id, facet_kind, normalized_value, entity_id),
    FOREIGN KEY (item_id, facet_kind, normalized_value)
        REFERENCES media_item_facets(item_id, facet_kind, normalized_value)
        ON DELETE CASCADE
);

CREATE INDEX media_item_facet_aliases_entity_idx
    ON media_item_facet_aliases (facet_kind, entity_id, item_id);
