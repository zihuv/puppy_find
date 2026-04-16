use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const ENV_FILE_NAME: &str = ".env";
const KEY_DB_PATH: &str = "DB_PATH";
const KEY_MODEL_PATH: &str = "MODEL_PATH";
const KEY_MODEL_DIR_LEGACY: &str = "MODEL_DIR";
const KEY_OMNI_INTRA_THREADS: &str = "OMNI_INTRA_THREADS";
const KEY_OMNI_FGCLIP_MAX_PATCHES: &str = "OMNI_FGCLIP_MAX_PATCHES";
const KEY_HOST: &str = "HOST";
const KEY_PORT: &str = "PORT";
const KEY_ASSET_DIR: &str = "ASSET_DIR";
const KEY_IMAGE_DIR_LEGACY: &str = "IMAGE_DIR";
const SUPPORTED_FGCLIP_MAX_PATCHES: [usize; 5] = [128, 256, 576, 784, 1024];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    pub db_path: String,
    pub model_path: String,
    #[serde(default = "default_omni_intra_threads")]
    pub omni_intra_threads: usize,
    #[serde(default = "default_omni_fgclip_max_patches")]
    pub omni_fgclip_max_patches: usize,
    pub host: String,
    pub port: u16,
    pub asset_dir: String,
}

fn default_omni_intra_threads() -> usize {
    4
}

fn default_omni_fgclip_max_patches() -> usize {
    256
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl AppSettings {
    pub fn defaults() -> Self {
        Self {
            db_path: "./puppy_find.db".to_owned(),
            model_path: "./models/chinese_clip_bundle".to_owned(),
            omni_intra_threads: default_omni_intra_threads(),
            omni_fgclip_max_patches: default_omni_fgclip_max_patches(),
            host: "127.0.0.1".to_owned(),
            port: 3000,
            asset_dir: "./materials".to_owned(),
        }
    }
}

pub fn load_or_create(workspace_dir: &Path) -> Result<AppSettings> {
    let env_path = env_path(workspace_dir);
    let existing_text = match fs::read_to_string(&env_path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", env_path.display()));
        }
    };

    let parsed = existing_text
        .as_deref()
        .map(parse_env)
        .transpose()?
        .unwrap_or_default();
    let defaults = AppSettings::defaults();

    let settings = AppSettings {
        db_path: parsed
            .get(KEY_DB_PATH)
            .cloned()
            .unwrap_or_else(|| defaults.db_path.clone()),
        model_path: parsed
            .get(KEY_MODEL_PATH)
            .cloned()
            .or_else(|| parsed.get(KEY_MODEL_DIR_LEGACY).cloned())
            .unwrap_or_else(|| defaults.model_path.clone()),
        omni_intra_threads: parsed
            .get(KEY_OMNI_INTRA_THREADS)
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|value| validate_omni_intra_threads(value).ok())
            .unwrap_or(defaults.omni_intra_threads),
        omni_fgclip_max_patches: parsed
            .get(KEY_OMNI_FGCLIP_MAX_PATCHES)
            .and_then(|value| value.parse::<usize>().ok())
            .and_then(|value| validate_omni_fgclip_max_patches(value).ok())
            .unwrap_or(defaults.omni_fgclip_max_patches),
        host: parsed
            .get(KEY_HOST)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| defaults.host.clone()),
        port: parsed
            .get(KEY_PORT)
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(defaults.port),
        asset_dir: parsed
            .get(KEY_ASSET_DIR)
            .cloned()
            .or_else(|| parsed.get(KEY_IMAGE_DIR_LEGACY).cloned())
            .unwrap_or_else(|| defaults.asset_dir.clone()),
    };

    let missing_required_keys = [
        KEY_DB_PATH,
        KEY_MODEL_PATH,
        KEY_OMNI_INTRA_THREADS,
        KEY_OMNI_FGCLIP_MAX_PATCHES,
        KEY_HOST,
        KEY_PORT,
        KEY_ASSET_DIR,
    ]
    .iter()
    .any(|key| !parsed.contains_key(*key));
    let used_legacy_keys =
        parsed.contains_key(KEY_MODEL_DIR_LEGACY) || parsed.contains_key(KEY_IMAGE_DIR_LEGACY);
    let invalid_host = parsed
        .get(KEY_HOST)
        .is_some_and(|value| value.trim().is_empty());
    let invalid_port = parsed
        .get(KEY_PORT)
        .is_some_and(|value| value.parse::<u16>().is_err());
    let invalid_omni_intra_threads = parsed.get(KEY_OMNI_INTRA_THREADS).is_some_and(|value| {
        value
            .parse::<usize>()
            .ok()
            .and_then(|value| validate_omni_intra_threads(value).ok())
            .is_none()
    });
    let invalid_omni_fgclip_max_patches =
        parsed
            .get(KEY_OMNI_FGCLIP_MAX_PATCHES)
            .is_some_and(|value| {
                value
                    .parse::<usize>()
                    .ok()
                    .and_then(|value| validate_omni_fgclip_max_patches(value).ok())
                    .is_none()
            });

    if existing_text.is_none()
        || missing_required_keys
        || used_legacy_keys
        || invalid_host
        || invalid_port
        || invalid_omni_intra_threads
        || invalid_omni_fgclip_max_patches
    {
        save(workspace_dir, &settings)?;
    }

    Ok(settings)
}

pub fn save(workspace_dir: &Path, settings: &AppSettings) -> Result<()> {
    let env_path = env_path(workspace_dir);
    let existing_text = match fs::read_to_string(&env_path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", env_path.display()));
        }
    };

    let content = render_env_file(&existing_text, settings);
    fs::write(&env_path, content)
        .with_context(|| format!("failed to write {}", env_path.display()))?;

    Ok(())
}

pub fn resolve_path(workspace_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_dir.join(path)
    };

    normalize_path_lexically(&absolute)
}

pub fn validate_db_path(workspace_dir: &Path, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("数据库文件位置不能为空");
    }

    let path = resolve_path(workspace_dir, trimmed);
    if path.is_dir() {
        bail!("数据库文件位置不能是目录: {}", path.display());
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if parent.exists() && !parent.is_dir() {
            bail!("数据库文件父路径不是目录: {}", parent.display());
        }
    }

    Ok(trimmed.to_owned())
}

pub fn validate_host(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("HOST 不能为空");
    }
    if trimmed.chars().any(char::is_whitespace) {
        bail!("HOST 不能包含空白字符");
    }
    Ok(trimmed.to_owned())
}

pub fn validate_omni_intra_threads(value: usize) -> Result<usize> {
    if value == 0 {
        bail!("OMNI_INTRA_THREADS 必须大于 0");
    }
    Ok(value)
}

pub fn validate_omni_fgclip_max_patches(value: usize) -> Result<usize> {
    if !SUPPORTED_FGCLIP_MAX_PATCHES.contains(&value) {
        bail!("OMNI_FGCLIP_MAX_PATCHES 必须是 128、256、576、784 或 1024");
    }
    Ok(value)
}

pub fn needs_setup(settings: &AppSettings) -> bool {
    settings.model_path.trim().is_empty() || settings.asset_dir.trim().is_empty()
}

fn env_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(ENV_FILE_NAME)
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn render_env_file(existing_text: &str, settings: &AppSettings) -> String {
    let mut output = Vec::new();
    let mut seen_keys = HashSet::new();

    if existing_text.trim().is_empty() {
        output.push("# PuppyFind local configuration".to_owned());
    }

    for line in existing_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            output.push(line.to_owned());
            continue;
        }

        let Some((key, _)) = trimmed.split_once('=') else {
            output.push(line.to_owned());
            continue;
        };

        let normalized_key = key
            .trim()
            .strip_prefix("export ")
            .unwrap_or_else(|| key.trim());

        if let Some(rendered) = render_known_line(normalized_key, settings) {
            if seen_keys.insert(normalized_key.to_owned()) {
                output.push(rendered);
            }
            continue;
        }

        if matches!(normalized_key, KEY_MODEL_DIR_LEGACY | KEY_IMAGE_DIR_LEGACY) {
            continue;
        }

        output.push(line.to_owned());
    }

    for key in [
        KEY_DB_PATH,
        KEY_MODEL_PATH,
        KEY_OMNI_INTRA_THREADS,
        KEY_OMNI_FGCLIP_MAX_PATCHES,
        KEY_HOST,
        KEY_PORT,
        KEY_ASSET_DIR,
    ] {
        if !seen_keys.contains(key) {
            output.push(
                render_known_line(key, settings).expect("known configuration key must render"),
            );
        }
    }

    format!("{}\n", output.join("\n"))
}

fn render_known_line(key: &str, settings: &AppSettings) -> Option<String> {
    match key {
        KEY_DB_PATH => Some(render_string_assignment(KEY_DB_PATH, &settings.db_path)),
        KEY_MODEL_PATH => Some(render_string_assignment(
            KEY_MODEL_PATH,
            &settings.model_path,
        )),
        KEY_OMNI_INTRA_THREADS => Some(format!(
            "{KEY_OMNI_INTRA_THREADS}={}",
            settings.omni_intra_threads
        )),
        KEY_OMNI_FGCLIP_MAX_PATCHES => Some(format!(
            "{KEY_OMNI_FGCLIP_MAX_PATCHES}={}",
            settings.omni_fgclip_max_patches
        )),
        KEY_HOST => Some(render_string_assignment(KEY_HOST, &settings.host)),
        KEY_PORT => Some(format!("{KEY_PORT}={}", settings.port)),
        KEY_ASSET_DIR => Some(render_string_assignment(KEY_ASSET_DIR, &settings.asset_dir)),
        _ => None,
    }
}

fn render_string_assignment(key: &str, value: &str) -> String {
    format!("{key}={}", quote_env_value(value))
}

fn quote_env_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }

    format!("\"{escaped}\"")
}

fn parse_env(text: &str) -> Result<HashMap<String, String>> {
    let mut values = HashMap::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let normalized_key = key
            .trim()
            .strip_prefix("export ")
            .unwrap_or_else(|| key.trim());
        values.insert(normalized_key.to_owned(), parse_env_value(value.trim())?);
    }

    Ok(values)
}

fn parse_env_value(value: &str) -> Result<String> {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        return Ok(unescape_quoted_value(&value[1..value.len() - 1]));
    }

    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return Ok(value[1..value.len() - 1].to_owned());
    }

    Ok(value.to_owned())
}

fn unescape_quoted_value(value: &str) -> String {
    let mut chars = value.chars();
    let mut output = String::with_capacity(value.len());

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') => output.push('\\'),
            Some('"') => output.push('"'),
            Some(other) => output.push(other),
            None => output.push('\\'),
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{resolve_path, validate_db_path};

    #[test]
    fn resolve_path_normalizes_relative_segments() {
        let workspace_dir = PathBuf::from("D:/code/puppy_find");

        let left = resolve_path(&workspace_dir, "./materials");
        let right = resolve_path(&workspace_dir, "materials");

        assert_eq!(left, right);
    }

    #[test]
    fn validate_db_path_preserves_relative_input() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let validated = validate_db_path(&workspace_dir, "./data/app.db").unwrap();

        assert_eq!(validated, "./data/app.db");

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    fn unique_test_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("puppy_find_config_test_{timestamp}"))
    }
}
