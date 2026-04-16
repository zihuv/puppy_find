use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use omni_search::{OmniSearch, OmniSearchBuilder, probe_local_model_dir};

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
