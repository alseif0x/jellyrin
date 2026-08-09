use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub report_version: u32,
    pub tool_version: &'static str,
    pub status: &'static str,
    pub dry_run: bool,
    pub source_schema_version: i64,
    pub target_schema_version: i64,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u128,
    pub validation: ValidationReport,
    pub tables: Vec<TableReport>,
    pub omitted: Vec<OmittedTableReport>,
    pub overall_digest_sha256: String,
}

#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub sqlite_integrity_check: &'static str,
    pub sqlite_foreign_key_violations: u64,
    pub postgres_server_version_num: i64,
    pub target_required_tables_checked: usize,
    pub durable_item_references_checked: usize,
    pub durable_item_references_missing: usize,
    pub provider_secret_references_checked: usize,
    pub provider_secret_references_missing: usize,
    pub target_was_empty_for_application_tables: bool,
    pub transaction_outcome: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TableReport {
    pub table: &'static str,
    pub source_rows: u64,
    pub migrated_rows: u64,
    pub target_rows_in_transaction: u64,
    pub source_normalized_digest_sha256: String,
    pub target_normalized_digest_sha256: String,
    pub validation: &'static str,
}

#[derive(Debug, Serialize)]
pub struct OmittedTableReport {
    pub table: &'static str,
    pub source_rows: u64,
    pub strategy: &'static str,
    pub reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FailureReport<'a> {
    pub report_version: u32,
    pub tool_version: &'static str,
    pub status: &'static str,
    pub error: &'a str,
}

#[derive(Debug, Serialize)]
pub struct SchemaReport {
    pub report_version: u32,
    pub tool_version: &'static str,
    pub status: &'static str,
    pub postgres_server_version_num: i64,
    pub schema_version_before: Option<i64>,
    pub schema_version_after: i64,
    pub embedded_migrations: usize,
    pub applied_migrations: u64,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u128,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ProviderUrlRetentionCounts {
    pub remote_source_url_rows: u64,
    pub remote_probe_source_url_rows: u64,
    pub invalid_remote_probe_rows: u64,
    pub live_tv_stream_url_rows: u64,
}

impl ProviderUrlRetentionCounts {
    pub fn is_clean(&self) -> bool {
        self.remote_source_url_rows == 0
            && self.remote_probe_source_url_rows == 0
            && self.invalid_remote_probe_rows == 0
            && self.live_tv_stream_url_rows == 0
    }
}

#[derive(Debug, Serialize)]
pub struct ProviderUrlRetentionReport {
    pub report_version: u32,
    pub tool_version: &'static str,
    pub status: &'static str,
    pub postgres: ProviderUrlRetentionCounts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqlite: Option<ProviderUrlRetentionCounts>,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u128,
}

impl ProviderUrlRetentionReport {
    pub fn is_clean(&self) -> bool {
        self.postgres.is_clean()
            && self
                .sqlite
                .as_ref()
                .is_none_or(ProviderUrlRetentionCounts::is_clean)
    }
}
