-- Wholphin's library home requests are folder-scoped and immediately paged. Keep their
-- PremiereDate, CommunityRating and DatePlayed shelves index-driven even for very large remote
-- catalogues.
CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_tv_premiere
    ON media_items (
        virtual_folder_id,
        public.jellyrin_metadata_timestamp(metadata, ARRAY['PremiereDate', 'AirDate', 'DateCreated']) DESC,
        id DESC
    )
    WHERE missing_since IS NULL
      AND media_type = 'Video'
      AND collection_type IN ('tvshows', 'tvshow', 'series');

CREATE INDEX IF NOT EXISTS idx_media_items_visible_folder_series_rating
    ON media_items (
        virtual_folder_id,
        public.jellyrin_metadata_number(metadata, ARRAY['CommunityRating', 'Rating']) DESC,
        id DESC
    )
    WHERE missing_since IS NULL
      AND media_type = 'Series';

CREATE INDEX IF NOT EXISTS idx_playback_states_user_played_date
    ON playback_states (user_id, played, updated_at DESC, item_id);
