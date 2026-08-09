-- External provider channels may intentionally omit a durable stream URL. In that case the
-- catalog stores only a signed opaque ProviderReference and resolves the ephemeral source JIT.
ALTER TABLE live_tv_channels
    ADD CONSTRAINT live_tv_channels_source_or_provider_reference
    CHECK (
        NULLIF(btrim(stream_url), '') IS NOT NULL
        OR (
            metadata ? 'ProviderReference'
            AND NULLIF(btrim(metadata ->> 'ProviderReference'), '') IS NOT NULL
        )
    );
