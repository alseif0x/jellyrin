CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_search_hints_trigram
    ON media_items USING gin ((lower(
        COALESCE(name, '') || ' ' ||
        COALESCE(metadata ->> 'Album', '') || ' ' ||
        COALESCE(metadata ->> 'AlbumName', '') || ' ' ||
        COALESCE(metadata ->> 'AlbumArtist', '') || ' ' ||
        COALESCE(metadata ->> 'AlbumArtists', '') || ' ' ||
        COALESCE(metadata ->> 'SeriesName', '') || ' ' ||
        COALESCE(metadata ->> 'Series', '') || ' ' ||
        COALESCE(metadata ->> 'Artists', '')
    )) gin_trgm_ops)
    WHERE missing_since IS NULL;
