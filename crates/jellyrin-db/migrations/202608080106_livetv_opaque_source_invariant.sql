-- SQLite cannot add a CHECK constraint to an existing table without rebuilding
-- it. Preflight the exact XOR invariant without changing data, then enforce it
-- for future writes with equivalent INSERT and UPDATE triggers.
CREATE TEMP TABLE jellyrin_livetv_opaque_source_preflight (
    invalid_row_count INTEGER NOT NULL,
    CONSTRAINT jellyrin_livetv_opaque_source_preflight_zero
        CHECK (invalid_row_count = 0)
);

INSERT INTO jellyrin_livetv_opaque_source_preflight (invalid_row_count)
SELECT count(*)
FROM live_tv_channels
WHERE
    (NULLIF(trim(stream_url), '') IS NOT NULL)
    =
    (
        CASE
            WHEN json_valid(metadata_json) THEN
                COALESCE(
                    json_type(metadata_json, '$.ProviderReference') = 'text'
                    AND NULLIF(
                        trim(json_extract(metadata_json, '$.ProviderReference')),
                        ''
                    ) IS NOT NULL,
                    0
                )
            ELSE 0
        END
    );

DROP TABLE jellyrin_livetv_opaque_source_preflight;

CREATE TRIGGER live_tv_channels_opaque_source_insert
BEFORE INSERT ON live_tv_channels
WHEN
    (NULLIF(trim(NEW.stream_url), '') IS NOT NULL)
    =
    (
        CASE
            WHEN json_valid(NEW.metadata_json) THEN
                COALESCE(
                    json_type(NEW.metadata_json, '$.ProviderReference') = 'text'
                    AND NULLIF(
                        trim(json_extract(NEW.metadata_json, '$.ProviderReference')),
                        ''
                    ) IS NOT NULL,
                    0
                )
            ELSE 0
        END
    )
BEGIN
    SELECT RAISE(
        ABORT,
        'live_tv channel must persist exactly one of stream_url or ProviderReference'
    );
END;

CREATE TRIGGER live_tv_channels_opaque_source_update
BEFORE UPDATE OF stream_url, metadata_json ON live_tv_channels
WHEN
    (NULLIF(trim(NEW.stream_url), '') IS NOT NULL)
    =
    (
        CASE
            WHEN json_valid(NEW.metadata_json) THEN
                COALESCE(
                    json_type(NEW.metadata_json, '$.ProviderReference') = 'text'
                    AND NULLIF(
                        trim(json_extract(NEW.metadata_json, '$.ProviderReference')),
                        ''
                    ) IS NOT NULL,
                    0
                )
            ELSE 0
        END
    )
BEGIN
    SELECT RAISE(
        ABORT,
        'live_tv channel must persist exactly one of stream_url or ProviderReference'
    );
END;
