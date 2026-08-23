CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_tv_premiere
    ON media_items (
        virtual_folder_id,
        COALESCE(
            json_extract(metadata_json, '$.PremiereDate'),
            json_extract(metadata_json, '$.AirDate'),
            json_extract(metadata_json, '$.DateCreated')
        ) DESC,
        id DESC
    )
    WHERE missing_since IS NULL
      AND media_type = 'Video'
      AND collection_type IN ('tvshows', 'tvshow', 'series');

CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_series_rating
    ON media_items (
        virtual_folder_id,
        CAST(COALESCE(
            json_extract(metadata_json, '$.CommunityRating'),
            json_extract(metadata_json, '$.Rating')
        ) AS REAL) DESC,
        id DESC
    )
    WHERE missing_since IS NULL
      AND media_type = 'Series';

CREATE INDEX IF NOT EXISTS idx_playback_states_user_played_date
    ON playback_states (user_id, played, updated_at DESC, item_id);
