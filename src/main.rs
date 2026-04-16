mod app_state;
mod config;
mod db;
mod indexer;
mod model;
mod search;
mod web;

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::app_state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let workspace_dir = std::env::current_dir().context("failed to read current directory")?;
    let settings = config::load_or_create(&workspace_dir)?;
    let db_path_value = config::validate_db_path(&workspace_dir, &settings.db_path)?;
    let db_path = config::resolve_path(&workspace_dir, &db_path_value);

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

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .init();
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
