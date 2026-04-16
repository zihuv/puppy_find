use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::app_state::AppState;
use crate::config::{self, AppSettings};
use crate::db;
use crate::{indexer, model, search};

#[derive(RustEmbed)]
#[folder = "assets"]
struct Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/assets/{*path}", get(asset))
        .route("/api/settings", get(get_settings).post(save_settings))
        .route("/api/index", post(start_index))
        .route("/api/index/status", get(index_status))
        .route("/api/search", post(search_images))
        .route("/api/images/{id}", get(get_image))
        .with_state(Arc::new(state))
}

async fn root() -> Result<Html<String>, ApiError> {
    let html = load_asset_text("index.html")?;
    Ok(Html(html))
}

async fn asset(Path(path): Path<String>) -> Result<Response<Body>, ApiError> {
    serve_asset(&path)
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsView> {
    let settings = state.settings();
    let needs_setup = config::needs_setup(&settings);
    Json(SettingsView {
        model_path: settings.model_path,
        asset_dir: settings.asset_dir,
        needs_setup,
    })
}

async fn save_settings(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SettingsPayload>,
) -> Result<Json<SettingsResponse>, ApiError> {
    if state.index_status().running {
        return Err(ApiError::bad_request(anyhow!(
            "索引执行中，暂时不能修改配置"
        )));
    }

    let workspace_dir = state.workspace_dir();
    let old_settings = state.settings();
    let model_path = model::validate_model_dir(
        workspace_dir,
        &payload.model_path,
        old_settings.omni_intra_threads,
        old_settings.omni_fgclip_max_patches,
    )
    .map_err(ApiError::bad_request)?;
    let asset_dir = model::validate_asset_dir(workspace_dir, &payload.asset_dir)
        .map_err(ApiError::bad_request)?;

    let new_settings = AppSettings {
        db_path: old_settings.db_path.clone(),
        model_path,
        omni_intra_threads: old_settings.omni_intra_threads,
        omni_fgclip_max_patches: old_settings.omni_fgclip_max_patches,
        host: old_settings.host.clone(),
        port: old_settings.port,
        asset_dir,
    };
    let old_model_path = resolved_path_key(workspace_dir, &old_settings.model_path);
    let new_model_path = resolved_path_key(workspace_dir, &new_settings.model_path);
    let old_asset_dir = resolved_path_key(workspace_dir, &old_settings.asset_dir);
    let new_asset_dir = resolved_path_key(workspace_dir, &new_settings.asset_dir);
    let new_model_signature =
        model::index_model_signature(workspace_dir, &new_settings).map_err(ApiError::internal)?;

    let index_data_changed = old_model_path != new_model_path || old_asset_dir != new_asset_dir;
    let should_clear_images = old_model_path != new_model_path || old_asset_dir != new_asset_dir;
    let active_db_path = state.db_path();

    if should_clear_images {
        db::clear_images(&active_db_path).map_err(ApiError::internal)?;
        db::set_index_model_signature(&active_db_path, new_model_signature.as_deref())
            .map_err(ApiError::internal)?;
    }

    if index_data_changed {
        state.model_manager().clear();
    }

    if index_data_changed {
        reset_index_status(state.as_ref());
    }

    config::save(state.workspace_dir(), &new_settings).map_err(ApiError::internal)?;
    state.replace_settings(new_settings.clone());

    Ok(Json(SettingsResponse {
        model_path: new_settings.model_path,
        asset_dir: new_settings.asset_dir,
        index_cleared: index_data_changed,
    }))
}

async fn start_index(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let settings = state.settings();
    if settings.model_path.is_empty() || settings.asset_dir.is_empty() {
        return Err(ApiError::bad_request(anyhow!(
            "请先保存 MODEL_PATH 和素材目录"
        )));
    }

    model::validate_model_dir(
        state.workspace_dir(),
        &settings.model_path,
        settings.omni_intra_threads,
        settings.omni_fgclip_max_patches,
    )
    .map_err(ApiError::bad_request)?;
    model::validate_existing_asset_dir(state.workspace_dir(), &settings.asset_dir)
        .map_err(ApiError::bad_request)?;
    let sync = sync_runtime_model_index(state.as_ref(), &settings)?;

    if !state.try_start_indexing() {
        return Ok((
            StatusCode::CONFLICT,
            Json(MessageResponse {
                message: "正在执行中".to_owned(),
            }),
        ));
    }

    indexer::spawn_indexing(state.as_ref().clone());

    Ok((
        StatusCode::ACCEPTED,
        Json(MessageResponse {
            message: if sync.index_cleared {
                "检测到模型已变更，已清空旧索引并启动重建".to_owned()
            } else {
                "索引任务已启动".to_owned()
            },
        }),
    ))
}

async fn index_status(State(state): State<Arc<AppState>>) -> Json<crate::app_state::IndexStatus> {
    Json(state.index_status())
}

async fn search_images(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    let query = payload.query.trim();
    if query.is_empty() {
        return Err(ApiError::bad_request(anyhow!("请输入搜索文本")));
    }

    let limit = payload.limit.unwrap_or(60).clamp(1, 200);
    let settings = state.settings();
    let sync = sync_runtime_model_index(state.as_ref(), &settings)?;
    if sync.index_cleared {
        return Err(ApiError::bad_request(
            "检测到模型已变更，已清空旧索引，请重新建立索引",
        ));
    }

    let state = state.clone();
    let query = query.to_owned();
    let items = tokio::task::spawn_blocking(move || search::run_search(&state, &query, limit))
        .await
        .context("search task join failed")
        .map_err(ApiError::internal)?
        .map_err(ApiError::bad_request)?;

    Ok(Json(SearchResponse { items }))
}

async fn get_image(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Response<Body>, ApiError> {
    let db_path = state.db_path();
    let path = db::get_image_path(&db_path, id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("图片不存在"))?;
    let bytes = fs::read(&path)
        .await
        .with_context(|| format!("failed to read image file {path}"))
        .map_err(ApiError::not_found)?;
    let content_type = mime_guess::from_path(&path).first_or_octet_stream();

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            HeaderValue::from_str(content_type.as_ref()).map_err(ApiError::internal)?,
        )
        .body(Body::from(bytes))
        .map_err(ApiError::internal)
}

fn serve_asset(path: &str) -> Result<Response<Body>, ApiError> {
    let embedded = Assets::get(path).ok_or_else(|| ApiError::not_found("资源不存在"))?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            HeaderValue::from_str(mime.as_ref()).map_err(ApiError::internal)?,
        )
        .body(Body::from(embedded.data.into_owned()))
        .map_err(ApiError::internal)
}

fn load_asset_text(path: &str) -> Result<String, ApiError> {
    let bytes = Assets::get(path).ok_or_else(|| ApiError::not_found("页面不存在"))?;
    String::from_utf8(bytes.data.into_owned())
        .context("asset is not valid UTF-8")
        .map_err(ApiError::internal)
}

fn resolved_path_key(workspace_dir: &std::path::Path, value: &str) -> String {
    model::path_to_string(&config::resolve_path(workspace_dir, value))
}

fn sync_runtime_model_index(
    state: &AppState,
    settings: &AppSettings,
) -> Result<db::IndexModelSync, ApiError> {
    if state.index_status().running {
        return Ok(db::IndexModelSync {
            stored_signature: db::get_index_model_signature(&state.db_path())
                .map_err(ApiError::internal)?,
            current_signature: None,
            index_cleared: false,
        });
    }

    let model_signature = model::index_model_signature(state.workspace_dir(), settings)
        .map_err(ApiError::internal)?;
    let sync = db::sync_index_model_signature(&state.db_path(), model_signature.as_deref())
        .map_err(ApiError::internal)?;

    if sync.index_cleared {
        state.model_manager().clear();
        reset_index_status(state);
    }

    Ok(sync)
}

fn reset_index_status(state: &AppState) {
    state.update_index_status(|status| {
        status.indexed = 0;
        status.total = 0;
        status.processed = 0;
        status.current_file = None;
        status.error = None;
    });
}

#[derive(Debug, Deserialize)]
struct SearchRequest {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    items: Vec<search::SearchItem>,
}

#[derive(Debug, Serialize)]
struct MessageResponse {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPayload {
    model_path: String,
    asset_dir: String,
}

#[derive(Debug, Serialize)]
struct SettingsResponse {
    model_path: String,
    asset_dir: String,
    index_cleared: bool,
}

#[derive(Debug, Serialize)]
struct SettingsView {
    model_path: String,
    asset_dir: String,
    needs_setup: bool,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(error: impl ToString) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }

    fn not_found(error: impl ToString) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: error.to_string(),
        }
    }

    fn internal(error: impl ToString) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response<Body> {
        let body = Json(ErrorResponse {
            error: self.message,
        });
        (self.status, body).into_response()
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}
