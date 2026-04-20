use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, anyhow, bail};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use walkdir::WalkDir;

use crate::app_state::AppState;
use crate::db::{self, IndexedImageSnapshot, NewImageRecord};

const AVIF_BRANDS: [&[u8; 4]; 2] = [b"avif", b"avis"];
const IMAGE_SIGNATURE_BYTES: usize = 32;

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
    has_supported_image_extension(path) && !has_avif_signature(path)
}

fn has_supported_image_extension(path: &Path) -> bool {
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

fn has_avif_signature(path: &Path) -> bool {
    let mut header = [0u8; IMAGE_SIGNATURE_BYTES];
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(read_len) = file.read(&mut header) else {
        return false;
    };

    is_avif_header(&header[..read_len])
}

fn is_avif_header(header: &[u8]) -> bool {
    if header.len() < 12 || &header[4..8] != b"ftyp" {
        return false;
    }

    let major_brand = &header[8..12];
    if AVIF_BRANDS.iter().any(|brand| major_brand == *brand) {
        return true;
    }

    if header.len() < 20 {
        return false;
    }

    header[16..]
        .chunks_exact(4)
        .any(|brand| AVIF_BRANDS.iter().any(|candidate| brand == *candidate))
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
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn count_indexable_images_skips_avif_disguised_as_png() {
        let root = unique_test_dir();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("photo.jpg"), b"plain-jpg").unwrap();
        fs::write(root.join("fake.png"), avif_header_bytes()).unwrap();

        let count = super::count_indexable_images(&root).unwrap();

        assert_eq!(count, 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_image_files_skips_avif_disguised_as_png() {
        let root = unique_test_dir();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("photo.jpg"), b"plain-jpg").unwrap();
        fs::write(root.join("fake.png"), avif_header_bytes()).unwrap();

        let files = super::collect_image_files(&root).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name, "photo.jpg");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn is_avif_header_matches_compatible_brand() {
        let header = [
            0, 0, 0, 24, b'f', b't', b'y', b'p', b'm', b'i', b'f', b'1', 0, 0, 0, 0, b'a', b'v',
            b'i', b'f',
        ];

        assert!(super::is_avif_header(&header));
    }

    fn avif_header_bytes() -> &'static [u8] {
        &[
            0, 0, 0, 24, b'f', b't', b'y', b'p', b'a', b'v', b'i', b'f', 0, 0, 0, 0, b'm', b'i',
            b'f', b'1',
        ]
    }

    fn unique_test_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("puppy_find_indexer_test_{timestamp}"))
    }
}
