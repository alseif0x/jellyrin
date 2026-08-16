ALTER TABLE media_item_tv_series_members
    ADD COLUMN season_number integer NOT NULL DEFAULT 2147483647,
    ADD COLUMN episode_number integer NOT NULL DEFAULT 2147483647,
    ADD COLUMN sort_name text NOT NULL DEFAULT '';

UPDATE media_item_tv_series_members AS member
SET season_number = COALESCE(
        CASE WHEN btrim(item.metadata->>'ParentIndexNumber') ~ '^[0-9]+$'
             THEN (item.metadata->>'ParentIndexNumber')::integer END,
        2147483647
    ),
    episode_number = COALESCE(
        CASE WHEN btrim(item.metadata->>'IndexNumber') ~ '^[0-9]+$'
             THEN (item.metadata->>'IndexNumber')::integer END,
        2147483647
    ),
    sort_name = lower(item.name)
FROM media_items AS item
WHERE item.id = member.item_id;

CREATE INDEX idx_media_item_tv_series_members_next_up
ON media_item_tv_series_members (
    series_id, season_number, episode_number, sort_name, item_id
);

UPDATE media_item_tv_series_coverage
SET projection_version = 2,
    completed_at = CURRENT_TIMESTAMP
WHERE projection_version = 1;
