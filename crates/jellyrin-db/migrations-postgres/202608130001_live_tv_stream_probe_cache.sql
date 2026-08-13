-- Bounded, derived metadata discovered from short probes of live channel streams.
-- Source URLs, provider references, credentials, ffprobe output and stderr never belong here.
-- There is intentionally no foreign key to live_tv_channels: provider snapshot publication
-- replaces channel rows in one transaction and useful probe results must survive that swap.
CREATE TABLE live_tv_channel_stream_probes (
    channel_id text NOT NULL,
    tuner_id text NOT NULL REFERENCES live_tv_tuners(tuner_id) ON DELETE CASCADE,
    remote_id text NOT NULL,
    source_revision text NOT NULL,
    probe_version smallint NOT NULL,
    outcome text NOT NULL,
    streams jsonb NOT NULL DEFAULT '[]'::jsonb,
    observed_at timestamptz NOT NULL,
    completed_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (channel_id, source_revision, probe_version),
    CONSTRAINT live_tv_stream_probes_channel_id_length
        CHECK (length(channel_id) BETWEEN 1 AND 512),
    CONSTRAINT live_tv_stream_probes_tuner_id_length
        CHECK (length(tuner_id) BETWEEN 1 AND 512),
    CONSTRAINT live_tv_stream_probes_remote_id_length
        CHECK (length(remote_id) BETWEEN 1 AND 512),
    CONSTRAINT live_tv_stream_probes_source_revision_length
        CHECK (length(source_revision) BETWEEN 16 AND 128),
    CONSTRAINT live_tv_stream_probes_version_positive CHECK (probe_version > 0),
    CONSTRAINT live_tv_stream_probes_outcome_known
        CHECK (outcome IN ('tracks', 'empty', 'failed', 'unsupported')),
    CONSTRAINT live_tv_stream_probes_streams_array CHECK (jsonb_typeof(streams) = 'array'),
    CONSTRAINT live_tv_stream_probes_completed_after_observed
        CHECK (completed_at >= observed_at),
    CONSTRAINT live_tv_stream_probes_expires_after_completed
        CHECK (expires_at > completed_at)
);

CREATE INDEX live_tv_channel_stream_probes_expiry_idx
    ON live_tv_channel_stream_probes (expires_at);
CREATE INDEX live_tv_channel_stream_probes_tuner_remote_idx
    ON live_tv_channel_stream_probes (tuner_id, remote_id);

DO $migration$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'jellyrin_runtime'
    ) THEN
        REVOKE ALL PRIVILEGES ON TABLE live_tv_channel_stream_probes FROM jellyrin_runtime;
        GRANT SELECT, INSERT, UPDATE, DELETE
            ON TABLE live_tv_channel_stream_probes TO jellyrin_runtime;
    END IF;
END;
$migration$;
