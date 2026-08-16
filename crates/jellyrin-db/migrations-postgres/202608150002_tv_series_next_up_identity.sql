ALTER TABLE media_item_tv_series_members
    ADD COLUMN item_name text NOT NULL DEFAULT '',
    ADD COLUMN item_path text NOT NULL DEFAULT '';

UPDATE media_item_tv_series_members AS member
SET item_name = item.name,
    item_path = item.path
FROM media_items AS item
WHERE item.id = member.item_id;

DROP INDEX idx_media_item_tv_series_members_next_up;

CREATE INDEX idx_media_item_tv_series_members_next_up
ON media_item_tv_series_members (
    series_id, season_number, episode_number, sort_name, item_id
)
INCLUDE (virtual_folder_id, item_name, item_path);

UPDATE media_item_tv_series_coverage
SET projection_version = 2,
    completed_at = CURRENT_TIMESTAMP
WHERE projection_version = 2;
