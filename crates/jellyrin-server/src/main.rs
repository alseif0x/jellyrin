use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;
use clap::Parser;
use jellyrin_api::{
    AppState, SystemLifecycleCommand, cleanup_stale_hls_transcodes, last_system_lifecycle_command,
    publish_system_lifecycle_command, reconcile_live_tv_recordings_on_startup,
    reconcile_transcode_sessions_on_startup, router, spawn_dlna_ssdp_service,
    spawn_periodic_live_tv_timer_scheduler, spawn_periodic_transcode_cleanup,
    spawn_periodic_xtream_media_sync_scheduler, subscribe_system_lifecycle_commands,
};
use jellyrin_db::Database;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "jellyrin", version, about = "Jellyrin media server")]
struct Args {
    #[arg(long, default_value = "0.0.0.0", env = "JELLYRIN_HOST")]
    host: String,

    #[arg(long, default_value_t = 8096, env = "JELLYRIN_PORT")]
    port: u16,

    #[arg(long, default_value = "./data", env = "JELLYRIN_DATA_DIR")]
    data_dir: PathBuf,

    #[arg(long, default_value = "./config", env = "JELLYRIN_CONFIG_DIR")]
    config_dir: PathBuf,

    #[arg(long, default_value = "./cache", env = "JELLYRIN_CACHE_DIR")]
    cache_dir: PathBuf,

    #[arg(long, default_value = "./logs", env = "JELLYRIN_LOG_DIR")]
    log_dir: PathBuf,

    #[arg(
        long,
        default_value = "/home/cdmonio/dev/jellyfin-web/dist",
        env = "JELLYRIN_WEB_DIR"
    )]
    web_dir: PathBuf,

    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,

    #[arg(long, env = "JELLYRIN_E2E_ADMIN_USER")]
    e2e_admin_user: Option<String>,

    #[arg(long, env = "JELLYRIN_E2E_ADMIN_PASSWORD")]
    e2e_admin_password: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse();
    prepare_dirs(&args).await?;

    let database_url = args.database_url.clone().unwrap_or_else(|| {
        format!(
            "sqlite://{}?mode=rwc",
            args.data_dir.join("jellyrin.db").to_string_lossy()
        )
    });

    let db = Database::connect(&database_url).await?;
    bootstrap_e2e_admin(&db, &args).await?;
    let stopped_transcodes = reconcile_transcode_sessions_on_startup(&db)
        .await
        .context("failed to reconcile transcode sessions")?;
    if stopped_transcodes > 0 {
        tracing::warn!(
            count = stopped_transcodes,
            "stopped stale transcode sessions from previous run"
        );
    }
    let cleaned_transcode_outputs = cleanup_stale_hls_transcodes(&db)
        .await
        .context("failed to clean stale transcode outputs")?;
    if cleaned_transcode_outputs > 0 {
        tracing::info!(
            count = cleaned_transcode_outputs,
            "cleaned stale transcode outputs on startup"
        );
    }
    let address: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .context("invalid bind address")?;
    let local_address = format!("http://{address}");

    let state = AppState {
        db: db.clone(),
        web_dir: args.web_dir,
        log_dir: args.log_dir,
        local_address,
    };
    let live_tv_recovery = reconcile_live_tv_recordings_on_startup(&state.db, &state.log_dir)
        .await
        .context("failed to reconcile Live TV recordings")?;
    if live_tv_recovery.removed_stale_recordings > 0
        || live_tv_recovery.removed_expired_timers > 0
        || live_tv_recovery.restarted_recordings > 0
    {
        tracing::warn!(
            removed_stale_recordings = live_tv_recovery.removed_stale_recordings,
            removed_expired_timers = live_tv_recovery.removed_expired_timers,
            restarted_recordings = live_tv_recovery.restarted_recordings,
            "reconciled Live TV recording state from previous run"
        );
    }
    let _transcode_cleanup_task = spawn_periodic_transcode_cleanup(db);
    let _live_tv_timer_scheduler_task = spawn_periodic_live_tv_timer_scheduler(state.clone());
    let _xtream_media_sync_scheduler_task =
        spawn_periodic_xtream_media_sync_scheduler(state.clone());

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    let _dlna_ssdp_task = spawn_dlna_ssdp_service(state.clone());

    tracing::info!(%address, "jellyrin listening");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server failed")?;
    if last_system_lifecycle_command() == Some(SystemLifecycleCommand::Restart) {
        anyhow::bail!("restart requested");
    }

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jellyrin=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

async fn bootstrap_e2e_admin(db: &Database, args: &Args) -> anyhow::Result<()> {
    match (&args.e2e_admin_user, &args.e2e_admin_password) {
        (Some(user), Some(password)) => {
            db.upsert_admin_user(user, password)
                .await
                .context("failed to bootstrap E2E admin user")?;
            tracing::warn!(user = %user, "bootstrapped E2E admin user from environment");
        }
        (None, None) => {}
        _ => {
            tracing::warn!(
                "ignoring incomplete E2E admin bootstrap environment; both user and password are required"
            );
        }
    }

    Ok(())
}

async fn prepare_dirs(args: &Args) -> anyhow::Result<()> {
    for path in [
        &args.data_dir,
        &args.config_dir,
        &args.cache_dir,
        &args.log_dir,
    ] {
        tokio::fs::create_dir_all(path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

async fn shutdown_signal() {
    let mut lifecycle = subscribe_system_lifecycle_commands();
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            publish_system_lifecycle_command(SystemLifecycleCommand::Shutdown);
        },
        _ = terminate => {
            publish_system_lifecycle_command(SystemLifecycleCommand::Shutdown);
        },
        command = lifecycle.recv() => {
            if let Ok(command) = command {
                tracing::warn!(?command, "received system lifecycle command");
            }
        },
    }
}
