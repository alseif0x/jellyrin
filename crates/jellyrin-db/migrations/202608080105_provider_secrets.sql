CREATE TABLE IF NOT EXISTS provider_secrets (
    secret_id TEXT PRIMARY KEY,
    provider_type TEXT NOT NULL,
    envelope_version INTEGER NOT NULL,
    key_id TEXT NOT NULL,
    nonce BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (length(secret_id) BETWEEN 1 AND 512),
    CHECK (length(provider_type) BETWEEN 1 AND 128),
    CHECK (envelope_version > 0),
    CHECK (length(key_id) BETWEEN 1 AND 128),
    CHECK (length(nonce) = 12),
    CHECK (length(ciphertext) >= 16),
    CHECK (revision > 0)
);

CREATE INDEX IF NOT EXISTS idx_provider_secrets_provider_updated
    ON provider_secrets(provider_type COLLATE NOCASE, updated_at);
