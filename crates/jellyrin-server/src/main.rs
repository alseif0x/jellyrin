use std::{net::SocketAddr, path::PathBuf};

use anyhow::Context;
use clap::Parser;
use jellyrin_api::{AppState, router};
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse();
    prepare_dirs(&args).await?;

    let database_url = args.database_url.unwrap_or_else(|| {
        format!(
            "sqlite://{}?mode=rwc",
            args.data_dir.join("jellyrin.db").to_string_lossy()
        )
    });

    let db = Database::connect(&database_url).await?;
    let address: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .context("invalid bind address")?;
    let local_address = format!("http://{address}");

    let state = AppState {
        db,
        web_dir: args.web_dir,
        local_address,
    };

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;

    tracing::info!(%address, "jellyrin listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed")?;

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
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
