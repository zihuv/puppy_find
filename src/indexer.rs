use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, anyhow, bail};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use walkdir::WalkDir;

use crate::app_state::AppState;
use crate::db::{self, IndexedImageSnapshot, NewImageRecord};

pub fn spawn_indexing(state: AppState) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let result = run_indexing(&state);
        match result {
            Ok(warning) => {
                if let Some(message) = warning.as_deref() {
                    warn!("indexing completed with warnings: {message}");
                }
                info!("indexing finished");
                state.finish_indexing(warning);
            }
            Err(error) => {
                error!("indexing failed: {error:#}");
                state.finish_indexing(Some(error.to_string()));
            }
        }
    })
}

fn run_indexing(state: &AppState) -> Result<Option<String>> {
    let settings = state.settings();
    let db_path = state.db_path();
    let image_dir = crate::config::resolve_path(state.workspace_dir(), &settings.asset_dir);

    let files = collect_image_files(&image_dir)?;
    let total_files = files.len();
    let existing = db::list_indexed_images(&db_path)?;
    let mut failed_count = 0usize;
    let mut failure_samples = Vec::new();

    state.update_index_status(|status| {
        status.total = total_files;
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
            match state.model_manager().embed_image_path(
                state.workspace_dir(),
                &settings,
                &file.path,
            ) {
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
                    failed_count += 1;
                    if failure_samples.len() < 3 {
                        failure_samples.push(format!("{path_string}: {error}"));
                    }
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
    let indexed = db::count_images(&db_path)?;
    state.update_index_status(|status| {
        status.indexed = indexed;
    });

    summarize_indexing_failures(total_files, indexed, failed_count, &failure_samples)
}

pub(crate) fn count_indexable_images(image_dir: &Path) -> Result<usize> {
    let metadata = fs::metadata(image_dir)
        .with_context(|| format!("素材目录不存在: {}", image_dir.display()))?;
    if !metadata.is_dir() {
        bail!("素材目录不是目录: {}", image_dir.display());
    }

    let mut count = 0;
    for entry in WalkDir::new(image_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if entry.file_type().is_file() && is_supported_image(entry.path()) {
            count += 1;
        }
    }

    Ok(count)
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
    let metadata = fs::metadata(image_dir)
        .with_context(|| format!("素材目录不存在: {}", image_dir.display()))?;
    if !metadata.is_dir() {
        bail!("素材目录不是目录: {}", image_dir.display());
    }

    let mut files = Vec::new();

    for entry in WalkDir::new(image_dir)
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

fn summarize_indexing_failures(
    total_files: usize,
    indexed: usize,
    failed_count: usize,
    failure_samples: &[String],
) -> Result<Option<String>> {
    if failed_count == 0 {
        return Ok(None);
    }

    let sample_text = failure_samples.join("；");
    if indexed == 0 {
        return Err(anyhow!(
            "索引失败：共扫描 {total_files} 张图片，{failed_count} 张嵌入失败。示例：{sample_text}"
        ));
    }

    Ok(Some(format!(
        "索引完成，但有 {failed_count}/{total_files} 张图片处理失败，当前已写入 {indexed} 条。示例：{sample_text}"
    )))
}

#[cfg(test)]
mod tests {
    #[test]
    fn summarize_indexing_failures_returns_error_when_everything_failed() {
        let error =
            super::summarize_indexing_failures(3, 0, 3, &["a.png: boom".to_owned()]).unwrap_err();

        assert!(error.to_string().contains("索引失败"));
        assert!(error.to_string().contains("a.png: boom"));
    }

    #[test]
    fn summarize_indexing_failures_returns_warning_when_partial_success_exists() {
        let warning =
            super::summarize_indexing_failures(5, 2, 3, &["a.png: boom".to_owned()]).unwrap();

        assert!(warning.unwrap().contains("处理失败"));
    }
}
