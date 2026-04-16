use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use tokio::task::JoinHandle;
use tracing::{error, info};
use walkdir::WalkDir;

use crate::app_state::AppState;
use crate::db::{self, IndexedImageSnapshot, NewImageRecord};

pub fn spawn_indexing(state: AppState) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let result = run_indexing(&state);
        match result {
            Ok(()) => {
                info!("indexing finished");
                state.finish_indexing(None);
            }
            Err(error) => {
                error!("indexing failed: {error:#}");
                state.finish_indexing(Some(error.to_string()));
            }
        }
    })
}

fn run_indexing(state: &AppState) -> Result<()> {
    let settings = state.settings();
    let db_path = state.db_path();
    let model_dir = PathBuf::from(&settings.model_path);
    let image_dir = PathBuf::from(&settings.asset_dir);

    let files = collect_image_files(&image_dir)?;
    let existing = db::list_indexed_images(&db_path)?;

    state.update_index_status(|status| {
        status.total = files.len();
        status.processed = 0;
        status.current_file = None;
        status.error = None;
    });

    let mut keep_paths = HashSet::with_capacity(files.len());

    for file in files {
        let path_string = crate::model::path_to_string(&file.path);
        keep_paths.insert(path_string.clone());
        state.update_index_status(|status| {
            status.current_file = Some(path_string.clone());
        });

        let unchanged = existing.get(&path_string).is_some_and(|stored| {
            stored.mtime_ms == file.mtime_ms && stored.size_bytes == file.size_bytes
        });

        if !unchanged {
            match state
                .model_manager()
                .embed_image_path(&model_dir, &file.path)
            {
                Ok(vector) => {
                    let record = NewImageRecord {
                        path: path_string.clone(),
                        file_name: file.file_name,
                        mtime_ms: file.mtime_ms,
                        size_bytes: file.size_bytes,
                        dims: vector.len(),
                        vector,
                    };
                    db::upsert_image(&db_path, &record)?;
                }
                Err(error) => {
                    error!("skipping {}: {error:#}", file.path.display());
                }
            }
        }

        state.update_index_status(|status| {
            status.processed += 1;
        });
    }

    let removed_paths = collect_removed_paths(existing, &keep_paths);
    db::delete_images_by_paths(&db_path, &removed_paths)?;

    Ok(())
}

fn collect_removed_paths(
    existing: HashMap<String, IndexedImageSnapshot>,
    keep_paths: &HashSet<String>,
) -> Vec<String> {
    existing
        .into_keys()
        .filter(|path| !keep_paths.contains(path))
        .collect()
}

fn collect_image_files(image_dir: &Path) -> Result<Vec<FileEntry>> {
    let image_dir = crate::model::normalize_existing_dir(image_dir, "素材目录")?;
    let mut files = Vec::new();

    for entry in WalkDir::new(&image_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() || !is_supported_image(entry.path()) {
            continue;
        }

        let metadata = fs::metadata(entry.path())
            .with_context(|| format!("failed to read metadata for {}", entry.path().display()))?;
        let modified = metadata
            .modified()
            .with_context(|| format!("failed to read mtime for {}", entry.path().display()))?;
        let mtime_ms = modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        files.push(FileEntry {
            path: entry.path().to_path_buf(),
            file_name: entry.file_name().to_string_lossy().to_string(),
            mtime_ms,
            size_bytes: i64::try_from(metadata.len()).context("file size does not fit into i64")?,
        });
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "bmp"
            )
        })
        .unwrap_or(false)
}

struct FileEntry {
    path: PathBuf,
    file_name: String,
    mtime_ms: i64,
    size_bytes: i64,
}
