use std::{
    ffi::OsStr,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use clap::Parser;
use jellyrin_api::{
    AppState, SystemLifecycleCommand, cleanup_stale_hls_transcodes, configure_api_cache_root,
    configure_plugin_packages_root, ensure_builtin_xtream_plugin, initialize_transcode_config,
    last_system_lifecycle_command, publish_system_lifecycle_command,
    reconcile_live_tv_recordings_on_startup, reconcile_transcode_sessions_on_startup, router,
    shutdown_runtime_resources, spawn_dlna_ssdp_service, spawn_file_watcher_with_consumer,
    spawn_periodic_live_tv_timer_scheduler, spawn_periodic_transcode_cleanup,
    spawn_periodic_xtream_media_sync_scheduler, subscribe_system_lifecycle_commands,
    validate_ffmpeg_runtime,
};
use jellyrin_db::{Database, DatabaseConfig, DatabaseDriver, DatabaseManager, ProviderSecretVault};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use zeroize::{Zeroize, Zeroizing};

const SERVER_GRACEFUL_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_SECRET_FILE_MAX_BYTES: u64 = 128 * 1024;
const CONTAINER_HEALTHCHECK_ARGUMENT: &str = "--healthcheck";
const CONTAINER_HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(3);
const HTTP_OK_STATUS_PREFIX: &[u8; 12] = b"HTTP/1.1 200";

#[derive(Parser)]
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

    #[arg(long, default_value = "./web", env = "JELLYRIN_WEB_DIR")]
    web_dir: PathBuf,

    #[arg(long, env = "DATABASE_URL", hide_env_values = true, value_name = "URL")]
    database_url: String,

    #[arg(
        long,
        default_value = "postgresql",
        env = "JELLYRIN_DB_DRIVER",
        value_name = "DRIVER"
    )]
    database_driver: DatabaseDriver,

    #[arg(long, default_value_t = 6, env = "JELLYRIN_DB_MAX_CONNECTIONS")]
    database_max_connections: u32,

    #[arg(long, default_value_t = 2, env = "JELLYRIN_DB_WORKER_MAX_CONNECTIONS")]
    database_worker_max_connections: u32,

    #[arg(long, default_value_t = 5, env = "JELLYRIN_DB_ACQUIRE_TIMEOUT_SECONDS")]
    database_acquire_timeout_seconds: u64,

    #[arg(long, default_value_t = 600, env = "JELLYRIN_DB_IDLE_TIMEOUT_SECONDS")]
    database_idle_timeout_seconds: u64,

    #[arg(long, default_value_t = 1800, env = "JELLYRIN_DB_MAX_LIFETIME_SECONDS")]
    database_max_lifetime_seconds: u64,

    #[arg(
        long,
        default_value_t = 10,
        env = "JELLYRIN_DB_API_STATEMENT_TIMEOUT_SECONDS"
    )]
    database_api_statement_timeout_seconds: u64,

    #[arg(
        long,
        default_value_t = 120,
        env = "JELLYRIN_DB_WORKER_STATEMENT_TIMEOUT_SECONDS"
    )]
    database_worker_statement_timeout_seconds: u64,

    #[arg(long, default_value_t = 3, env = "JELLYRIN_DB_LOCK_TIMEOUT_SECONDS")]
    database_lock_timeout_seconds: u64,

    #[arg(
        long,
        default_value = "primary",
        env = "JELLYRIN_PROVIDER_SECRET_KEY_ID"
    )]
    provider_secret_key_id: String,

    #[arg(long, env = "JELLYRIN_PROVIDER_SECRET_KEY", hide_env_values = true)]
    provider_secret_key: Option<String>,

    #[arg(long, env = "JELLYRIN_PROVIDER_SECRET_KEY_FILE")]
    provider_secret_key_file: Option<PathBuf>,

    #[arg(long, env = "JELLYRIN_PROVIDER_SECRET_KEYRING", hide_env_values = true)]
    provider_secret_keyring: Option<String>,

    #[arg(long, env = "JELLYRIN_PROVIDER_SECRET_KEYRING_FILE")]
    provider_secret_keyring_file: Option<PathBuf>,

    #[arg(long, env = "JELLYRIN_E2E_ADMIN_USER")]
    e2e_admin_user: Option<String>,

    #[arg(long, env = "JELLYRIN_E2E_ADMIN_PASSWORD", hide_env_values = true)]
    e2e_admin_password: Option<String>,
}

async fn load_provider_secret_vault(
    args: &mut Args,
) -> anyhow::Result<Option<ProviderSecretVault>> {
    let configured_sources = usize::from(args.provider_secret_key.is_some())
        + usize::from(args.provider_secret_key_file.is_some())
        + usize::from(args.provider_secret_keyring.is_some())
        + usize::from(args.provider_secret_keyring_file.is_some());
    anyhow::ensure!(
        configured_sources <= 1,
        "configure at most one provider secret source: key, key file, keyring, or keyring file"
    );

    if let Some(encoded_key) = args.provider_secret_key.take() {
        let mut encoded_key = Zeroizing::new(encoded_key);
        let result = ProviderSecretVault::from_base64(&args.provider_secret_key_id, &encoded_key);
        encoded_key.zeroize();
        return result.map(Some);
    }
    if let Some(path) = args.provider_secret_key_file.as_deref() {
        let encoded_key = read_protected_provider_secret_file(path).await?;
        return ProviderSecretVault::from_base64(&args.provider_secret_key_id, &encoded_key)
            .map(Some);
    }
    if let Some(keyring) = args.provider_secret_keyring.take() {
        let mut keyring = Zeroizing::new(keyring);
        let result = ProviderSecretVault::from_keyring_json(&keyring);
        keyring.zeroize();
        return result.map(Some);
    }
    if let Some(path) = args.provider_secret_keyring_file.as_deref() {
        let keyring = read_protected_provider_secret_file(path).await?;
        return ProviderSecretVault::from_keyring_json(&keyring).map(Some);
    }
    Ok(None)
}

async fn read_protected_provider_secret_file(path: &Path) -> anyhow::Result<Zeroizing<String>> {
    use tokio::io::AsyncReadExt;

    let path_metadata = tokio::fs::symlink_metadata(path)
        .await
        .context("failed to inspect provider secret path")?;
    anyhow::ensure!(
        !path_metadata.file_type().is_symlink(),
        "provider secret path must not be a symbolic link"
    );

    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .await
        .with_context(|| format!("failed to open provider secret file at {}", path.display()))?;
    let metadata = file
        .metadata()
        .await
        .context("failed to inspect provider secret file")?;
    anyhow::ensure!(
        metadata.is_file(),
        "provider secret path must be a regular file"
    );
    anyhow::ensure!(
        metadata.len() <= PROVIDER_SECRET_FILE_MAX_BYTES,
        "provider secret file exceeds the maximum supported size"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        anyhow::ensure!(
            path_metadata.dev() == metadata.dev() && path_metadata.ino() == metadata.ino(),
            "provider secret path changed while it was being opened"
        );
        let mode = metadata.permissions().mode() & 0o777;
        anyhow::ensure!(
            mode & 0o037 == 0,
            "provider secret file permissions are too broad; use mode 0400 or 0440"
        );
    }
    let mut payload = Zeroizing::new(String::new());
    file.take(PROVIDER_SECRET_FILE_MAX_BYTES + 1)
        .read_to_string(&mut payload)
        .await
        .context("failed to read provider secret file")?;
    anyhow::ensure!(
        payload.len() as u64 <= PROVIDER_SECRET_FILE_MAX_BYTES,
        "provider secret file exceeds the maximum supported size"
    );
    Ok(payload)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Keep the container probe independent of Clap's required database URL and
    // of every startup side effect. This replaces a general-purpose HTTP
    // client in the runtime image with a bounded localhost-only probe.
    if container_healthcheck_requested(std::env::args_os()) {
        return run_container_healthcheck();
    }

    init_tracing();

    let mut args = Args::parse();
    let provider_secret_vault = load_provider_secret_vault(&mut args).await?;
    prepare_dirs(&args).await?;
    configure_api_cache_root(&args.cache_dir).context("failed to configure API cache storage")?;
    configure_plugin_packages_root(&args.data_dir)
        .context("failed to configure plugin package storage")?;
    initialize_transcode_config();
    validate_ffmpeg_runtime()
        .await
        .context("required FFmpeg capabilities are unavailable")?;

    let database_config = DatabaseConfig::new(args.database_driver, &args.database_url)?
        .with_api_max_connections(args.database_max_connections)?
        .with_worker_max_connections(args.database_worker_max_connections)?
        .with_acquire_timeout(Duration::from_secs(args.database_acquire_timeout_seconds))?
        .with_idle_timeout(Duration::from_secs(args.database_idle_timeout_seconds))?
        .with_max_lifetime(Duration::from_secs(args.database_max_lifetime_seconds))?
        .with_statement_timeouts(
            Duration::from_secs(args.database_api_statement_timeout_seconds),
            Duration::from_secs(args.database_worker_statement_timeout_seconds),
        )?
        .with_lock_timeout(Duration::from_secs(args.database_lock_timeout_seconds))?;
    let database_manager = match provider_secret_vault {
        Some(vault) => DatabaseManager::new(database_config)?.with_provider_secret_vault(vault),
        None => DatabaseManager::new(database_config)?,
    };
    tracing::info!(
        database_driver = %database_manager.driver(),
        "selected database adapter"
    );
    let db = database_manager.connect().await?;
    db.schema_health()
        .await
        .context("database schema is not ready; run the migration job first")?;
    db.validate_provider_secret_readiness()
        .await
        .context("provider secret key configuration is not ready")?;
    let rotated_provider_secrets = db
        .rotate_provider_secrets_to_active_key()
        .await
        .context("failed to rotate provider secrets to the active key")?;
    if rotated_provider_secrets > 0 {
        tracing::info!(
            rotated_secrets = rotated_provider_secrets,
            "re-encrypted provider secrets with the active key"
        );
    }
    let backfilled_provider_configs = db
        .backfill_legacy_provider_secrets()
        .await
        .context("failed to backfill legacy provider credentials")?;
    if backfilled_provider_configs > 0 {
        tracing::info!(
            rewritten_configurations = backfilled_provider_configs,
            "moved legacy provider credentials into the encrypted secret store"
        );
    }
    match db.reconcile_orphaned_provider_secrets().await {
        Ok(orphaned_provider_secrets) if orphaned_provider_secrets > 0 => {
            tracing::info!(
                deleted_envelopes = orphaned_provider_secrets,
                "removed unreferenced provider secret envelopes"
            );
        }
        Ok(_) => {}
        Err(error) => {
            // Reconciliation fails closed before deletion. A malformed unrelated configuration
            // must retain envelopes for repair without preventing the server from starting.
            tracing::warn!(
                error = %error,
                "provider secret reconciliation was skipped; all candidate envelopes were retained"
            );
        }
    }
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
    if let Err(error) = ensure_builtin_xtream_plugin(&state.db).await {
        tracing::warn!(?error, "failed to register builtin xtream plugin");
    }
    let mut background_tasks = vec![
        spawn_periodic_transcode_cleanup(db),
        spawn_periodic_live_tv_timer_scheduler(state.clone()),
        spawn_periodic_xtream_media_sync_scheduler(state.clone()),
    ];
    let file_watcher =
        if let Some((watcher, consumer)) = spawn_file_watcher_with_consumer(state.clone()).await {
            background_tasks.push(consumer);
            Some(watcher)
        } else {
            None
        };

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    background_tasks.push(spawn_dlna_ssdp_service(state.clone()));
    let background_abort_handles = background_tasks
        .iter()
        .map(tokio::task::JoinHandle::abort_handle)
        .collect::<Vec<_>>();
    let (shutdown_started_tx, shutdown_started_rx) = tokio::sync::oneshot::channel();

    tracing::info!(%address, "jellyrin listening");
    let server = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = shutdown_started_tx.send(());
        // Stop producers of new background work before cancelling active
        // process-backed resources. This runs before Axum waits for long-lived
        // response bodies, allowing stream cancellation to unblock draining.
        for handle in background_abort_handles {
            handle.abort();
        }
        let report = shutdown_runtime_resources().await;
        tracing::info!(?report, "initial runtime shutdown sweep completed");
    });
    let server_result = match await_server_with_bounded_drain(
        server,
        shutdown_started_rx,
        SERVER_GRACEFUL_DRAIN_TIMEOUT,
    )
    .await
    {
        BoundedServerOutcome::Completed(result) => Some(result),
        BoundedServerOutcome::DrainTimedOut => {
            tracing::warn!(
                timeout_seconds = SERVER_GRACEFUL_DRAIN_TIMEOUT.as_secs(),
                "HTTP graceful drain timed out; closing remaining connections"
            );
            None
        }
    };

    // Dropping the watcher closes its OS resources; the consumer and all other
    // periodic tasks were aborted by the graceful-shutdown future. Abort again
    // for the server-error path, then await every task so none is detached.
    drop(file_watcher);
    abort_and_join_background_tasks(background_tasks).await;
    let final_shutdown = shutdown_runtime_resources().await;
    tracing::info!(?final_shutdown, "final runtime shutdown sweep completed");

    if let Some(server_result) = server_result {
        server_result.context("server failed")?;
    }
    if last_system_lifecycle_command() == Some(SystemLifecycleCommand::Restart) {
        anyhow::bail!("restart requested");
    }

    Ok(())
}

fn container_healthcheck_requested(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    let _executable = args.next();
    args.next().as_deref() == Some(OsStr::new(CONTAINER_HEALTHCHECK_ARGUMENT))
        && args.next().is_none()
}

fn run_container_healthcheck() -> anyhow::Result<()> {
    let port = std::env::var("JELLYRIN_PORT")
        .unwrap_or_else(|_| "8096".to_owned())
        .parse::<u16>()
        .context("JELLYRIN_PORT must be a valid TCP port")?;
    probe_health_endpoint(port)
}

fn probe_health_endpoint(port: u16) -> anyhow::Result<()> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, CONTAINER_HEALTHCHECK_TIMEOUT)
        .context("failed to connect to Jellyrin health endpoint")?;
    stream
        .set_read_timeout(Some(CONTAINER_HEALTHCHECK_TIMEOUT))
        .context("failed to set healthcheck read timeout")?;
    stream
        .set_write_timeout(Some(CONTAINER_HEALTHCHECK_TIMEOUT))
        .context("failed to set healthcheck write timeout")?;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .context("failed to request Jellyrin health endpoint")?;

    let mut status_prefix = [0_u8; HTTP_OK_STATUS_PREFIX.len()];
    stream
        .read_exact(&mut status_prefix)
        .context("failed to read Jellyrin health response")?;
    anyhow::ensure!(
        status_prefix == *HTTP_OK_STATUS_PREFIX,
        "Jellyrin health endpoint did not return HTTP 200"
    );
    Ok(())
}

enum BoundedServerOutcome<T> {
    Completed(T),
    DrainTimedOut,
}

async fn await_server_with_bounded_drain<F>(
    server: F,
    shutdown_started: tokio::sync::oneshot::Receiver<()>,
    drain_timeout: Duration,
) -> BoundedServerOutcome<<F as std::future::IntoFuture>::Output>
where
    F: std::future::IntoFuture,
{
    let mut server = Box::pin(server.into_future());
    tokio::select! {
        result = server.as_mut() => BoundedServerOutcome::Completed(result),
        _ = shutdown_started => {
            match tokio::time::timeout(drain_timeout, server.as_mut()).await {
                Ok(result) => BoundedServerOutcome::Completed(result),
                Err(_) => BoundedServerOutcome::DrainTimedOut,
            }
        }
    }
}

async fn abort_and_join_background_tasks(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        match task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => tracing::warn!(%error, "background task failed during shutdown"),
        }
    }
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

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, io::Write};

    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(notify) = self.0.take() {
                let _ = notify.send(());
            }
        }
    }

    #[test]
    fn container_healthcheck_argument_must_be_exact_and_exclusive() {
        let args = ["jellyrin-server", "--healthcheck"].map(OsString::from);
        assert!(super::container_healthcheck_requested(args.into_iter()));

        let extra = ["jellyrin-server", "--healthcheck", "--port", "9000"].map(OsString::from);
        assert!(!super::container_healthcheck_requested(extra.into_iter()));
        let normal = ["jellyrin-server", "--port", "8096"].map(OsString::from);
        assert!(!super::container_healthcheck_requested(normal.into_iter()));
    }

    #[test]
    fn container_healthcheck_accepts_only_http_200() {
        let (healthy_port, healthy_server) = serve_status_once(b"HTTP/1.1 200 OK\r\n");
        super::probe_health_endpoint(healthy_port).unwrap();
        healthy_server.join().unwrap();

        let (unhealthy_port, unhealthy_server) =
            serve_status_once(b"HTTP/1.1 503 Service Unavailable\r\n");
        let error = super::probe_health_endpoint(unhealthy_port).unwrap_err();
        unhealthy_server.join().unwrap();
        assert!(error.to_string().contains("did not return HTTP 200"));
    }

    fn serve_status_once(status_line: &'static [u8]) -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = [0_u8; 256];
            let _ = std::io::Read::read(&mut connection, &mut request).unwrap();
            connection.write_all(status_line).unwrap();
            // The probe intentionally stops after the status prefix. Its close can race this
            // trailing header write and legitimately surface ECONNRESET on the fixture socket.
            let _ = connection.write_all(b"Content-Length: 0\r\nConnection: close\r\n\r\n");
        });
        (port, server)
    }

    #[tokio::test]
    async fn shutdown_aborts_and_joins_background_tasks() {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _drop_guard = NotifyOnDrop(Some(dropped_tx));
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });
        ready_rx.await.unwrap();

        super::abort_and_join_background_tasks(vec![task]).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_server_drain_timeout_is_bounded() {
        let (shutdown_started_tx, shutdown_started_rx) = tokio::sync::oneshot::channel();
        shutdown_started_tx.send(()).unwrap();
        let started = tokio::time::Instant::now();
        let outcome = super::await_server_with_bounded_drain(
            std::future::pending::<()>(),
            shutdown_started_rx,
            std::time::Duration::from_millis(10),
        )
        .await;

        assert!(matches!(
            outcome,
            super::BoundedServerOutcome::DrainTimedOut
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn protected_provider_secret_file_has_a_hard_read_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-keyring.json");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![
            b'x';
            super::PROVIDER_SECRET_FILE_MAX_BYTES as usize + 1
        ])
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        }

        let error = super::read_protected_provider_secret_file(&path)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("maximum supported size"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_provider_secret_file_rejects_symbolic_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("provider-keyring.json");
        let link = directory.path().join("provider-keyring-link.json");
        std::fs::write(&target, r#"{"active_key_id":"test"}"#).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o400)).unwrap();
        symlink(&target, &link).unwrap();

        let error = super::read_protected_provider_secret_file(&link)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("must not be a symbolic link"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_provider_secret_file_accepts_a_private_regular_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provider-keyring.json");
        std::fs::write(&path, r#"{"active_key_id":"test"}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();

        let payload = super::read_protected_provider_secret_file(&path)
            .await
            .unwrap();

        assert_eq!(payload.as_str(), r#"{"active_key_id":"test"}"#);
    }
}
