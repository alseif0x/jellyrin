-- Safe, immutable metadata accessors used by the PostgreSQL catalog hot path.
-- They preserve Jellyfin's key precedence while accepting imported key casing.
CREATE OR REPLACE FUNCTION public.jellyrin_metadata_value(metadata jsonb, keys text[])
RETURNS jsonb
LANGUAGE plpgsql
IMMUTABLE
PARALLEL SAFE
AS $$
DECLARE
    requested_key text;
    matched_value jsonb;
BEGIN
    FOREACH requested_key IN ARRAY keys LOOP
        SELECT entry.value
          INTO matched_value
          FROM jsonb_each(metadata) AS entry(key, value)
         WHERE lower(entry.key) = lower(requested_key)
         ORDER BY entry.key
         LIMIT 1;
        IF FOUND THEN
            RETURN matched_value;
        END IF;
    END LOOP;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION public.jellyrin_metadata_number(metadata jsonb, keys text[])
RETURNS double precision
LANGUAGE plpgsql
IMMUTABLE
-- The exception handler uses a PostgreSQL subtransaction. Marking this function
-- parallel-safe lets a parallel CREATE INDEX invoke it in a worker, where
-- subtransactions are forbidden.
PARALLEL UNSAFE
AS $$
DECLARE
    value jsonb;
    raw text;
BEGIN
    value := public.jellyrin_metadata_value(metadata, keys);
    IF value IS NULL OR jsonb_typeof(value) NOT IN ('number', 'string') THEN
        RETURN NULL;
    END IF;
    raw := CASE WHEN jsonb_typeof(value) = 'string' THEN value #>> '{}' ELSE value::text END;
    RETURN raw::double precision;
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION public.jellyrin_metadata_timestamp(metadata jsonb, keys text[])
RETURNS timestamptz
LANGUAGE plpgsql
IMMUTABLE
-- See jellyrin_metadata_number: this function also has an exception handler.
PARALLEL UNSAFE
AS $$
DECLARE
    value jsonb;
    raw text;
BEGIN
    value := public.jellyrin_metadata_value(metadata, keys);
    IF value IS NULL OR jsonb_typeof(value) <> 'string' THEN
        RETURN NULL;
    END IF;
    raw := btrim(value #>> '{}');
    IF raw ~ '^\d{4}-\d{2}-\d{2}$' THEN
        raw := raw || 'T00:00:00Z';
    ELSIF raw !~ '^\d{4}-\d{2}-\d{2}T' THEN
        RETURN NULL;
    END IF;
    RETURN raw::timestamptz;
EXCEPTION WHEN datetime_field_overflow OR invalid_datetime_format THEN
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION public.jellyrin_metadata_has_text(metadata jsonb, keys text[])
RETURNS boolean
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
RETURN COALESCE(
    jsonb_typeof(public.jellyrin_metadata_value(metadata, keys)) = 'string'
    AND btrim(public.jellyrin_metadata_value(metadata, keys) #>> '{}') <> '',
    false
);

CREATE OR REPLACE FUNCTION public.jellyrin_metadata_boolean(metadata jsonb, keys text[])
RETURNS boolean
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
RETURN COALESCE(
    CASE
        WHEN jsonb_typeof(public.jellyrin_metadata_value(metadata, keys)) = 'boolean'
        THEN (public.jellyrin_metadata_value(metadata, keys) #>> '{}')::boolean
        ELSE false
    END,
    false
);

CREATE OR REPLACE FUNCTION public.jellyrin_metadata_has_provider_id(metadata jsonb, provider_key text)
RETURNS boolean
LANGUAGE plpgsql
IMMUTABLE
PARALLEL SAFE
AS $$
DECLARE
    parent_key text;
    provider_map jsonb;
    provider_value jsonb;
BEGIN
    FOREACH parent_key IN ARRAY ARRAY['ProviderIds', 'SeriesProviderIds'] LOOP
        provider_map := public.jellyrin_metadata_value(metadata, ARRAY[parent_key]);
        IF jsonb_typeof(provider_map) <> 'object' THEN
            CONTINUE;
        END IF;
        provider_value := public.jellyrin_metadata_value(provider_map, ARRAY[provider_key]);
        IF jsonb_typeof(provider_value) = 'string'
           AND btrim(provider_value #>> '{}') <> '' THEN
            RETURN true;
        END IF;
    END LOOP;
    RETURN false;
END;
$$;

-- The previous lookup started with virtual_folder_id, which is ideal for filter menus but not
-- for a global /Items filter. This companion index keeps item filtering index-driven.
CREATE INDEX IF NOT EXISTS idx_media_item_query_filter_values_catalog_lookup
    ON media_item_query_filter_values (value_kind, lower(btrim(display_value)), item_id);

CREATE INDEX IF NOT EXISTS idx_media_items_visible_community_rating
    ON media_items (public.jellyrin_metadata_number(metadata, ARRAY['CommunityRating', 'Rating']))
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_critic_rating
    ON media_items (public.jellyrin_metadata_number(metadata, ARRAY['CriticRating']))
    WHERE missing_since IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_visible_premiere_date
    ON media_items (public.jellyrin_metadata_timestamp(metadata, ARRAY['PremiereDate', 'AirDate', 'DateCreated']))
    WHERE missing_since IS NULL;
