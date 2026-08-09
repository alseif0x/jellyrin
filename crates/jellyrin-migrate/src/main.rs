use std::path::PathBuf;

use clap::{Args as ClapArgs, Parser, Subcommand};
use jellyrin_migrate::{
    FailureReport, MigrationOptions, ProviderUrlAuditOptions, RuntimeHygieneAuditOptions,
    apply_schema, audit_provider_url_retention, audit_runtime_hygiene, execute,
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "jellyrin-migrate",
    version,
    about = "One-shot PostgreSQL schema and SQLite data migration for Jellyrin"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Apply the embedded PostgreSQL migrations with a DDL-capable role and exit.
    #[command(alias = "migrate-schema")]
    Schema(SchemaArgs),

    /// Migrate durable SQLite data into an already-migrated PostgreSQL schema.
    #[command(alias = "migrate", alias = "migrate-data")]
    Data(DataArgs),

    /// Count credential-bearing legacy provider locations without printing their contents.
    #[command(alias = "audit-provider-urls")]
    AuditSourceHygiene(AuditSourceHygieneArgs),

    /// Scan logs and process argv snapshots without printing credential-bearing contents.
    AuditRuntimeHygiene(AuditRuntimeHygieneArgs),
}

#[derive(Debug, ClapArgs)]
struct SchemaArgs {
    /// PostgreSQL connection URL. Prefer DATABASE_URL to avoid shell history.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, value_name = "URL")]
    target: String,

    /// Also write the secret-free JSON report to this path.
    #[arg(long, value_name = "JSON_PATH")]
    report: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
struct DataArgs {
    /// Consistent SQLite snapshot. Jellyrin must not be writing to it.
    #[arg(long, value_name = "SQLITE_DB")]
    source: PathBuf,

    /// PostgreSQL connection URL. Prefer DATABASE_URL to avoid shell history.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, value_name = "URL")]
    target: String,

    /// Execute every conversion and constraint check, then roll back PostgreSQL.
    #[arg(long)]
    dry_run: bool,

    /// Also write the secret-free JSON report to this path.
    #[arg(long, value_name = "JSON_PATH")]
    report: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
struct AuditSourceHygieneArgs {
    /// Optional read-only SQLite snapshot to audit before cutover.
    #[arg(long, value_name = "SQLITE_DB")]
    source: Option<PathBuf>,

    /// PostgreSQL connection URL. Prefer DATABASE_URL to avoid shell history.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, value_name = "URL")]
    target: String,

    /// Also write the counts-only JSON report to this path.
    #[arg(long, value_name = "JSON_PATH")]
    report: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
struct AuditRuntimeHygieneArgs {
    /// Regular log file to scan. Repeat for every required source; symlinks are rejected.
    #[arg(long = "log", value_name = "PATH")]
    logs: Vec<PathBuf>,

    /// Regular NUL-delimited /proc/<pid>/cmdline snapshot. Repeat for each process.
    #[arg(long = "argv", value_name = "PATH")]
    argv: Vec<PathBuf>,

    /// Exact Jellyrin loopback relay port. Required whenever --argv is supplied.
    #[arg(long, value_name = "PORT")]
    relay_port: Option<u16>,

    /// Also write the counts-only JSON report to this path.
    #[arg(long, value_name = "JSON_PATH")]
    report: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let result = match args.command {
        Command::Schema(args) => match apply_schema(&args.target).await {
            Ok(report) => emit_success(&report, args.report).await,
            Err(error) => Err(redact_error(&error.to_string())),
        },
        Command::Data(args) => match execute(MigrationOptions {
            source: args.source,
            target_url: args.target,
            dry_run: args.dry_run,
        })
        .await
        {
            Ok(report) => emit_success(&report, args.report).await,
            Err(error) => Err(redact_error(&error.to_string())),
        },
        Command::AuditSourceHygiene(args) => {
            match audit_provider_url_retention(ProviderUrlAuditOptions {
                source: args.source,
                target_url: args.target,
            })
            .await
            {
                Ok(report) => {
                    let exit_code = provider_url_audit_exit_code(&report);
                    match emit_success(&report, args.report).await {
                        Ok(()) if exit_code == 0 => Ok(()),
                        Ok(()) => std::process::exit(exit_code),
                        Err(error) => {
                            emit_failure(&redact_error(&error));
                            std::process::exit(3);
                        }
                    }
                }
                Err(error) => {
                    emit_failure(&redact_error(&error.to_string()));
                    std::process::exit(3);
                }
            }
        }
        Command::AuditRuntimeHygiene(args) => {
            match audit_runtime_hygiene(RuntimeHygieneAuditOptions {
                log_files: args.logs,
                argv_files: args.argv,
                relay_port: args.relay_port,
            }) {
                Ok(report) => {
                    let exit_code = report.exit_code();
                    match emit_success(&report, args.report).await {
                        Ok(()) if exit_code == 0 => Ok(()),
                        Ok(()) => std::process::exit(exit_code),
                        Err(error) => {
                            emit_failure(&redact_error(&error));
                            std::process::exit(3);
                        }
                    }
                }
                Err(error) => {
                    emit_failure(&redact_error(&error.to_string()));
                    std::process::exit(3);
                }
            }
        }
    };

    if let Err(error) = result {
        emit_failure(&error);
        std::process::exit(1);
    }
}

fn provider_url_audit_exit_code(report: &jellyrin_migrate::ProviderUrlRetentionReport) -> i32 {
    if report.is_clean() { 0 } else { 2 }
}

async fn emit_success<T: Serialize>(report: &T, path: Option<PathBuf>) -> Result<(), String> {
    let encoded = serde_json::to_vec_pretty(report).map_err(|error| error.to_string())?;
    if let Some(path) = path {
        tokio::fs::write(path, &encoded)
            .await
            .map_err(|error| format!("failed to write JSON report: {error}"))?;
    }
    println!("{}", String::from_utf8_lossy(&encoded));
    Ok(())
}

fn emit_failure(error: &str) {
    let failure = FailureReport {
        report_version: 1,
        tool_version: env!("CARGO_PKG_VERSION"),
        status: "failed",
        error,
    };
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&failure).expect("failure report serialization failed")
    );
}

fn redact_error(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            if word.contains("://")
                || word.to_ascii_lowercase().contains("password=")
                || word.to_ascii_lowercase().contains("access_token=")
            {
                "[REDACTED]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_data_and_schema_subcommands() {
        let data = Args::try_parse_from([
            "jellyrin-migrate",
            "data",
            "--source",
            "/tmp/source.db",
            "--target",
            "postgresql://localhost/jellyrin",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            data.command,
            Command::Data(DataArgs { dry_run: true, .. })
        ));

        let runtime_audit = Args::try_parse_from([
            "jellyrin-migrate",
            "audit-runtime-hygiene",
            "--log",
            "/var/log/jellyrin/server.log",
            "--argv",
            "/proc/123/cmdline",
            "--relay-port",
            "8096",
        ])
        .unwrap();
        assert!(matches!(
            runtime_audit.command,
            Command::AuditRuntimeHygiene(AuditRuntimeHygieneArgs { logs, argv, .. })
                if logs.len() == 1 && argv.len() == 1
        ));

        let schema = Args::try_parse_from([
            "jellyrin-migrate",
            "schema",
            "--target",
            "postgresql://localhost/jellyrin",
        ])
        .unwrap();
        assert!(matches!(schema.command, Command::Schema(_)));

        let audit = Args::try_parse_from([
            "jellyrin-migrate",
            "audit-source-hygiene",
            "--target",
            "postgresql://localhost/jellyrin",
            "--source",
            "/tmp/source.db",
        ])
        .unwrap();
        assert!(matches!(
            audit.command,
            Command::AuditSourceHygiene(AuditSourceHygieneArgs {
                source: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn legacy_data_alias_is_accepted() {
        let parsed = Args::try_parse_from([
            "jellyrin-migrate",
            "migrate",
            "--source",
            "/tmp/source.db",
            "--target",
            "postgresql://localhost/jellyrin",
        ])
        .unwrap();
        assert!(matches!(parsed.command, Command::Data(_)));
    }

    #[test]
    fn failure_redaction_removes_urls_and_named_secrets() {
        let redacted = redact_error(
            "connect postgresql://user:secret@db/name password=hunter2 access_token=abc",
        );
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("abc"));
        assert_eq!(redacted, "connect [REDACTED] [REDACTED] [REDACTED]");
    }

    #[test]
    fn provider_url_audit_uses_findings_exit_code_two() {
        use jellyrin_migrate::{ProviderUrlRetentionCounts, ProviderUrlRetentionReport};

        let report = |remote_source_url_rows| ProviderUrlRetentionReport {
            report_version: 1,
            tool_version: "test",
            status: "test",
            postgres: ProviderUrlRetentionCounts {
                remote_source_url_rows,
                remote_probe_source_url_rows: 0,
                invalid_remote_probe_rows: 0,
                live_tv_stream_url_rows: 0,
            },
            sqlite: None,
            started_at: "start".to_string(),
            finished_at: "finish".to_string(),
            duration_ms: 0,
        };
        assert_eq!(provider_url_audit_exit_code(&report(0)), 0);
        assert_eq!(provider_url_audit_exit_code(&report(1)), 2);
    }
}
