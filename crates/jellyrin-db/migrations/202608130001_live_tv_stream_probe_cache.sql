-- Derived live stream metadata. Never store a source URL, credential, provider reference,
-- ffprobe payload or stderr in this table. The tuner FK gives deterministic lifecycle cleanup;
-- channel membership is checked by repository writes so probes survive snapshot row replacement.
CREATE TABLE live_tv_channel_stream_probes (
    channel_id TEXT NOT NULL,
    tuner_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    probe_version INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    streams_json TEXT NOT NULL DEFAULT '[]',
    observed_at TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (channel_id, source_revision, probe_version),
    FOREIGN KEY(tuner_id) REFERENCES live_tv_tuners(tuner_id) ON DELETE CASCADE,
    CHECK (length(channel_id) BETWEEN 1 AND 512),
    CHECK (length(tuner_id) BETWEEN 1 AND 512),
    CHECK (length(remote_id) BETWEEN 1 AND 512),
    CHECK (length(source_revision) BETWEEN 16 AND 128),
    CHECK (probe_version > 0),
    CHECK (outcome IN ('tracks', 'empty', 'failed', 'unsupported')),
    CHECK (json_valid(streams_json) AND json_type(streams_json) = 'array'),
    CHECK (completed_at >= observed_at),
    CHECK (expires_at > completed_at)
);

CREATE INDEX idx_live_tv_channel_stream_probes_expiry
    ON live_tv_channel_stream_probes(expires_at);
CREATE INDEX idx_live_tv_channel_stream_probes_tuner_remote
    ON live_tv_channel_stream_probes(tuner_id, remote_id);
