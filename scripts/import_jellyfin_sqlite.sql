PRAGMA busy_timeout = 10000;
ATTACH DATABASE '/home/cdmonio/dev/jellyfin-data/data/data/jellyfin.db' AS jf;

BEGIN IMMEDIATE;

DELETE FROM active_viewing_sessions;
DELETE FROM active_playback_sessions;
DELETE FROM active_session_users;
DELETE FROM transcode_sessions;
DELETE FROM playback_states;
DELETE FROM media_item_versions;
DELETE FROM trickplay_infos;
DELETE FROM media_item_lyrics;
DELETE FROM media_list_items;
DELETE FROM media_lists;
DELETE FROM media_item_deletions;
DELETE FROM media_items;
DELETE FROM virtual_folders;

INSERT INTO users (
    id, name, is_administrator, is_disabled, sync_play_access, created_at, updated_at
)
SELECT
    lower(u.Id),
    u.Username,
    1,
    0,
    CASE u.SyncPlayAccess
        WHEN 0 THEN 'None'
        WHEN 1 THEN 'JoinGroups'
        ELSE 'CreateAndJoinGroups'
    END,
    COALESCE(u.LastLoginDate, u.LastActivityDate, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    COALESCE(u.LastActivityDate, u.LastLoginDate, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
FROM jf.Users u
WHERE u.Username IS NOT NULL AND trim(u.Username) != ''
ON CONFLICT(id) DO UPDATE SET
    name = excluded.name,
    is_administrator = excluded.is_administrator,
    is_disabled = excluded.is_disabled,
    sync_play_access = excluded.sync_play_access,
    updated_at = excluded.updated_at;

WITH folders AS (
    SELECT
        lower(root.Id) AS id,
        root.Name AS name,
        CASE
            WHEN EXISTS (
                SELECT 1
                FROM jf.BaseItems child
                WHERE child.TopParentId = root.Id
                  AND child.Type LIKE '%Audio.Audio'
            ) THEN 'music'
            WHEN EXISTS (
                SELECT 1
                FROM jf.BaseItems child
                WHERE child.TopParentId = root.Id
                  AND (
                    child.Type LIKE '%TV.Series'
                    OR child.Type LIKE '%TV.Season'
                    OR child.Type LIKE '%TV.Episode'
                  )
            ) THEN 'tvshows'
            ELSE 'movies'
        END AS collection_type,
        json_array(root.Path) AS locations_json,
        COALESCE(root.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS created_at,
        COALESCE(root.DateLastSaved, root.DateModified, root.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at
    FROM jf.BaseItems root
    WHERE root.Id = root.TopParentId
      AND root.Path IS NOT NULL
      AND trim(root.Path) != ''
      AND root.Path NOT LIKE '%AppDataPath%'
      AND root.Path NOT LIKE '%MetadataPath%'
      AND EXISTS (
          SELECT 1
          FROM jf.BaseItems child
          WHERE child.TopParentId = root.Id
            AND child.MediaType IN ('Video', 'Audio')
            AND child.Path IS NOT NULL
            AND trim(child.Path) != ''
            AND child.Path NOT LIKE '%AppDataPath%'
            AND child.Path NOT LIKE '%MetadataPath%'
      )
)
INSERT OR IGNORE INTO virtual_folders (
    id, name, collection_type, locations_json, created_at, updated_at
)
SELECT id, name, collection_type, locations_json, created_at, updated_at
FROM folders;

WITH collection_folders AS (
    SELECT
        lower(i.Id) AS id,
        COALESCE(NULLIF(trim(i.Name), ''), i.Id) AS name,
        json_extract(i.Data, '$.CollectionType') AS collection_type,
        COALESCE(
            json_extract(i.Data, '$.PhysicalLocationsList'),
            json_array(i.Path)
        ) AS locations_json,
        COALESCE(i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS created_at,
        COALESCE(i.DateLastSaved, i.DateModified, i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at
    FROM jf.BaseItems i
    WHERE i.Type LIKE '%CollectionFolder'
      AND json_extract(i.Data, '$.CollectionType') IS NOT NULL
      AND (
          json_extract(i.Data, '$.PhysicalLocationsList') IS NOT NULL
          OR (i.Path IS NOT NULL AND trim(i.Path) != '')
      )
),
physical_collection_folders AS (
    SELECT
        lower(folder.Id) AS id,
        COALESCE(NULLIF(trim(folder.Name), ''), folder.Id) AS name,
        json_extract(collection.Data, '$.CollectionType') AS collection_type,
        json_array(folder.Path) AS locations_json,
        COALESCE(folder.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS created_at,
        COALESCE(folder.DateLastSaved, folder.DateModified, folder.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at
    FROM jf.BaseItems collection
    JOIN json_each(collection.Data, '$.PhysicalFolderIds') physical_folder
    JOIN jf.BaseItems folder ON lower(replace(folder.Id, '-', '')) = lower(replace(physical_folder.value, '-', ''))
    WHERE collection.Type LIKE '%CollectionFolder'
      AND json_extract(collection.Data, '$.CollectionType') IS NOT NULL
      AND folder.Path IS NOT NULL
      AND trim(folder.Path) != ''
      AND folder.Path NOT LIKE '%AppDataPath%'
      AND folder.Path NOT LIKE '%MetadataPath%'
)
INSERT OR IGNORE INTO virtual_folders (
    id, name, collection_type, locations_json, created_at, updated_at
)
SELECT id, name, collection_type, locations_json, created_at, updated_at FROM collection_folders
UNION ALL
SELECT id, name, collection_type, locations_json, created_at, updated_at FROM physical_collection_folders;

WITH folders AS (
    SELECT
        lower(root.Id) AS id,
        root.Name AS name,
        CASE
            WHEN EXISTS (
                SELECT 1 FROM jf.BaseItems child
                WHERE child.TopParentId = root.Id AND child.Type LIKE '%Audio.Audio'
            ) THEN 'music'
            WHEN EXISTS (
                SELECT 1 FROM jf.BaseItems child
                WHERE child.TopParentId = root.Id
                  AND (child.Type LIKE '%TV.Series' OR child.Type LIKE '%TV.Season' OR child.Type LIKE '%TV.Episode')
            ) THEN 'tvshows'
            ELSE 'movies'
        END AS collection_type,
        json_array(root.Path) AS locations_json,
        COALESCE(root.DateLastSaved, root.DateModified, root.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at
    FROM jf.BaseItems root
    WHERE root.Id = root.TopParentId
      AND root.Path IS NOT NULL
      AND trim(root.Path) != ''
      AND root.Path NOT LIKE '%AppDataPath%'
      AND root.Path NOT LIKE '%MetadataPath%'
)
UPDATE virtual_folders
SET
    name = (SELECT folders.name FROM folders WHERE folders.id = virtual_folders.id),
    collection_type = (SELECT folders.collection_type FROM folders WHERE folders.id = virtual_folders.id),
    locations_json = (SELECT folders.locations_json FROM folders WHERE folders.id = virtual_folders.id),
    updated_at = (SELECT folders.updated_at FROM folders WHERE folders.id = virtual_folders.id)
WHERE id IN (SELECT id FROM folders);

WITH stream_json AS (
    SELECT
        lower(ms.ItemId) AS item_id,
        json_group_array(
            json_object(
                'Index', ms.StreamIndex,
                'Type', CASE ms.StreamType
                    WHEN 0 THEN 'Audio'
                    WHEN 1 THEN 'Video'
                    WHEN 2 THEN 'Subtitle'
                    ELSE 'Unknown'
                END,
                'Codec', ms.Codec,
                'CodecTag', ms.CodecTag,
                'Language', ms.Language,
                'Title', ms.Title,
                'DisplayTitle', COALESCE(ms.Title, ms.Codec),
                'IsDefault', CASE WHEN ms.IsDefault != 0 THEN json('true') ELSE json('false') END,
                'IsForced', CASE WHEN ms.IsForced != 0 THEN json('true') ELSE json('false') END,
                'IsExternal', CASE WHEN ms.IsExternal != 0 THEN json('true') ELSE json('false') END,
                'IsTextSubtitleStream', CASE
                    WHEN ms.StreamType = 2 AND lower(COALESCE(ms.Codec, '')) IN ('srt', 'subrip', 'ass', 'ssa', 'webvtt', 'vtt') THEN json('true')
                    ELSE json('false')
                END,
                'ChannelLayout', ms.ChannelLayout,
                'Channels', ms.Channels,
                'SampleRate', ms.SampleRate,
                'BitRate', ms.BitRate,
                'BitDepth', ms.BitDepth,
                'Width', ms.Width,
                'Height', ms.Height,
                'Profile', ms.Profile,
                'PixelFormat', ms.PixelFormat,
                'AverageFrameRate', ms.AverageFrameRate,
                'RealFrameRate', ms.RealFrameRate,
                'Path', ms.Path
            )
        ) AS media_streams_json
    FROM jf.MediaStreamInfos ms
    GROUP BY lower(ms.ItemId)
),
provider_json AS (
    SELECT
        lower(ItemId) AS item_id,
        json_group_object(ProviderId, ProviderValue) AS provider_ids_json
    FROM jf.BaseItemProviders
    GROUP BY lower(ItemId)
),
people_json AS (
    SELECT
        item_id,
        json_group_array(
            json_object(
                'Name', name,
                'Id', lower(replace(person_id, '-', '')),
                'Type', COALESCE(NULLIF(person_type, ''), 'Actor'),
                'Role', NULLIF(role, '')
            )
        ) AS people_json
    FROM (
        SELECT
            lower(map.ItemId) AS item_id,
            map.PeopleId AS person_id,
            people.Name AS name,
            people.PersonType AS person_type,
            map.Role AS role,
            COALESCE(map.ListOrder, map.SortOrder, 0) AS sort_order
        FROM jf.PeopleBaseItemMap map
        JOIN jf.Peoples people ON people.Id = map.PeopleId
        ORDER BY lower(map.ItemId), sort_order, people.Name COLLATE NOCASE
    )
    GROUP BY item_id
),
items AS (
    SELECT
        lower(i.Id) AS id,
        lower(i.TopParentId) AS virtual_folder_id,
        COALESCE(NULLIF(trim(i.Name), ''), i.Path) AS name,
        i.Path AS path,
        CASE i.MediaType WHEN 'Audio' THEN 'Audio' ELSE 'Video' END AS media_type,
        CASE
            WHEN i.Type LIKE '%Audio.Audio' THEN 'music'
            WHEN i.Type LIKE '%TV.Episode' THEN 'tvshows'
            ELSE 'movies'
        END AS collection_type,
        COALESCE(i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS created_at,
        COALESCE(i.DateLastSaved, i.DateModified, i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at,
        i.Size AS file_size,
        i.DateModified AS modified_at,
        i.RunTimeTicks AS runtime_ticks,
        i.TotalBitrate AS bitrate,
        i.Width AS width,
        i.Height AS height,
        COALESCE(s.media_streams_json, json_array()) AS media_streams_json,
        json_object(
            'Name', i.Name,
            'OriginalTitle', i.OriginalTitle,
            'Overview', i.Overview,
            'CommunityRating', i.CommunityRating,
            'CriticRating', i.CriticRating,
            'OfficialRating', i.OfficialRating,
            'PremiereDate', i.PremiereDate,
            'ProductionYear', i.ProductionYear,
            'Genres', CASE
                WHEN i.Genres IS NULL OR trim(i.Genres) = '' THEN json_array()
                ELSE json_array(i.Genres)
            END,
            'Studios', CASE
                WHEN i.Studios IS NULL OR trim(i.Studios) = '' THEN json_array()
                ELSE json_array(i.Studios)
            END,
            'Tags', CASE
                WHEN i.Tags IS NULL OR trim(i.Tags) = '' THEN json_array()
                ELSE json_array(i.Tags)
            END,
            'SeriesName', i.SeriesName,
            'SeriesId', lower(i.SeriesId),
            'SeasonId', lower(i.SeasonId),
            'SeasonName', i.SeasonName,
            'ParentIndexNumber', i.ParentIndexNumber,
            'IndexNumber', i.IndexNumber,
            'ProviderIds', json_patch(
                json_object('Jellyfin', lower(i.Id), 'External', i.ExternalId),
                COALESCE(p.provider_ids_json, json_object())
            ),
            'People', json(COALESCE(pe.people_json, json_array())),
            'BackdropImageTag', (
                SELECT lower(replace(img.Id, '-', ''))
                FROM jf.BaseItemImageInfos img
                WHERE img.ItemId = i.Id
                  AND img.ImageType = 2
                ORDER BY img.DateModified DESC
                LIMIT 1
            ),
            'BackdropImagePath', (
                SELECT img.Path
                FROM jf.BaseItemImageInfos img
                WHERE img.ItemId = i.Id
                  AND img.ImageType = 2
                ORDER BY img.DateModified DESC
                LIMIT 1
            ),
            'MetadataSource', 'JellyfinSQLite'
        ) AS metadata_json
    FROM jf.BaseItems i
    LEFT JOIN stream_json s ON s.item_id = lower(i.Id)
    LEFT JOIN provider_json p ON p.item_id = lower(i.Id)
    LEFT JOIN people_json pe ON pe.item_id = lower(i.Id)
    WHERE i.MediaType IN ('Video', 'Audio')
      AND i.Path IS NOT NULL
      AND trim(i.Path) != ''
      AND i.Path NOT LIKE '%AppDataPath%'
      AND i.Path NOT LIKE '%MetadataPath%'
      AND i.TopParentId IS NOT NULL
      AND EXISTS (
          SELECT 1 FROM virtual_folders vf WHERE vf.id = lower(i.TopParentId)
      )
)
INSERT OR IGNORE INTO media_items (
    id, virtual_folder_id, name, path, media_type, collection_type,
    created_at, updated_at, last_seen_at, missing_since, file_size,
    modified_at, runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
)
SELECT
    id, virtual_folder_id, name, path, media_type, collection_type,
    created_at, updated_at, updated_at, NULL, file_size,
    modified_at, runtime_ticks, bitrate, width, height, media_streams_json, metadata_json
FROM items;

WITH stream_json AS (
    SELECT
        lower(ms.ItemId) AS item_id,
        json_group_array(
            json_object(
                'Index', ms.StreamIndex,
                'Type', CASE ms.StreamType WHEN 0 THEN 'Audio' WHEN 1 THEN 'Video' WHEN 2 THEN 'Subtitle' ELSE 'Unknown' END,
                'Codec', ms.Codec,
                'CodecTag', ms.CodecTag,
                'Language', ms.Language,
                'Title', ms.Title,
                'DisplayTitle', COALESCE(ms.Title, ms.Codec),
                'IsDefault', CASE WHEN ms.IsDefault != 0 THEN json('true') ELSE json('false') END,
                'IsForced', CASE WHEN ms.IsForced != 0 THEN json('true') ELSE json('false') END,
                'IsExternal', CASE WHEN ms.IsExternal != 0 THEN json('true') ELSE json('false') END,
                'IsTextSubtitleStream', CASE
                    WHEN ms.StreamType = 2 AND lower(COALESCE(ms.Codec, '')) IN ('srt', 'subrip', 'ass', 'ssa', 'webvtt', 'vtt') THEN json('true')
                    ELSE json('false')
                END,
                'ChannelLayout', ms.ChannelLayout,
                'Channels', ms.Channels,
                'SampleRate', ms.SampleRate,
                'BitRate', ms.BitRate,
                'BitDepth', ms.BitDepth,
                'Width', ms.Width,
                'Height', ms.Height,
                'Profile', ms.Profile,
                'PixelFormat', ms.PixelFormat,
                'AverageFrameRate', ms.AverageFrameRate,
                'RealFrameRate', ms.RealFrameRate,
                'Path', ms.Path
            )
        ) AS media_streams_json
    FROM jf.MediaStreamInfos ms
    GROUP BY lower(ms.ItemId)
),
provider_json AS (
    SELECT
        lower(ItemId) AS item_id,
        json_group_object(ProviderId, ProviderValue) AS provider_ids_json
    FROM jf.BaseItemProviders
    GROUP BY lower(ItemId)
),
people_json AS (
    SELECT
        item_id,
        json_group_array(
            json_object(
                'Name', name,
                'Id', lower(replace(person_id, '-', '')),
                'Type', COALESCE(NULLIF(person_type, ''), 'Actor'),
                'Role', NULLIF(role, '')
            )
        ) AS people_json
    FROM (
        SELECT
            lower(map.ItemId) AS item_id,
            map.PeopleId AS person_id,
            people.Name AS name,
            people.PersonType AS person_type,
            map.Role AS role,
            COALESCE(map.ListOrder, map.SortOrder, 0) AS sort_order
        FROM jf.PeopleBaseItemMap map
        JOIN jf.Peoples people ON people.Id = map.PeopleId
        ORDER BY lower(map.ItemId), sort_order, people.Name COLLATE NOCASE
    )
    GROUP BY item_id
),
items AS (
    SELECT
        lower(i.Id) AS id,
        lower(i.TopParentId) AS virtual_folder_id,
        COALESCE(NULLIF(trim(i.Name), ''), i.Path) AS name,
        i.Path AS path,
        CASE i.MediaType WHEN 'Audio' THEN 'Audio' ELSE 'Video' END AS media_type,
        CASE
            WHEN i.Type LIKE '%Audio.Audio' THEN 'music'
            WHEN i.Type LIKE '%TV.Episode' THEN 'tvshows'
            ELSE 'movies'
        END AS collection_type,
        COALESCE(i.DateLastSaved, i.DateModified, i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at,
        i.Size AS file_size,
        i.DateModified AS modified_at,
        i.RunTimeTicks AS runtime_ticks,
        i.TotalBitrate AS bitrate,
        i.Width AS width,
        i.Height AS height,
        COALESCE(s.media_streams_json, json_array()) AS media_streams_json,
        json_object(
            'Name', i.Name,
            'OriginalTitle', i.OriginalTitle,
            'Overview', i.Overview,
            'CommunityRating', i.CommunityRating,
            'CriticRating', i.CriticRating,
            'OfficialRating', i.OfficialRating,
            'PremiereDate', i.PremiereDate,
            'ProductionYear', i.ProductionYear,
            'Genres', CASE WHEN i.Genres IS NULL OR trim(i.Genres) = '' THEN json_array() ELSE json_array(i.Genres) END,
            'Studios', CASE WHEN i.Studios IS NULL OR trim(i.Studios) = '' THEN json_array() ELSE json_array(i.Studios) END,
            'Tags', CASE WHEN i.Tags IS NULL OR trim(i.Tags) = '' THEN json_array() ELSE json_array(i.Tags) END,
            'SeriesName', i.SeriesName,
            'SeriesId', lower(i.SeriesId),
            'SeasonId', lower(i.SeasonId),
            'SeasonName', i.SeasonName,
            'ParentIndexNumber', i.ParentIndexNumber,
            'IndexNumber', i.IndexNumber,
            'ProviderIds', json_patch(
                json_object('Jellyfin', lower(i.Id), 'External', i.ExternalId),
                COALESCE(p.provider_ids_json, json_object())
            ),
            'People', json(COALESCE(pe.people_json, json_array())),
            'BackdropImageTag', (
                SELECT lower(replace(img.Id, '-', ''))
                FROM jf.BaseItemImageInfos img
                WHERE img.ItemId = i.Id
                  AND img.ImageType = 2
                ORDER BY img.DateModified DESC
                LIMIT 1
            ),
            'BackdropImagePath', (
                SELECT img.Path
                FROM jf.BaseItemImageInfos img
                WHERE img.ItemId = i.Id
                  AND img.ImageType = 2
                ORDER BY img.DateModified DESC
                LIMIT 1
            ),
            'MetadataSource', 'JellyfinSQLite'
        ) AS metadata_json
    FROM jf.BaseItems i
    LEFT JOIN stream_json s ON s.item_id = lower(i.Id)
    LEFT JOIN provider_json p ON p.item_id = lower(i.Id)
    LEFT JOIN people_json pe ON pe.item_id = lower(i.Id)
    WHERE i.MediaType IN ('Video', 'Audio')
      AND i.Path IS NOT NULL
      AND trim(i.Path) != ''
      AND i.Path NOT LIKE '%AppDataPath%'
      AND i.Path NOT LIKE '%MetadataPath%'
      AND i.TopParentId IS NOT NULL
      AND EXISTS (SELECT 1 FROM virtual_folders vf WHERE vf.id = lower(i.TopParentId))
)
UPDATE media_items
SET
    id = (SELECT items.id FROM items WHERE items.path = media_items.path),
    virtual_folder_id = (SELECT items.virtual_folder_id FROM items WHERE items.path = media_items.path),
    name = (SELECT items.name FROM items WHERE items.path = media_items.path),
    media_type = (SELECT items.media_type FROM items WHERE items.path = media_items.path),
    collection_type = (SELECT items.collection_type FROM items WHERE items.path = media_items.path),
    updated_at = (SELECT items.updated_at FROM items WHERE items.path = media_items.path),
    last_seen_at = (SELECT items.updated_at FROM items WHERE items.path = media_items.path),
    missing_since = NULL,
    file_size = (SELECT items.file_size FROM items WHERE items.path = media_items.path),
    modified_at = (SELECT items.modified_at FROM items WHERE items.path = media_items.path),
    runtime_ticks = (SELECT items.runtime_ticks FROM items WHERE items.path = media_items.path),
    bitrate = (SELECT items.bitrate FROM items WHERE items.path = media_items.path),
    width = (SELECT items.width FROM items WHERE items.path = media_items.path),
    height = (SELECT items.height FROM items WHERE items.path = media_items.path),
    media_streams_json = (SELECT items.media_streams_json FROM items WHERE items.path = media_items.path),
    metadata_json = (SELECT items.metadata_json FROM items WHERE items.path = media_items.path)
WHERE path IN (SELECT path FROM items);

WITH lists AS (
    SELECT
        lower(i.Id) AS id,
        CASE
            WHEN i.Type LIKE '%Playlists.Playlist' THEN 'playlist'
            ELSE 'collection'
        END AS kind,
        COALESCE(NULLIF(trim(i.Name), ''), i.Id) AS name,
        CASE
            WHEN i.Type LIKE '%Movies.BoxSet' THEN 'boxsets'
            ELSE NULL
        END AS collection_type,
        CASE
            WHEN i.OwnerId IS NOT NULL
             AND EXISTS (SELECT 1 FROM users u WHERE u.id = lower(i.OwnerId))
            THEN lower(i.OwnerId)
            ELSE NULL
        END AS owner_user_id,
        json_object(
            'Overview', i.Overview,
            'ProviderIds', (
                SELECT coalesce(json_group_object(provider.ProviderId, provider.ProviderValue), json('{}'))
                FROM jf.BaseItemProviders provider
                WHERE provider.ItemId = i.Id
            ),
            'PrimaryImageTag', (
                SELECT lower(replace(img.Id, '-', ''))
                FROM jf.BaseItemImageInfos img
                WHERE img.ItemId = i.Id
                  AND img.ImageType = 0
                ORDER BY img.DateModified DESC
                LIMIT 1
            ),
            'PrimaryImagePath', (
                SELECT img.Path
                FROM jf.BaseItemImageInfos img
                WHERE img.ItemId = i.Id
                  AND img.ImageType = 0
                ORDER BY img.DateModified DESC
                LIMIT 1
            )
        ) AS metadata_json,
        COALESCE(i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS created_at,
        COALESCE(i.DateLastSaved, i.DateModified, i.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS updated_at
    FROM jf.BaseItems i
    WHERE i.Type LIKE '%Playlists.Playlist'
       OR i.Type LIKE '%Movies.BoxSet'
)
INSERT INTO media_lists (
    id, kind, name, collection_type, owner_user_id, metadata_json, created_at, updated_at
)
SELECT id, kind, name, collection_type, owner_user_id, metadata_json, created_at, updated_at
FROM lists
WHERE true
ON CONFLICT(id) DO UPDATE SET
    kind = excluded.kind,
    name = excluded.name,
    collection_type = excluded.collection_type,
    owner_user_id = excluded.owner_user_id,
    metadata_json = excluded.metadata_json,
    updated_at = excluded.updated_at;

WITH linked_items AS (
    SELECT
        lower(l.ParentId) AS list_id,
        lower(l.ChildId) AS item_id,
        substr(lower(replace(l.ParentId, '-', '')), 1, 16)
            || substr(lower(replace(l.ChildId, '-', '')), 1, 16) AS playlist_item_id,
        COALESCE(l.SortOrder, ROW_NUMBER() OVER (PARTITION BY l.ParentId ORDER BY child.SortName, child.Name, child.Id) - 1) AS position,
        COALESCE(parent.DateLastSaved, parent.DateModified, parent.DateCreated, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS added_at
    FROM jf.LinkedChildren l
    JOIN jf.BaseItems parent ON parent.Id = l.ParentId
    JOIN jf.BaseItems child ON child.Id = l.ChildId
    WHERE EXISTS (SELECT 1 FROM media_lists ml WHERE ml.id = lower(l.ParentId))
      AND EXISTS (SELECT 1 FROM media_items mi WHERE mi.id = lower(l.ChildId))
)
INSERT INTO media_list_items (
    list_id, item_id, playlist_item_id, position, added_at
)
SELECT list_id, item_id, playlist_item_id, position, added_at
FROM linked_items
WHERE true
ON CONFLICT(list_id, item_id) DO UPDATE SET
    playlist_item_id = excluded.playlist_item_id,
    position = excluded.position,
    added_at = excluded.added_at;

INSERT INTO playback_states (
    user_id, item_id, media_source_id, position_ticks, is_paused, played,
    updated_at, audio_stream_index, subtitle_stream_index, is_favorite, rating
)
SELECT
    lower(ud.UserId),
    lower(ud.ItemId),
    NULL,
    COALESCE(ud.PlaybackPositionTicks, 0),
    0,
    CASE WHEN ud.Played != 0 THEN 1 ELSE 0 END,
    COALESCE(ud.LastPlayedDate, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    ud.AudioStreamIndex,
    ud.SubtitleStreamIndex,
    CASE WHEN ud.IsFavorite != 0 THEN 1 ELSE 0 END,
    ud.Rating
FROM jf.UserData ud
WHERE EXISTS (SELECT 1 FROM users u WHERE u.id = lower(ud.UserId))
  AND EXISTS (SELECT 1 FROM media_items mi WHERE mi.id = lower(ud.ItemId))
ON CONFLICT(user_id, item_id) DO UPDATE SET
    position_ticks = excluded.position_ticks,
    played = excluded.played,
    updated_at = excluded.updated_at,
    audio_stream_index = excluded.audio_stream_index,
    subtitle_stream_index = excluded.subtitle_stream_index,
    is_favorite = excluded.is_favorite,
    rating = excluded.rating;

UPDATE users
SET
    created_at = CASE
        WHEN created_at LIKE '%Z' THEN replace(created_at, ' ', 'T')
        ELSE replace(created_at, ' ', 'T') || 'Z'
    END,
    updated_at = CASE
        WHEN updated_at LIKE '%Z' THEN replace(updated_at, ' ', 'T')
        ELSE replace(updated_at, ' ', 'T') || 'Z'
    END;

UPDATE virtual_folders
SET
    created_at = CASE
        WHEN created_at LIKE '%Z' THEN replace(created_at, ' ', 'T')
        ELSE replace(created_at, ' ', 'T') || 'Z'
    END,
    updated_at = CASE
        WHEN updated_at LIKE '%Z' THEN replace(updated_at, ' ', 'T')
        ELSE replace(updated_at, ' ', 'T') || 'Z'
    END;

UPDATE media_items
SET
    created_at = CASE
        WHEN created_at LIKE '%Z' THEN replace(created_at, ' ', 'T')
        ELSE replace(created_at, ' ', 'T') || 'Z'
    END,
    updated_at = CASE
        WHEN updated_at LIKE '%Z' THEN replace(updated_at, ' ', 'T')
        ELSE replace(updated_at, ' ', 'T') || 'Z'
    END,
    last_seen_at = CASE
        WHEN last_seen_at IS NULL THEN NULL
        WHEN last_seen_at LIKE '%Z' THEN replace(last_seen_at, ' ', 'T')
        ELSE replace(last_seen_at, ' ', 'T') || 'Z'
    END,
    modified_at = CASE
        WHEN modified_at IS NULL THEN NULL
        WHEN modified_at LIKE '%Z' THEN replace(modified_at, ' ', 'T')
        ELSE replace(modified_at, ' ', 'T') || 'Z'
    END,
    metadata_json = CASE
        WHEN json_extract(metadata_json, '$.PremiereDate') IS NULL THEN metadata_json
        WHEN json_extract(metadata_json, '$.PremiereDate') LIKE '%Z' THEN
            json_set(metadata_json, '$.PremiereDate', replace(json_extract(metadata_json, '$.PremiereDate'), ' ', 'T'))
        ELSE
            json_set(metadata_json, '$.PremiereDate', replace(json_extract(metadata_json, '$.PremiereDate'), ' ', 'T') || 'Z')
    END;

UPDATE media_lists
SET
    created_at = CASE
        WHEN created_at LIKE '%Z' THEN replace(created_at, ' ', 'T')
        ELSE replace(created_at, ' ', 'T') || 'Z'
    END,
    updated_at = CASE
        WHEN updated_at LIKE '%Z' THEN replace(updated_at, ' ', 'T')
        ELSE replace(updated_at, ' ', 'T') || 'Z'
    END;

UPDATE media_list_items
SET added_at = CASE
    WHEN added_at LIKE '%Z' THEN replace(added_at, ' ', 'T')
    ELSE replace(added_at, ' ', 'T') || 'Z'
END;

UPDATE playback_states
SET updated_at = CASE
    WHEN updated_at LIKE '%Z' THEN replace(updated_at, ' ', 'T')
    ELSE replace(updated_at, ' ', 'T') || 'Z'
END;

COMMIT;

DETACH DATABASE jf;
