PRAGMA busy_timeout = 10000;
ATTACH DATABASE '/home/cdmonio/dev/jellyfin-data/data/data/jellyfin.db' AS jf;

BEGIN IMMEDIATE;

UPDATE media_items
SET metadata_json = json_set(
    metadata_json,
    '$.SeriesName', (
        SELECT ser.Name
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesOverview', (
        SELECT ser.Overview
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesOriginalTitle', (
        SELECT ser.OriginalTitle
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesOfficialRating', (
        SELECT ser.OfficialRating
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesCommunityRating', (
        SELECT ser.CommunityRating
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesCriticRating', (
        SELECT ser.CriticRating
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesGenres', (
        SELECT CASE
            WHEN ser.Genres IS NULL OR trim(ser.Genres) = '' THEN json_array()
            ELSE json('["' || replace(replace(ser.Genres, '"', '\"'), '|', '","') || '"]')
        END
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesTags', (
        SELECT CASE
            WHEN ser.Tags IS NULL OR trim(ser.Tags) = '' THEN json_array()
            ELSE json('["' || replace(replace(ser.Tags, '"', '\"'), '|', '","') || '"]')
        END
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesStudios', (
        SELECT CASE
            WHEN ser.Studios IS NULL OR trim(ser.Studios) = '' THEN json_array()
            ELSE json('["' || replace(replace(ser.Studios, '"', '\"'), '|', '","') || '"]')
        END
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesProviderIds', (
        SELECT coalesce(json_group_object(provider.ProviderId, provider.ProviderValue), json('{}'))
        FROM jf.BaseItemProviders provider
        WHERE lower(provider.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
    ),
    '$.SeriesPeople', (
        SELECT coalesce(json_group_array(json_object(
            'Name', ordered.Name,
            'Id', coalesce(ordered.PersonItemId, lower(replace(ordered.PeopleId, '-', ''))),
            'Role', ordered.Role,
            'Type', ordered.PersonType,
            'PrimaryImageTag', ordered.PrimaryImageTag,
            'PrimaryImagePath', ordered.PrimaryImagePath,
            'PrimaryImageAspectRatio', ordered.PrimaryImageAspectRatio,
            'ProviderIds', coalesce(ordered.ProviderIds, json('{}')),
            'SeriesCount', ordered.SeriesCount,
            'EpisodeCount', ordered.EpisodeCount,
            'MovieCount', ordered.MovieCount,
            'AlbumCount', 0,
            'ArtistCount', 0,
            'SongCount', 0,
            'MusicVideoCount', 0,
            'TrailerCount', 0,
            'ProgramCount', 0,
            'SpecialFeatureCount', 0,
            'LocalTrailerCount', 0,
            'ChildCount', ordered.SeriesCount + ordered.EpisodeCount + ordered.MovieCount
        )), json('[]'))
        FROM (
            SELECT
                map.PeopleId,
                map.Role,
                person.Name,
                person.PersonType,
                lower(replace(person_item.Id, '-', '')) AS PersonItemId,
                (
                    SELECT lower(replace(img.Id, '-', ''))
                    FROM jf.BaseItemImageInfos img
                    WHERE img.ItemId = person_item.Id
                      AND img.ImageType = 0
                    ORDER BY img.DateModified DESC
                    LIMIT 1
                ) AS PrimaryImageTag,
                (
                    SELECT img.Path
                    FROM jf.BaseItemImageInfos img
                    WHERE img.ItemId = person_item.Id
                      AND img.ImageType = 0
                    ORDER BY img.DateModified DESC
                    LIMIT 1
                ) AS PrimaryImagePath,
                CASE
                    WHEN person_item.Id IS NULL THEN NULL
                    ELSE 0.6666666666666666
                END AS PrimaryImageAspectRatio,
                (
                    SELECT coalesce(json_group_object(provider.ProviderId, provider.ProviderValue), json('{}'))
                    FROM jf.BaseItemProviders provider
                    WHERE provider.ItemId = person_item.Id
                ) AS ProviderIds,
                (
                    SELECT COUNT(DISTINCT related.SeriesId)
                    FROM jf.Peoples same_name
                    JOIN jf.PeopleBaseItemMap related_map ON related_map.PeopleId = same_name.Id
                    JOIN jf.BaseItems related ON related.Id = related_map.ItemId
                    WHERE same_name.Name = person.Name
                      AND related.Type = 'MediaBrowser.Controller.Entities.TV.Episode'
                      AND related.SeriesId IS NOT NULL
                ) AS SeriesCount,
                (
                    SELECT COUNT(DISTINCT related.Id)
                    FROM jf.Peoples same_name
                    JOIN jf.PeopleBaseItemMap related_map ON related_map.PeopleId = same_name.Id
                    JOIN jf.BaseItems related ON related.Id = related_map.ItemId
                    WHERE same_name.Name = person.Name
                      AND related.Type = 'MediaBrowser.Controller.Entities.TV.Episode'
                ) AS EpisodeCount,
                (
                    SELECT COUNT(DISTINCT related.Id)
                    FROM jf.Peoples same_name
                    JOIN jf.PeopleBaseItemMap related_map ON related_map.PeopleId = same_name.Id
                    JOIN jf.BaseItems related ON related.Id = related_map.ItemId
                    WHERE same_name.Name = person.Name
                      AND related.Type = 'MediaBrowser.Controller.Entities.Movies.Movie'
                ) AS MovieCount
            FROM jf.PeopleBaseItemMap map
            JOIN jf.Peoples person ON person.Id = map.PeopleId
            LEFT JOIN jf.BaseItems person_item
              ON person_item.Name = person.Name
             AND person_item.Type = 'MediaBrowser.Controller.Entities.Person'
            WHERE lower(map.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
            ORDER BY map.ListOrder, map.SortOrder
        ) ordered
    ),
    '$.SeriesRemoteTrailers', (
        SELECT coalesce(json_extract(ser.Data, '$.RemoteTrailers'), json('[]'))
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.People', (
        SELECT coalesce(json_group_array(json_object(
            'Name', ordered.Name,
            'Id', coalesce(ordered.PersonItemId, lower(replace(ordered.PeopleId, '-', ''))),
            'Role', ordered.Role,
            'Type', ordered.PersonType,
            'PrimaryImageTag', ordered.PrimaryImageTag,
            'PrimaryImagePath', ordered.PrimaryImagePath,
            'PrimaryImageAspectRatio', ordered.PrimaryImageAspectRatio,
            'ProviderIds', coalesce(ordered.ProviderIds, json('{}')),
            'SeriesCount', ordered.SeriesCount,
            'EpisodeCount', ordered.EpisodeCount,
            'MovieCount', ordered.MovieCount,
            'AlbumCount', 0,
            'ArtistCount', 0,
            'SongCount', 0,
            'MusicVideoCount', 0,
            'TrailerCount', 0,
            'ProgramCount', 0,
            'SpecialFeatureCount', 0,
            'LocalTrailerCount', 0,
            'ChildCount', ordered.SeriesCount + ordered.EpisodeCount + ordered.MovieCount
        )), json('[]'))
        FROM (
            SELECT
                map.PeopleId,
                map.Role,
                person.Name,
                person.PersonType,
                lower(replace(person_item.Id, '-', '')) AS PersonItemId,
                (
                    SELECT lower(replace(img.Id, '-', ''))
                    FROM jf.BaseItemImageInfos img
                    WHERE img.ItemId = person_item.Id
                      AND img.ImageType = 0
                    ORDER BY img.DateModified DESC
                    LIMIT 1
                ) AS PrimaryImageTag,
                (
                    SELECT img.Path
                    FROM jf.BaseItemImageInfos img
                    WHERE img.ItemId = person_item.Id
                      AND img.ImageType = 0
                    ORDER BY img.DateModified DESC
                    LIMIT 1
                ) AS PrimaryImagePath,
                CASE
                    WHEN person_item.Id IS NULL THEN NULL
                    ELSE 0.6666666666666666
                END AS PrimaryImageAspectRatio,
                (
                    SELECT coalesce(json_group_object(provider.ProviderId, provider.ProviderValue), json('{}'))
                    FROM jf.BaseItemProviders provider
                    WHERE provider.ItemId = person_item.Id
                ) AS ProviderIds,
                (
                    SELECT COUNT(DISTINCT related.SeriesId)
                    FROM jf.Peoples same_name
                    JOIN jf.PeopleBaseItemMap related_map ON related_map.PeopleId = same_name.Id
                    JOIN jf.BaseItems related ON related.Id = related_map.ItemId
                    WHERE same_name.Name = person.Name
                      AND related.Type = 'MediaBrowser.Controller.Entities.TV.Episode'
                      AND related.SeriesId IS NOT NULL
                ) AS SeriesCount,
                (
                    SELECT COUNT(DISTINCT related.Id)
                    FROM jf.Peoples same_name
                    JOIN jf.PeopleBaseItemMap related_map ON related_map.PeopleId = same_name.Id
                    JOIN jf.BaseItems related ON related.Id = related_map.ItemId
                    WHERE same_name.Name = person.Name
                      AND related.Type = 'MediaBrowser.Controller.Entities.TV.Episode'
                ) AS EpisodeCount,
                (
                    SELECT COUNT(DISTINCT related.Id)
                    FROM jf.Peoples same_name
                    JOIN jf.PeopleBaseItemMap related_map ON related_map.PeopleId = same_name.Id
                    JOIN jf.BaseItems related ON related.Id = related_map.ItemId
                    WHERE same_name.Name = person.Name
                      AND related.Type = 'MediaBrowser.Controller.Entities.Movies.Movie'
                ) AS MovieCount
            FROM jf.PeopleBaseItemMap map
            JOIN jf.Peoples person ON person.Id = map.PeopleId
            LEFT JOIN jf.BaseItems person_item
              ON person_item.Name = person.Name
             AND person_item.Type = 'MediaBrowser.Controller.Entities.Person'
            WHERE lower(map.ItemId) = lower(media_items.id)
            ORDER BY map.ListOrder, map.SortOrder
        ) ordered
    ),
    '$.SeriesProductionYear', (
        SELECT ser.ProductionYear
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesPremiereDate', (
        SELECT ser.PremiereDate
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.EndDate', (
        SELECT ser.EndDate
        FROM jf.BaseItems ser
        WHERE lower(ser.Id) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
        LIMIT 1
    ),
    '$.SeriesPrimaryImageTag', (
        SELECT lower(replace(img.Id, '-', ''))
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 0
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesPrimaryImagePath', (
        SELECT img.Path
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 0
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesBackdropImageTag', (
        SELECT lower(replace(img.Id, '-', ''))
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 2
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesBackdropImagePath', (
        SELECT img.Path
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 2
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesLogoImageTag', (
        SELECT lower(replace(img.Id, '-', ''))
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 4
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesLogoImagePath', (
        SELECT img.Path
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 4
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesThumbImageTag', (
        SELECT lower(replace(img.Id, '-', ''))
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 5
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeriesThumbImagePath', (
        SELECT img.Path
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeriesId'))
          AND img.ImageType = 5
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeasonOverview', (
        SELECT sea.Overview
        FROM jf.BaseItems sea
        WHERE lower(sea.Id) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
        LIMIT 1
    ),
    '$.SeasonPremiereDate', (
        SELECT sea.PremiereDate
        FROM jf.BaseItems sea
        WHERE lower(sea.Id) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
        LIMIT 1
    ),
    '$.SeasonEndDate', (
        SELECT sea.EndDate
        FROM jf.BaseItems sea
        WHERE lower(sea.Id) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
        LIMIT 1
    ),
    '$.SeasonProductionYear', (
        SELECT sea.ProductionYear
        FROM jf.BaseItems sea
        WHERE lower(sea.Id) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
        LIMIT 1
    ),
    '$.SeasonProviderIds', (
        SELECT coalesce(json_group_object(provider.ProviderId, provider.ProviderValue), json('{}'))
        FROM jf.BaseItemProviders provider
        WHERE lower(provider.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
    ),
    '$.SeasonExternalUrls', (
        SELECT json_array(
            json_object(
                'Name', 'IMDb',
                'Url', 'https://www.imdb.com/title/' || imdb.ProviderValue || '/episodes/?season=' || sea.IndexNumber
            ),
            json_object(
                'Name', 'TMDB',
                'Url', 'https://www.themoviedb.org/tv/' || tmdb.ProviderValue || '/season/' || sea.IndexNumber
            )
        )
        FROM jf.BaseItems sea
        JOIN jf.BaseItems ser ON ser.Id = sea.SeriesId
        LEFT JOIN jf.BaseItemProviders imdb ON imdb.ItemId = ser.Id AND imdb.ProviderId = 'Imdb'
        LEFT JOIN jf.BaseItemProviders tmdb ON tmdb.ItemId = ser.Id AND tmdb.ProviderId = 'Tmdb'
        WHERE lower(sea.Id) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
          AND imdb.ProviderValue IS NOT NULL
          AND tmdb.ProviderValue IS NOT NULL
        LIMIT 1
    ),
    '$.SeasonPeople', (
        SELECT coalesce(json_group_array(json_object(
            'Name', ordered.Name,
            'Id', coalesce(ordered.PersonItemId, lower(replace(ordered.PeopleId, '-', ''))),
            'Role', ordered.Role,
            'Type', ordered.PersonType,
            'PrimaryImageTag', ordered.PrimaryImageTag,
            'PrimaryImagePath', ordered.PrimaryImagePath,
            'PrimaryImageAspectRatio', ordered.PrimaryImageAspectRatio,
            'ProviderIds', coalesce(ordered.ProviderIds, json('{}'))
        )), json('[]'))
        FROM (
            SELECT
                map.PeopleId,
                map.Role,
                person.Name,
                person.PersonType,
                lower(replace(person_item.Id, '-', '')) AS PersonItemId,
                (
                    SELECT lower(replace(img.Id, '-', ''))
                    FROM jf.BaseItemImageInfos img
                    WHERE img.ItemId = person_item.Id
                      AND img.ImageType = 0
                    ORDER BY img.DateModified DESC
                    LIMIT 1
                ) AS PrimaryImageTag,
                (
                    SELECT img.Path
                    FROM jf.BaseItemImageInfos img
                    WHERE img.ItemId = person_item.Id
                      AND img.ImageType = 0
                    ORDER BY img.DateModified DESC
                    LIMIT 1
                ) AS PrimaryImagePath,
                CASE
                    WHEN person_item.Id IS NULL THEN NULL
                    ELSE 0.6666666666666666
                END AS PrimaryImageAspectRatio,
                (
                    SELECT coalesce(json_group_object(provider.ProviderId, provider.ProviderValue), json('{}'))
                    FROM jf.BaseItemProviders provider
                    WHERE provider.ItemId = person_item.Id
                ) AS ProviderIds
            FROM jf.PeopleBaseItemMap map
            JOIN jf.Peoples person ON person.Id = map.PeopleId
            LEFT JOIN jf.BaseItems person_item
              ON person_item.Name = person.Name
             AND person_item.Type = 'MediaBrowser.Controller.Entities.Person'
            WHERE lower(map.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
            ORDER BY map.ListOrder, map.SortOrder
        ) ordered
    ),
    '$.SeasonPrimaryImageTag', (
        SELECT lower(replace(img.Id, '-', ''))
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
          AND img.ImageType = 0
        ORDER BY img.DateModified DESC
        LIMIT 1
    ),
    '$.SeasonPrimaryImagePath', (
        SELECT img.Path
        FROM jf.BaseItemImageInfos img
        WHERE lower(img.ItemId) = lower(json_extract(media_items.metadata_json, '$.SeasonId'))
          AND img.ImageType = 0
        ORDER BY img.DateModified DESC
        LIMIT 1
    )
)
WHERE json_extract(metadata_json, '$.SeriesId') IS NOT NULL;

UPDATE media_items
SET metadata_json = json_set(
    metadata_json,
    '$.SeriesGenres', (
        SELECT json_group_array(value)
        FROM json_each(json_extract(metadata_json, '$.SeriesGenres'))
        WHERE trim(value) <> ''
    ),
    '$.SeriesTags', (
        SELECT json_group_array(value)
        FROM json_each(json_extract(metadata_json, '$.SeriesTags'))
        WHERE trim(value) <> ''
    ),
    '$.SeriesStudios', (
        SELECT json_group_array(value)
        FROM json_each(json_extract(metadata_json, '$.SeriesStudios'))
        WHERE trim(value) <> ''
    )
)
WHERE json_extract(metadata_json, '$.SeriesId') IS NOT NULL;

COMMIT;
