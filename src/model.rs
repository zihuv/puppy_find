use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, anyhow, bail};
use omni_search::{OmniSearch, OmniSearchBuilder, probe_local_model_dir};
use walkdir::WalkDir;

use crate::config::AppSettings;

#[derive(Clone, Default)]
pub struct ModelManager {
    inner: Arc<Mutex<ModelSlot>>,
}

#[derive(Default)]
struct ModelSlot {
    model_key: Option<ModelCacheKey>,
    model: Option<OmniSearch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelCacheKey {
    model_dir: PathBuf,
    omni_intra_threads: usize,
    omni_fgclip_max_patches: usize,
}

impl ModelManager {
    pub fn embed_text(
        &self,
        workspace_dir: &Path,
        settings: &AppSettings,
        text: &str,
    ) -> Result<Vec<f32>> {
        self.with_model(workspace_dir, settings, |model| {
            model
                .embed_text(text)
                .map(|embedding| embedding.as_slice().to_vec())
                .map_err(|error| anyhow!("failed to embed text: {error}"))
        })
    }

    pub fn embed_image_path(
        &self,
        workspace_dir: &Path,
        settings: &AppSettings,
        image_path: &Path,
    ) -> Result<Vec<f32>> {
        self.with_model(workspace_dir, settings, |model| {
            model
                .embed_image_path(image_path)
                .map(|embedding| embedding.as_slice().to_vec())
                .map_err(|error| anyhow!("failed to embed image {}: {error}", image_path.display()))
        })
    }

    pub fn clear(&self) {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.model = None;
        slot.model_key = None;
    }

    fn with_model<T>(
        &self,
        workspace_dir: &Path,
        settings: &AppSettings,
        f: impl FnOnce(&OmniSearch) -> Result<T>,
    ) -> Result<T> {
        let mut slot = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let normalized =
            normalize_existing_dir(workspace_dir, Path::new(&settings.model_path), "模型目录")?;
        let model_key = ModelCacheKey {
            model_dir: normalized.clone(),
            omni_intra_threads: settings.omni_intra_threads,
            omni_fgclip_max_patches: settings.omni_fgclip_max_patches,
        };

        if slot.model_key.as_ref() != Some(&model_key) {
            let mut builder = OmniSearch::builder();
            builder.from_local_model_dir(&normalized);
            apply_runtime_settings(&mut builder, settings);
            let model = builder.build().with_context(|| {
                format!("failed to load model bundle from {}", normalized.display())
            })?;

            slot.model = Some(model);
            slot.model_key = Some(model_key);
        }

        let model = slot
            .model
            .as_ref()
            .ok_or_else(|| anyhow!("model runtime is not initialized"))?;
        f(model)
    }
}

pub fn validate_model_dir(
    workspace_dir: &Path,
    value: &str,
    omni_intra_threads: usize,
    omni_fgclip_max_patches: usize,
) -> Result<String> {
    let trimmed = trim_path_input(value, "模型目录")?;
    let normalized = normalize_existing_dir(workspace_dir, Path::new(trimmed), "模型目录")?;
    let probe = probe_local_model_dir(&normalized);

    if !probe.ok {
        let message = probe
            .error
            .unwrap_or_else(|| "模型目录不是有效的 omni_search bundle".to_owned());
        bail!("{message}");
    }

    ensure_model_loadable(
        &probe.normalized_path,
        omni_intra_threads,
        omni_fgclip_max_patches,
    )?;

    Ok(trimmed.to_owned())
}

pub fn index_model_signature(
    workspace_dir: &Path,
    settings: &AppSettings,
) -> Result<Option<String>> {
    let normalized =
        match normalize_existing_dir(workspace_dir, Path::new(&settings.model_path), "模型目录")
        {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
    let probe = probe_local_model_dir(&normalized);
    if !probe.ok {
        return Ok(None);
    }

    fingerprint_model_dir(&probe.normalized_path, settings.omni_fgclip_max_patches).map(Some)
}

pub fn validate_asset_dir(workspace_dir: &Path, value: &str) -> Result<String> {
    let trimmed = trim_path_input(value, "素材目录")?;
    normalize_dir_path(workspace_dir, Path::new(trimmed), "素材目录")?;
    Ok(trimmed.to_owned())
}

pub fn validate_existing_asset_dir(workspace_dir: &Path, value: &str) -> Result<String> {
    let trimmed = trim_path_input(value, "素材目录")?;
    normalize_existing_dir(workspace_dir, Path::new(trimmed), "素材目录")?;
    Ok(trimmed.to_owned())
}

pub fn normalize_dir_path(workspace_dir: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("{label}不能为空");
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_dir.join(path)
    };

    Ok(crate::config::resolve_path(
        workspace_dir,
        &absolute.to_string_lossy(),
    ))
}

pub fn normalize_existing_dir(workspace_dir: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let absolute = normalize_dir_path(workspace_dir, path, label)?;

    let metadata = fs::metadata(&absolute)
        .with_context(|| format!("{label}不存在: {}", absolute.display()))?;
    if !metadata.is_dir() {
        bail!("{label}不是目录: {}", absolute.display());
    }

    Ok(absolute)
}

pub fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn trim_path_input<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label}不能为空");
    }
    Ok(trimmed)
}

fn apply_runtime_settings(builder: &mut OmniSearchBuilder, settings: &AppSettings) {
    apply_runtime_overrides(
        builder,
        settings.omni_intra_threads,
        settings.omni_fgclip_max_patches,
    );
}

fn apply_runtime_overrides(
    builder: &mut OmniSearchBuilder,
    omni_intra_threads: usize,
    omni_fgclip_max_patches: usize,
) {
    builder.intra_threads(omni_intra_threads);
    builder.fgclip_max_patches(omni_fgclip_max_patches);
}

fn ensure_model_loadable(
    model_dir: &Path,
    omni_intra_threads: usize,
    omni_fgclip_max_patches: usize,
) -> Result<()> {
    let mut builder = OmniSearch::builder();
    builder.from_local_model_dir(model_dir);
    apply_runtime_overrides(&mut builder, omni_intra_threads, omni_fgclip_max_patches);
    builder
        .build()
        .with_context(|| format!("failed to load model bundle from {}", model_dir.display()))?;
    Ok(())
}

fn fingerprint_model_dir(model_dir: &Path, omni_fgclip_max_patches: usize) -> Result<String> {
    let mut hasher = StableHasher::default();
    let normalized_model_dir = path_to_string(model_dir);
    hasher.update_str("puppy_find_model_signature_v1");
    hasher.update_str(&normalized_model_dir);
    hasher.update_usize(omni_fgclip_max_patches);

    let mut file_count = 0usize;
    for entry in WalkDir::new(model_dir)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }

        let relative_path = entry.path().strip_prefix(model_dir).with_context(|| {
            format!(
                "failed to compute relative path for {}",
                entry.path().display()
            )
        })?;
        let metadata = fs::metadata(entry.path())
            .with_context(|| format!("failed to read metadata for {}", entry.path().display()))?;
        let modified = metadata
            .modified()
            .with_context(|| format!("failed to read mtime for {}", entry.path().display()))?;
        let mtime_ms = modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        hasher.update_str(&path_to_string(relative_path));
        hasher.update_u64(metadata.len());
        hasher.update_u128(mtime_ms);
        file_count += 1;
    }

    Ok(format!(
        "v1:{}:{}:{}:{:016x}",
        normalized_model_dir,
        omni_fgclip_max_patches,
        file_count,
        hasher.finish()
    ))
}

#[derive(Clone, Copy)]
struct StableHasher {
    state: u64,
}

impl Default for StableHasher {
    fn default() -> Self {
        Self {
            state: 0xcbf29ce484222325,
        }
    }
}

impl StableHasher {
    fn update_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn update_str(&mut self, value: &str) {
        self.update_bytes(value.as_bytes());
        self.update_bytes(&[0]);
    }

    fn update_usize(&mut self, value: usize) {
        self.update_u64(value as u64);
    }

    fn update_u64(&mut self, value: u64) {
        self.update_bytes(&value.to_le_bytes());
    }

    fn update_u128(&mut self, value: u128) {
        self.update_bytes(&value.to_le_bytes());
    }

    fn finish(self) -> u64 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::fingerprint_model_dir;

    #[test]
    fn fingerprint_model_dir_changes_when_bundle_file_changes() {
        let root = unique_test_dir();
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("weights.bin");
        fs::write(&file_path, b"model-a").unwrap();

        let first = fingerprint_model_dir(&root, 256).unwrap();
        sleep(Duration::from_millis(5));
        fs::write(&file_path, b"model-b-updated").unwrap();
        let second = fingerprint_model_dir(&root, 256).unwrap();

        assert_ne!(first, second);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fingerprint_model_dir_changes_when_patch_setting_changes() {
        let root = unique_test_dir();
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("weights.bin"), b"model").unwrap();

        let left = fingerprint_model_dir(&root, 256).unwrap();
        let right = fingerprint_model_dir(&root, 576).unwrap();

        assert_ne!(left, right);

        let _ = fs::remove_dir_all(&root);
    }

    fn unique_test_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("puppy_find_model_test_{timestamp}"))
    }
}
