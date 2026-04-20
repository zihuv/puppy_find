use std::path::{Path as FsPath, PathBuf};
use std::process::Command;
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
        .route("/api/runtime", get(get_runtime_status))
        .route("/api/pick-directory", post(pick_directory))
        .route("/api/open-path", post(open_path))
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
        omni_device: settings.omni_device.to_string(),
        omni_provider_policy: settings.omni_provider_policy.to_string(),
        omni_fgclip_max_patches: settings.omni_fgclip_max_patches,
        needs_setup,
    })
}

async fn get_runtime_status(State(state): State<Arc<AppState>>) -> Json<RuntimeStatusResponse> {
    let settings = state.settings();
    Json(build_runtime_status_response(state.as_ref(), &settings))
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
        old_settings.omni_device,
        old_settings.omni_provider_policy,
        &old_settings.omni_intra_threads,
        old_settings.omni_fgclip_max_patches,
    )
    .map_err(ApiError::bad_request)?;
    let asset_dir = model::validate_asset_dir(workspace_dir, &payload.asset_dir)
        .map_err(ApiError::bad_request)?;

    let new_settings = AppSettings {
        db_path: old_settings.db_path.clone(),
        model_path,
        omni_device: old_settings.omni_device,
        omni_provider_policy: old_settings.omni_provider_policy,
        omni_intra_threads: old_settings.omni_intra_threads.clone(),
        omni_fgclip_max_patches: old_settings.omni_fgclip_max_patches,
        host: old_settings.host.clone(),
        port: old_settings.port,
        asset_dir,
        log_dir: old_settings.log_dir.clone(),
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
        omni_device: new_settings.omni_device.to_string(),
        omni_provider_policy: new_settings.omni_provider_policy.to_string(),
        omni_fgclip_max_patches: new_settings.omni_fgclip_max_patches,
        index_cleared: index_data_changed,
    }))
}

async fn open_path(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OpenPathRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let target_path = resolve_requested_path(state.workspace_dir(), &payload.path)
        .map_err(ApiError::bad_request)?;
    let target_kind = inspect_open_target(&target_path).map_err(ApiError::bad_request)?;
    let display_path = target_path.display().to_string();

    tokio::task::spawn_blocking(move || open_in_file_manager(&target_path, target_kind))
        .await
        .context("open path task join failed")
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;

    Ok(Json(MessageResponse {
        message: format!("已打开路径: {display_path}"),
    }))
}

async fn pick_directory(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<PickDirectoryRequest>,
) -> Result<Json<PickDirectoryResponse>, ApiError> {
    let initial_dir = dialog_initial_directory(state.workspace_dir(), payload.path.as_deref());
    let workspace_dir = state.workspace_dir().to_path_buf();

    let selected = tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new();
        dialog = dialog.set_directory(initial_dir);
        dialog.pick_folder()
    })
    .await
    .context("pick directory task join failed")
    .map_err(ApiError::internal)?;

    let Some(selected) = selected else {
        return Ok(Json(PickDirectoryResponse {
            path: None,
            canceled: true,
        }));
    };

    Ok(Json(PickDirectoryResponse {
        path: Some(display_selected_path(&workspace_dir, &selected)),
        canceled: false,
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
        settings.omni_device,
        settings.omni_provider_policy,
        &settings.omni_intra_threads,
        settings.omni_fgclip_max_patches,
    )
    .map_err(ApiError::bad_request)?;
    let asset_dir = model::validate_existing_asset_dir(state.workspace_dir(), &settings.asset_dir)
        .map_err(ApiError::bad_request)?;
    let total = indexer::count_indexable_images(&crate::config::resolve_path(
        state.workspace_dir(),
        &asset_dir,
    ))
    .map_err(ApiError::bad_request)?;
    let sync = sync_runtime_model_index(state.as_ref(), &settings)?;

    if !state.try_start_indexing(total) {
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
    if !state.index_status().running {
        refresh_idle_index_status(state.as_ref());
    }
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

fn resolve_requested_path(workspace_dir: &FsPath, value: &str) -> Result<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("路径不能为空"));
    }

    Ok(config::resolve_path(workspace_dir, trimmed))
}

fn dialog_initial_directory(workspace_dir: &FsPath, value: Option<&str>) -> PathBuf {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return workspace_dir.to_path_buf();
    };

    let mut candidate = config::resolve_path(workspace_dir, value);
    loop {
        if candidate.is_dir() {
            return candidate;
        }

        if candidate.exists() {
            if let Some(parent) = candidate.parent() {
                return parent.to_path_buf();
            }
            return workspace_dir.to_path_buf();
        }

        let Some(parent) = candidate.parent() else {
            return workspace_dir.to_path_buf();
        };
        if parent == candidate {
            return workspace_dir.to_path_buf();
        }
        candidate = parent.to_path_buf();
    }
}

fn display_selected_path(workspace_dir: &FsPath, selected: &FsPath) -> String {
    if let Ok(relative) = selected.strip_prefix(workspace_dir) {
        if relative.as_os_str().is_empty() {
            return ".".to_owned();
        }

        return format!("./{}", model::path_to_string(relative));
    }

    model::path_to_string(selected)
}

fn inspect_open_target(path: &FsPath) -> Result<OpenTargetKind> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("路径不存在: {}", path.display()))?;

    if metadata.is_dir() {
        return Ok(OpenTargetKind::Directory);
    }

    if metadata.is_file() {
        return Ok(OpenTargetKind::File);
    }

    Err(anyhow!("不支持打开该路径类型: {}", path.display()))
}

fn open_in_file_manager(path: &FsPath, kind: OpenTargetKind) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer.exe");
        match kind {
            OpenTargetKind::Directory => {
                command.arg(windows_path_arg(path));
            }
            OpenTargetKind::File => {
                command.arg(format!("/select,{}", windows_path_arg(path)));
            }
        }
        command
            .spawn()
            .with_context(|| format!("failed to open Explorer for {}", path.display()))?;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        if matches!(kind, OpenTargetKind::File) {
            command.arg("-R");
        }
        command
            .arg(path)
            .spawn()
            .with_context(|| format!("failed to open Finder for {}", path.display()))?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = match kind {
            OpenTargetKind::Directory => path.to_path_buf(),
            OpenTargetKind::File => path
                .parent()
                .map(FsPath::to_path_buf)
                .ok_or_else(|| anyhow!("文件没有可打开的父目录: {}", path.display()))?,
        };

        Command::new("xdg-open")
            .arg(&target)
            .spawn()
            .with_context(|| format!("failed to open file manager for {}", target.display()))?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(anyhow!("当前平台暂不支持打开路径"))
}

#[cfg(target_os = "windows")]
fn windows_path_arg(path: &FsPath) -> String {
    path.to_string_lossy().replace('/', "\\")
}

fn build_runtime_status_response(
    state: &AppState,
    settings: &AppSettings,
) -> RuntimeStatusResponse {
    if settings.model_path.trim().is_empty() {
        return RuntimeStatusResponse {
            snapshot: None,
            error: None,
        };
    }

    match state
        .model_manager()
        .runtime_snapshot(state.workspace_dir(), settings)
    {
        Ok(snapshot) => RuntimeStatusResponse {
            snapshot: Some(snapshot),
            error: None,
        },
        Err(error) => RuntimeStatusResponse {
            snapshot: None,
            error: Some(error.to_string()),
        },
    }
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

fn refresh_idle_index_status(state: &AppState) {
    let settings = state.settings();
    let indexed = match db::count_images(&state.db_path()) {
        Ok(indexed) => indexed,
        Err(_) => return,
    };
    let asset_dir = crate::config::resolve_path(state.workspace_dir(), &settings.asset_dir);
    let total = indexer::count_indexable_images(&asset_dir).unwrap_or(indexed);

    state.update_index_status(|status| {
        if status.running {
            return;
        }

        status.indexed = indexed;
        status.total = total;
        status.processed = indexed.min(total);
        status.current_file = None;
    });
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenPathRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PickDirectoryRequest {
    path: Option<String>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    items: Vec<search::SearchItem>,
}

#[derive(Debug, Serialize)]
struct PickDirectoryResponse {
    path: Option<String>,
    canceled: bool,
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
    omni_device: String,
    omni_provider_policy: String,
    omni_fgclip_max_patches: usize,
    index_cleared: bool,
}

#[derive(Debug, Serialize)]
struct SettingsView {
    model_path: String,
    asset_dir: String,
    omni_device: String,
    omni_provider_policy: String,
    omni_fgclip_max_patches: usize,
    needs_setup: bool,
}

#[derive(Debug, Serialize)]
struct RuntimeStatusResponse {
    snapshot: Option<omni_search::RuntimeSnapshot>,
    error: Option<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenTargetKind {
    Directory,
    File,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        OpenTargetKind, dialog_initial_directory, display_selected_path, inspect_open_target,
        resolve_requested_path,
    };

    #[test]
    fn resolve_requested_path_uses_workspace_for_relative_paths() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let resolved = resolve_requested_path(&workspace_dir, "./images/corgi").unwrap();

        assert_eq!(resolved, workspace_dir.join("images").join("corgi"));

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn resolve_requested_path_rejects_empty_input() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let error = resolve_requested_path(&workspace_dir, "   ").unwrap_err();

        assert!(error.to_string().contains("路径不能为空"));

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn dialog_initial_directory_falls_back_to_existing_parent() {
        let root = unique_test_dir();
        let child_dir = root.join("images");

        fs::create_dir_all(&child_dir).unwrap();

        let resolved = dialog_initial_directory(&root, Some("./images/missing/deeper/not-found"));

        assert_eq!(resolved, child_dir);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn display_selected_path_prefers_workspace_relative_format() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(workspace_dir.join("images")).unwrap();
        let selected = workspace_dir.join("images").join("corgi");

        let displayed = display_selected_path(&workspace_dir, &selected);

        assert_eq!(displayed, "./images/corgi");

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn inspect_open_target_distinguishes_files_and_directories() {
        let root = unique_test_dir();
        let child_dir = root.join("assets");
        let child_file = root.join("dog.png");

        fs::create_dir_all(&child_dir).unwrap();
        fs::write(&child_file, b"image").unwrap();

        assert_eq!(
            inspect_open_target(&child_dir).unwrap(),
            OpenTargetKind::Directory
        );
        assert_eq!(
            inspect_open_target(&child_file).unwrap(),
            OpenTargetKind::File
        );

        let _ = fs::remove_dir_all(&root);
    }

    fn unique_test_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("puppy_find_web_test_{timestamp}"))
    }
}
