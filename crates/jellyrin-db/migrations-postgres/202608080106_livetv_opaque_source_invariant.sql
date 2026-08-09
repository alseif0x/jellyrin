-- A channel has exactly one durable source representation:
--   * legacy rows keep stream_url and have no ProviderReference;
--   * opaque provider rows keep ProviderReference and have an empty stream_url.
-- The earlier source-or-reference constraint already rejects rows with neither.
-- Refuse mixed historical rows before installing the complementary constraint;
-- never print or modify their credential-bearing source values.
DO $migration$
DECLARE
    mixed_row_count bigint;
BEGIN
    SELECT count(*)
    INTO mixed_row_count
    FROM live_tv_channels
    WHERE NULLIF(btrim(stream_url), '') IS NOT NULL
      AND metadata ? 'ProviderReference'
      AND NULLIF(btrim(metadata ->> 'ProviderReference'), '') IS NOT NULL;

    IF mixed_row_count <> 0 THEN
        RAISE EXCEPTION USING
            ERRCODE = '23514',
            MESSAGE = format(
                'live_tv_channels contains %s mixed source row(s)',
                mixed_row_count
            ),
            HINT = 'Re-import affected provider catalogues so each channel has either stream_url or ProviderReference, then retry the migration.';
    END IF;
END;
$migration$;

ALTER TABLE live_tv_channels
    ADD CONSTRAINT live_tv_channels_opaque_reference_excludes_stream_url
    CHECK (
        NOT (
            NULLIF(btrim(stream_url), '') IS NOT NULL
            AND metadata ? 'ProviderReference'
            AND NULLIF(btrim(metadata ->> 'ProviderReference'), '') IS NOT NULL
        )
    ) NOT VALID;

ALTER TABLE live_tv_channels
    VALIDATE CONSTRAINT live_tv_channels_opaque_reference_excludes_stream_url;
