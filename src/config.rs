use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::{fmt, str::FromStr};

use anyhow::{Context, Result, bail};
use omni_search::{
    ProviderPolicy, RuntimeDevice, default_intra_threads as omni_default_intra_threads,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const CONFIG_DIR_NAME: &str = "config";
const ENV_FILE_NAME: &str = ".env";
const DEFAULT_MODEL_DIR: &str = "./config/model";
const DEFAULT_LOG_DIR: &str = "./config/log";
const KEY_DB_PATH: &str = "DB_PATH";
const KEY_MODEL_PATH: &str = "MODEL_PATH";
const KEY_MODEL_DIR_LEGACY: &str = "MODEL_DIR";
const KEY_OMNI_DEVICE: &str = "OMNI_DEVICE";
const KEY_OMNI_PROVIDER_POLICY: &str = "OMNI_PROVIDER_POLICY";
const KEY_OMNI_INTRA_THREADS: &str = "OMNI_INTRA_THREADS";
const KEY_OMNI_FGCLIP_MAX_PATCHES: &str = "OMNI_FGCLIP_MAX_PATCHES";
const KEY_HOST: &str = "HOST";
const KEY_PORT: &str = "PORT";
const KEY_ASSET_DIR: &str = "ASSET_DIR";
const KEY_LOG_DIR: &str = "LOG_DIR";
const KEY_IMAGE_DIR_LEGACY: &str = "IMAGE_DIR";
const SUPPORTED_FGCLIP_MAX_PATCHES: [usize; 5] = [128, 256, 576, 784, 1024];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmniIntraThreads {
    Auto,
    Fixed(usize),
}

impl OmniIntraThreads {
    pub fn resolved(&self) -> usize {
        match self {
            Self::Auto => omni_default_intra_threads(),
            Self::Fixed(value) => *value,
        }
    }

    pub fn as_env_value(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Fixed(value) => value.to_string(),
        }
    }
}

impl Default for OmniIntraThreads {
    fn default() -> Self {
        Self::Auto
    }
}

impl fmt::Display for OmniIntraThreads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Fixed(value) => write!(f, "{value}"),
        }
    }
}

impl FromStr for OmniIntraThreads {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.trim().eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }

        let value = value
            .parse::<usize>()
            .context("failed to parse OMNI_INTRA_THREADS")?;
        validate_omni_intra_threads(value)?;
        Ok(Self::Fixed(value))
    }
}

impl Serialize for OmniIntraThreads {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Fixed(value) => serializer.serialize_u64(*value as u64),
        }
    }
}

impl<'de> Deserialize<'de> for OmniIntraThreads {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            String(String),
            Number(usize),
        }

        match Repr::deserialize(deserializer)? {
            Repr::String(value) => OmniIntraThreads::from_str(&value)
                .map_err(|error| serde::de::Error::custom(error.to_string())),
            Repr::Number(value) => {
                validate_omni_intra_threads(value)
                    .map_err(|error| serde::de::Error::custom(error.to_string()))?;
                Ok(OmniIntraThreads::Fixed(value))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSettings {
    pub db_path: String,
    pub model_path: String,
    #[serde(default = "default_omni_device")]
    pub omni_device: RuntimeDevice,
    #[serde(default = "default_omni_provider_policy")]
    pub omni_provider_policy: ProviderPolicy,
    #[serde(default = "default_omni_intra_threads")]
    pub omni_intra_threads: OmniIntraThreads,
    #[serde(default = "default_omni_fgclip_max_patches")]
    pub omni_fgclip_max_patches: usize,
    pub host: String,
    pub port: u16,
    pub asset_dir: String,
    pub log_dir: String,
}

fn default_omni_device() -> RuntimeDevice {
    RuntimeDevice::Auto
}

fn default_omni_provider_policy() -> ProviderPolicy {
    ProviderPolicy::Interactive
}

fn default_omni_intra_threads() -> OmniIntraThreads {
    OmniIntraThreads::Auto
}

fn default_omni_fgclip_max_patches() -> usize {
    256
}

fn default_asset_dir() -> String {
    dirs::picture_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Pictures")))
        .unwrap_or_else(|| PathBuf::from("./images"))
        .to_string_lossy()
        .into_owned()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl AppSettings {
    pub fn defaults() -> Self {
        Self {
            db_path: "./config/puppy_find.db".to_owned(),
            model_path: DEFAULT_MODEL_DIR.to_owned(),
            omni_device: default_omni_device(),
            omni_provider_policy: default_omni_provider_policy(),
            omni_intra_threads: default_omni_intra_threads(),
            omni_fgclip_max_patches: default_omni_fgclip_max_patches(),
            host: "127.0.0.1".to_owned(),
            port: 3000,
            asset_dir: default_asset_dir(),
            log_dir: DEFAULT_LOG_DIR.to_owned(),
        }
    }

    pub fn resolved_omni_intra_threads(&self) -> usize {
        self.omni_intra_threads.resolved()
    }
}

pub fn load_or_create(workspace_dir: &Path) -> Result<AppSettings> {
    ensure_default_layout(workspace_dir)?;
    let existing_text = read_existing_env_text(workspace_dir)?;

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
        omni_device: parsed
            .get(KEY_OMNI_DEVICE)
            .and_then(|value| parse_runtime_device(value).ok())
            .unwrap_or(defaults.omni_device),
        omni_provider_policy: parsed
            .get(KEY_OMNI_PROVIDER_POLICY)
            .and_then(|value| parse_provider_policy(value).ok())
            .unwrap_or(defaults.omni_provider_policy),
        omni_intra_threads: parsed
            .get(KEY_OMNI_INTRA_THREADS)
            .and_then(|value| parse_omni_intra_threads(value).ok())
            .unwrap_or_else(|| defaults.omni_intra_threads.clone()),
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
        log_dir: parsed
            .get(KEY_LOG_DIR)
            .cloned()
            .unwrap_or_else(|| defaults.log_dir.clone()),
    };

    let missing_required_keys = [
        KEY_DB_PATH,
        KEY_MODEL_PATH,
        KEY_OMNI_DEVICE,
        KEY_OMNI_PROVIDER_POLICY,
        KEY_OMNI_INTRA_THREADS,
        KEY_OMNI_FGCLIP_MAX_PATCHES,
        KEY_HOST,
        KEY_PORT,
        KEY_ASSET_DIR,
        KEY_LOG_DIR,
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
    let invalid_omni_device = parsed
        .get(KEY_OMNI_DEVICE)
        .is_some_and(|value| parse_runtime_device(value).is_err());
    let invalid_omni_provider_policy = parsed
        .get(KEY_OMNI_PROVIDER_POLICY)
        .is_some_and(|value| parse_provider_policy(value).is_err());
    let invalid_omni_intra_threads = parsed
        .get(KEY_OMNI_INTRA_THREADS)
        .is_some_and(|value| parse_omni_intra_threads(value).is_err());
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
        || invalid_omni_device
        || invalid_omni_provider_policy
        || invalid_omni_intra_threads
        || invalid_omni_fgclip_max_patches
    {
        save(workspace_dir, &settings)?;
    }

    Ok(settings)
}

pub fn save(workspace_dir: &Path, settings: &AppSettings) -> Result<()> {
    let env_path = env_path(workspace_dir);
    if let Some(parent) = env_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let existing_text = read_existing_env_text(workspace_dir)?.unwrap_or_default();

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

pub fn validate_omni_intra_threads(value: usize) -> Result<usize> {
    if value == 0 {
        bail!("OMNI_INTRA_THREADS 必须大于 0");
    }
    Ok(value)
}

fn parse_runtime_device(value: &str) -> Result<RuntimeDevice> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(RuntimeDevice::Auto),
        "cpu" => Ok(RuntimeDevice::Cpu),
        "gpu" => Ok(RuntimeDevice::Gpu),
        _ => bail!("OMNI_DEVICE 必须是 auto、cpu 或 gpu"),
    }
}

fn parse_provider_policy(value: &str) -> Result<ProviderPolicy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(ProviderPolicy::Auto),
        "interactive" => Ok(ProviderPolicy::Interactive),
        "service" => Ok(ProviderPolicy::Service),
        _ => bail!("OMNI_PROVIDER_POLICY 必须是 auto、interactive 或 service"),
    }
}

fn parse_omni_intra_threads(value: &str) -> Result<OmniIntraThreads> {
    OmniIntraThreads::from_str(value)
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
    let root_env_path = workspace_dir.join(ENV_FILE_NAME);
    if root_env_path.is_file() {
        root_env_path
    } else {
        config_dir_path(workspace_dir).join(ENV_FILE_NAME)
    }
}

fn config_dir_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(CONFIG_DIR_NAME)
}

fn ensure_default_layout(workspace_dir: &Path) -> Result<()> {
    let defaults = AppSettings::defaults();
    for relative_path in [&defaults.model_path, &defaults.log_dir] {
        let path = resolve_path(workspace_dir, relative_path);
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

fn read_existing_env_text(workspace_dir: &Path) -> Result<Option<String>> {
    let env_path = env_path(workspace_dir);
    match fs::read_to_string(&env_path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", env_path.display())),
    }
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
        KEY_OMNI_DEVICE,
        KEY_OMNI_PROVIDER_POLICY,
        KEY_OMNI_INTRA_THREADS,
        KEY_OMNI_FGCLIP_MAX_PATCHES,
        KEY_HOST,
        KEY_PORT,
        KEY_ASSET_DIR,
        KEY_LOG_DIR,
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
        KEY_OMNI_DEVICE => Some(render_string_assignment(
            KEY_OMNI_DEVICE,
            &settings.omni_device.to_string(),
        )),
        KEY_OMNI_PROVIDER_POLICY => Some(render_string_assignment(
            KEY_OMNI_PROVIDER_POLICY,
            &settings.omni_provider_policy.to_string(),
        )),
        KEY_OMNI_INTRA_THREADS => Some(format!(
            "{KEY_OMNI_INTRA_THREADS}={}",
            settings.omni_intra_threads.as_env_value()
        )),
        KEY_OMNI_FGCLIP_MAX_PATCHES => Some(format!(
            "{KEY_OMNI_FGCLIP_MAX_PATCHES}={}",
            settings.omni_fgclip_max_patches
        )),
        KEY_HOST => Some(render_string_assignment(KEY_HOST, &settings.host)),
        KEY_PORT => Some(format!("{KEY_PORT}={}", settings.port)),
        KEY_ASSET_DIR => Some(render_string_assignment(KEY_ASSET_DIR, &settings.asset_dir)),
        KEY_LOG_DIR => Some(render_string_assignment(KEY_LOG_DIR, &settings.log_dir)),
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

    use omni_search::{ProviderPolicy, RuntimeDevice};

    use super::{
        OmniIntraThreads, default_asset_dir, env_path, load_or_create, quote_env_value,
        resolve_path, save, validate_db_path,
    };

    #[test]
    fn resolve_path_normalizes_relative_segments() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let left = resolve_path(&workspace_dir, "./images");
        let right = resolve_path(&workspace_dir, "images");

        assert_eq!(left, right);

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn validate_db_path_preserves_relative_input() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let validated = validate_db_path(&workspace_dir, "./data/app.db").unwrap();

        assert_eq!(validated, "./data/app.db");

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn load_or_create_writes_env_under_config_directory() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let settings = load_or_create(&workspace_dir).unwrap();
        let env_text = fs::read_to_string(env_path(&workspace_dir)).unwrap();

        assert_eq!(settings.db_path, "./config/puppy_find.db");
        assert_eq!(settings.model_path, "./config/model");
        assert_eq!(settings.asset_dir, default_asset_dir());
        assert_eq!(settings.log_dir, "./config/log");
        assert_eq!(settings.omni_device, RuntimeDevice::Auto);
        assert_eq!(settings.omni_provider_policy, ProviderPolicy::Interactive);
        assert_eq!(settings.omni_intra_threads, OmniIntraThreads::Auto);
        assert!(workspace_dir.join("config").join("model").is_dir());
        assert!(workspace_dir.join("config").join("log").is_dir());
        assert!(!workspace_dir.join("materials").exists());
        assert_eq!(
            env_path(&workspace_dir),
            workspace_dir.join("config").join(".env")
        );
        assert!(env_text.contains("DB_PATH=\"./config/puppy_find.db\""));
        assert!(env_text.contains("MODEL_PATH=\"./config/model\""));
        assert!(env_text.contains("OMNI_DEVICE=\"auto\""));
        assert!(env_text.contains("OMNI_PROVIDER_POLICY=\"interactive\""));
        assert!(env_text.contains(&format!(
            "ASSET_DIR={}",
            quote_env_value(&settings.asset_dir)
        )));
        assert!(env_text.contains("LOG_DIR=\"./config/log\""));

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn load_or_create_prefers_root_env_when_present() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();
        fs::write(
            workspace_dir.join(".env"),
            "DB_PATH=\"./legacy.db\"\nMODEL_PATH=\"./legacy-model\"\nOMNI_DEVICE=\"gpu\"\nOMNI_PROVIDER_POLICY=\"service\"\nOMNI_INTRA_THREADS=auto\nOMNI_FGCLIP_MAX_PATCHES=576\nHOST=\"0.0.0.0\"\nPORT=4000\nASSET_DIR=\"./legacy-assets\"\nLOG_DIR=\"./legacy-log\"\n",
        )
        .unwrap();

        let settings = load_or_create(&workspace_dir).unwrap();
        let root_env = fs::read_to_string(env_path(&workspace_dir)).unwrap();

        assert_eq!(settings.db_path, "./legacy.db");
        assert_eq!(settings.model_path, "./legacy-model");
        assert_eq!(settings.omni_device, RuntimeDevice::Gpu);
        assert_eq!(settings.omni_provider_policy, ProviderPolicy::Service);
        assert_eq!(settings.omni_intra_threads, OmniIntraThreads::Auto);
        assert_eq!(settings.asset_dir, "./legacy-assets");
        assert_eq!(settings.log_dir, "./legacy-log");
        assert_eq!(env_path(&workspace_dir), workspace_dir.join(".env"));
        assert!(root_env.contains("DB_PATH=\"./legacy.db\""));
        assert!(root_env.contains("OMNI_DEVICE=\"gpu\""));
        assert!(root_env.contains("OMNI_PROVIDER_POLICY=\"service\""));
        assert!(root_env.contains("OMNI_INTRA_THREADS=auto"));
        assert!(root_env.contains("LOG_DIR=\"./legacy-log\""));
        assert!(!workspace_dir.join("config").join(".env").exists());

        let _ = fs::remove_dir_all(&workspace_dir);
    }

    #[test]
    fn save_preserves_auto_intra_threads_literal() {
        let workspace_dir = unique_test_dir();
        fs::create_dir_all(&workspace_dir).unwrap();

        let mut settings = load_or_create(&workspace_dir).unwrap();
        settings.omni_intra_threads = OmniIntraThreads::Auto;
        save(&workspace_dir, &settings).unwrap();

        let env_text = fs::read_to_string(env_path(&workspace_dir)).unwrap();

        assert!(env_text.contains("OMNI_INTRA_THREADS=auto"));

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
