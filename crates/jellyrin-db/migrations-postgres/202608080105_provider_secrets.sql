CREATE TABLE IF NOT EXISTS provider_secrets (
    secret_id text PRIMARY KEY,
    provider_type text NOT NULL,
    envelope_version smallint NOT NULL,
    key_id text NOT NULL,
    nonce bytea NOT NULL,
    ciphertext bytea NOT NULL,
    revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT provider_secrets_id_length CHECK (length(secret_id) BETWEEN 1 AND 512),
    CONSTRAINT provider_secrets_provider_length CHECK (length(provider_type) BETWEEN 1 AND 128),
    CONSTRAINT provider_secrets_envelope_version_positive CHECK (envelope_version > 0),
    CONSTRAINT provider_secrets_key_id_length CHECK (length(key_id) BETWEEN 1 AND 128),
    CONSTRAINT provider_secrets_nonce_length CHECK (octet_length(nonce) = 12),
    CONSTRAINT provider_secrets_ciphertext_length CHECK (octet_length(ciphertext) >= 16),
    CONSTRAINT provider_secrets_revision_positive CHECK (revision > 0)
);

CREATE INDEX IF NOT EXISTS provider_secrets_provider_updated_idx
    ON provider_secrets(lower(provider_type), updated_at DESC);

REVOKE ALL ON TABLE provider_secrets FROM PUBLIC;
