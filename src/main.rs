mod app_state;
mod config;
mod db;
mod indexer;
mod model;
mod search;
mod web;

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::app_state::AppState;

const LOG_FILE_PREFIX: &str = "puppy_find.log";
const PRODUCTION_LOG_RETENTION_DAYS: u64 = 7;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace_dir = resolve_workspace_dir()?;
    let settings = config::load_or_create(&workspace_dir)?;
    let _log_guard = init_tracing(&workspace_dir, &settings);
    let db_path_value = config::validate_db_path(&workspace_dir, &settings.db_path)?;
    let db_path = config::resolve_path(&workspace_dir, &db_path_value);

    info!("workspace directory: {}", workspace_dir.display());
    info!(
        "file logs: {}",
        config::resolve_path(&workspace_dir, &settings.log_dir).display()
    );
    info!(
        "omni runtime: device={}, intra_threads={} (resolved={}), fgclip_max_patches={}",
        settings.omni_device,
        settings.omni_intra_threads,
        settings.resolved_omni_intra_threads(),
        settings.omni_fgclip_max_patches
    );

    db::init(&db_path)?;
    let model_signature = model::index_model_signature(&workspace_dir, &settings)?;
    let sync = db::sync_index_model_signature(&db_path, model_signature.as_deref())?;
    if sync.index_cleared {
        info!("detected model change on startup, cleared stale image vectors");
    }
    let indexed = db::count_images(&db_path)?;
    let state = AppState::new(workspace_dir, settings.clone(), indexed);

    let app = web::router(state);
    let listener = TcpListener::bind((settings.host.as_str(), settings.port))
        .await
        .context("failed to bind HTTP listener")?;
    let address = listener
        .local_addr()
        .context("failed to read listener address")?;
    let base_url = browser_base_url(&settings, address);

    info!("PuppyFind listening on {base_url}");

    let skip_browser = std::env::var("PUPPY_FIND_NO_BROWSER")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if !skip_browser {
        if let Err(error) = webbrowser::open(&base_url) {
            warn!("failed to open browser automatically: {error}");
        }
    }

    axum::serve(listener, app)
        .await
        .context("server exited unexpectedly")?;

    Ok(())
}

fn resolve_workspace_dir() -> anyhow::Result<PathBuf> {
    if cfg!(debug_assertions) {
        return std::env::current_dir().context("failed to read current directory");
    }

    let executable = std::env::current_exe().context("failed to read executable path")?;
    executable
        .parent()
        .map(Path::to_path_buf)
        .context("executable path does not have a parent directory")
}

fn init_tracing(
    workspace_dir: &Path,
    settings: &crate::config::AppSettings,
) -> Option<WorkerGuard> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_dir = config::resolve_path(workspace_dir, &settings.log_dir);

    let log_appender = match prepare_log_appender(&log_dir) {
        Ok(log_appender) => log_appender,
        Err(error) => {
            eprintln!(
                "failed to initialize file logging at {}: {error}",
                log_dir.display()
            );

            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_target(false)
                .compact()
                .init();
            return None;
        }
    };

    let (file_writer, guard) = tracing_appender::non_blocking(log_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .compact()
        .with_writer(file_writer);

    if cfg!(debug_assertions) {
        let console_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .compact()
            .with_writer(std::io::stderr);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(console_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .init();
    }

    Some(guard)
}

fn prepare_log_appender(
    log_dir: &Path,
) -> std::io::Result<tracing_appender::rolling::RollingFileAppender> {
    fs::create_dir_all(log_dir)?;
    if !cfg!(debug_assertions) {
        prune_old_logs(log_dir)?;
    }

    Ok(tracing_appender::rolling::daily(log_dir, LOG_FILE_PREFIX))
}

fn prune_old_logs(log_dir: &Path) -> std::io::Result<()> {
    let retention = Duration::from_secs(PRODUCTION_LOG_RETENTION_DAYS * 24 * 60 * 60);
    let Some(cutoff) = SystemTime::now().checked_sub(retention) else {
        return Ok(());
    };

    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !file_name.starts_with(LOG_FILE_PREFIX) {
            continue;
        }

        let modified = match entry.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        if modified >= cutoff {
            continue;
        }

        let _ = fs::remove_file(entry.path());
    }

    Ok(())
}

fn browser_base_url(settings: &crate::config::AppSettings, address: SocketAddr) -> String {
    let host = match settings.host.as_str() {
        "0.0.0.0" => "127.0.0.1".to_owned(),
        "::" => "::1".to_owned(),
        other => other.to_owned(),
    };

    let display_address = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{}", address.port())
    } else {
        format!("{host}:{}", address.port())
    };

    format!("http://{display_address}")
}
