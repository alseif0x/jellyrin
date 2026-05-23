CREATE TABLE IF NOT EXISTS task_runs (
    id TEXT PRIMARY KEY,
    task_key TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    started_at TEXT NOT NULL,
    completed_at TEXT,
    result_json TEXT,
    error_message TEXT,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_task_runs_one_running
ON task_runs(task_key)
WHERE status = 'running';

CREATE INDEX IF NOT EXISTS idx_task_runs_task_latest
ON task_runs(task_key, completed_at DESC);
