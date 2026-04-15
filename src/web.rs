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
use crate::db::{self, AppSettings};
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

async fn get_settings(State(state): State<Arc<AppState>>) -> Json<AppSettings> {
    Json(state.settings())
}

async fn save_settings(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AppSettings>,
) -> Result<Json<SettingsResponse>, ApiError> {
    if state.index_status().running {
        return Err(ApiError::bad_request(anyhow!(
            "索引执行中，暂时不能修改配置"
        )));
    }

    let model_dir = model::validate_model_dir(&payload.model_dir).map_err(ApiError::bad_request)?;
    let image_dir = model::validate_image_dir(&payload.image_dir).map_err(ApiError::bad_request)?;

    let new_settings = AppSettings {
        model_dir,
        image_dir,
    };
    let old_settings = state.settings();
    let changed = old_settings != new_settings;

    db::save_settings(state.db_path(), &new_settings).map_err(ApiError::internal)?;

    if changed {
        db::clear_images(state.db_path()).map_err(ApiError::internal)?;
        state.model_manager().clear();
        state.update_index_status(|status| {
            status.total = 0;
            status.processed = 0;
            status.current_file = None;
            status.error = None;
        });
    }

    state.replace_settings(new_settings.clone());

    Ok(Json(SettingsResponse {
        model_dir: new_settings.model_dir,
        image_dir: new_settings.image_dir,
        index_cleared: changed,
    }))
}

async fn start_index(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let settings = state.settings();
    if settings.model_dir.is_empty() || settings.image_dir.is_empty() {
        return Err(ApiError::bad_request(anyhow!("请先保存模型目录和图片目录")));
    }

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
            message: "索引任务已启动".to_owned(),
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
    let path = db::get_image_path(state.db_path(), id)
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

#[derive(Debug, Serialize)]
struct SettingsResponse {
    model_dir: String,
    image_dir: String,
    index_cleared: bool,
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
