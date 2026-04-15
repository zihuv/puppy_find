mod app_state;
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
    let db_path = workspace_dir.join("puppy_find.sqlite3");

    db::init(&db_path)?;
    let settings = db::load_settings(&db_path)?.unwrap_or_default();
    let state = AppState::new(db_path, settings);

    let app = web::router(state);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind HTTP listener")?;
    let address = listener
        .local_addr()
        .context("failed to read listener address")?;
    let base_url = format!("http://{}", format_socket_address(address));

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

fn format_socket_address(address: SocketAddr) -> String {
    match address {
        SocketAddr::V4(_) => address.to_string(),
        SocketAddr::V6(v6) => format!("[{}]:{}", v6.ip(), v6.port()),
    }
}
