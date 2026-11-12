// plugin.toml schema. Lives at <plugins_dir>/<id>/plugin.toml and tells the
// host how to load + run a plugin. Used by the marketplace install path and
// PluginManager::load_all.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const MAX_SETTINGS: usize = 32;

#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginMeta,
    #[serde(default)]
    pub runtime: Option<RuntimeSpec>,
    #[serde(default)]
    pub hooks: HashMap<String, String>,
    #[serde(default)]
    pub capabilities: HashMap<String, Vec<String>>,
    /// out-of-band enable flag, a top-level key in plugin.toml written by the
    /// toggle_plugin_enabled command. defaults to true so plugins without the
    /// key load normally; when false the host must not instantiate the plugin
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub settings: Vec<SettingSpec>,
}

// one field on the plugin's settings page, stored as a top-level key in config.toml
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    #[serde(default)]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    String,
    Bool,
    Number,
    Choice,
}

impl SettingSpec {
    pub fn accepts(&self, value: &toml::Value) -> bool {
        match (self.kind, value) {
            (SettingKind::String, toml::Value::String(_))
            | (SettingKind::Bool, toml::Value::Boolean(_))
            | (SettingKind::Number, toml::Value::Integer(_) | toml::Value::Float(_)) => true,
            (SettingKind::Choice, toml::Value::String(choice)) => self.choices.contains(choice),
            _ => false,
        }
    }

    pub fn default_value(&self) -> toml::Value {
        self.default.clone().unwrap_or_else(|| match self.kind {
            SettingKind::String => toml::Value::String(String::new()),
            SettingKind::Bool => toml::Value::Boolean(false),
            SettingKind::Number => toml::Value::Integer(0),
            SettingKind::Choice => toml::Value::String(self.choices[0].clone()),
        })
    }
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub payload: PayloadFormat,
}

// what the notify hooks receive: the bare path or url, or a json object
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayloadFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSpec {
    /// "wasm" today. "native" reserved for a future Rust-bundled host.
    #[serde(rename = "type")]
    pub runtime_type: String,
    /// path to the artefact, relative to the plugin's dir
    #[serde(default = "default_wasm_file")]
    pub file: String,
    /// per-instantiation memory cap, in bytes; the host supplies a bounded default
    #[serde(default)]
    pub memory_max_bytes: Option<usize>,
    /// tunes the per-hook epoch deadline (ms), capped at 2000. None = ~500ms
    /// default. fuel is always capped regardless of this value
    #[serde(default)]
    pub time_slice_ms: Option<u64>,
}

fn default_wasm_file() -> String {
    "plugin.wasm".to_string()
}

impl PluginManifest {
    pub fn load(plugin_dir: &Path) -> Result<Self> {
        let path = plugin_dir.join("plugin.toml");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow!("reading {}: {e}", path.display()))?;
        let manifest: PluginManifest =
            toml::from_str(&raw).map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if self.plugin.id.is_empty() {
            bail!("plugin.id is required");
        }
        if !self
            .plugin
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("plugin.id must be [a-zA-Z0-9_-]+");
        }
        if self.plugin.version.is_empty() {
            bail!("plugin.version is required");
        }
        if let Some(rt) = &self.runtime {
            if rt.runtime_type != "wasm" {
                bail!(
                    "runtime.type '{}' not supported (only 'wasm')",
                    rt.runtime_type
                );
            }
            // runtime.file is joined onto the plugin dir at load time, so it
            // must stay inside it. a windows drive-absolute path like `C:/x`
            // parses as a plain relative path on unix (no Prefix component),
            // so a manifest crafted on/for one OS must be rejected on both —
            // check the drive-letter shape explicitly rather than relying on
            // the host's Path semantics.
            let file = Path::new(&rt.file);
            let has_drive_prefix = rt
                .file
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic())
                && rt.file.as_bytes().get(1) == Some(&b':');
            if rt.file.is_empty()
                || rt.file.contains('\\')
                || file.is_absolute()
                || has_drive_prefix
                || file.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                bail!("runtime.file must be a relative path inside the plugin dir");
            }
        }
        if let Some(patterns) = self.capabilities.get("fetch") {
            for pattern in patterns {
                validate_fetch_pattern(pattern)?;
            }
        }
        validate_settings(&self.settings)
    }

    /// returns true if this plugin should be instantiated by the WASM host.
    /// Plugins without a [runtime] section stay metadata-only (the 0.3.x
    /// behaviour — they appear in the Marketplace tab but execute no code).
    pub fn is_wasm(&self) -> bool {
        self.runtime
            .as_ref()
            .map(|r| r.runtime_type == "wasm")
            .unwrap_or(false)
    }
}

fn validate_settings(settings: &[SettingSpec]) -> Result<()> {
    if settings.len() > MAX_SETTINGS {
        bail!("a plugin declares at most {MAX_SETTINGS} settings");
    }
    let mut seen = HashSet::new();
    for spec in settings {
        let key = &spec.key;
        if key.is_empty()
            || key.len() > 48
            || !key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("setting key '{key}' must match [a-z0-9_]{{1,48}}");
        }
        if !seen.insert(key) {
            bail!("setting key '{key}' is declared twice");
        }
        if spec.kind == SettingKind::Choice && spec.choices.is_empty() {
            bail!("choice setting '{key}' needs at least one choice");
        }
        if spec
            .default
            .as_ref()
            .is_some_and(|value| !spec.accepts(value))
        {
            bail!("setting '{key}' has a default that doesn't match its type");
        }
    }
    Ok(())
}

/// a fetch capability entry must name a concrete https host, optionally with a
/// trailing `*` for a path/subpath wildcard. this forbids catch-all egress like
/// `https://*` (whose prefix "https://" matched every https url), so a plugin's
/// network reach has to be spelled out host by host.
fn validate_fetch_pattern(pattern: &str) -> Result<()> {
    let has_wildcard = pattern.ends_with('*');
    let prefix = pattern.strip_suffix('*').unwrap_or(pattern);
    let host_and_path = prefix
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("fetch pattern '{pattern}' must start with https://"))?;
    let host = host_and_path.split('/').next().unwrap_or("");
    if host.is_empty() || host.starts_with('*') || !host.contains('.') {
        bail!(
            "fetch pattern '{pattern}' must name a concrete host, e.g. https://api.example.com/*"
        );
    }
    // a wildcard directly after the host (https://api.example.com*) prefix-matches
    // https://api.example.com.evil.com, so the displayed capability understates
    // the real reach. require the host to be terminated by a path separator so
    // the wildcard can only widen the path, never the host label
    if has_wildcard && !host_and_path.contains('/') {
        bail!(
            "fetch pattern '{pattern}' must put the wildcard on a path boundary, e.g. https://api.example.com/*"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_file(file: &str) -> PluginManifest {
        PluginManifest {
            plugin: PluginMeta {
                id: "test".into(),
                name: "Test".into(),
                version: "1.0.0".into(),
                author: None,
                description: None,
                payload: PayloadFormat::Text,
            },
            runtime: Some(RuntimeSpec {
                runtime_type: "wasm".into(),
                file: file.into(),
                memory_max_bytes: None,
                time_slice_ms: None,
            }),
            hooks: HashMap::new(),
            capabilities: HashMap::new(),
            enabled: true,
            settings: Vec::new(),
        }
    }

    fn manifest_with_settings(settings: &str) -> Result<PluginManifest> {
        let body = format!("[plugin]\nid=\"x\"\nname=\"X\"\nversion=\"1.0.0\"\n{settings}");
        let manifest: PluginManifest = toml::from_str(&body)?;
        manifest.validate()?;
        Ok(manifest)
    }

    #[test]
    fn settings_parse_with_defaults_and_choices() {
        let manifest = manifest_with_settings(
            r#"
[[settings]]
key = "webhook"
label = "Webhook URL"
type = "string"
secret = true

[[settings]]
key = "size"
label = "Size"
type = "choice"
choices = ["small", "large"]
default = "large"
"#,
        )
        .unwrap();
        let [webhook, size] = &manifest.settings[..] else {
            panic!("expected two settings");
        };
        assert!(webhook.secret);
        assert_eq!(webhook.default_value(), toml::Value::String(String::new()));
        assert_eq!(size.default_value(), toml::Value::String("large".into()));
    }

    #[test]
    fn malformed_settings_fail_the_load() {
        let spec = |body: &str| manifest_with_settings(&format!("[[settings]]\n{body}\n"));
        assert!(spec("key=\"Bad Key\"\nlabel=\"x\"\ntype=\"string\"").is_err());
        assert!(spec("key=\"k\"\nlabel=\"x\"\ntype=\"colour\"").is_err());
        assert!(spec("key=\"k\"\nlabel=\"x\"\ntype=\"choice\"").is_err());
        assert!(spec("key=\"k\"\nlabel=\"x\"\ntype=\"bool\"\ndefault=\"yes\"").is_err());
        assert!(
            spec("key=\"k\"\nlabel=\"x\"\ntype=\"choice\"\nchoices=[\"a\"]\ndefault=\"b\"")
                .is_err()
        );
        assert!(spec("key=\"k\"\nlabel=\"x\"\ntype=\"number\"\ndefault=2.5").is_ok());
        let twice = "[[settings]]\nkey=\"k\"\nlabel=\"x\"\ntype=\"bool\"\n";
        assert!(manifest_with_settings(&twice.repeat(2)).is_err());
        assert!(manifest_with_settings(
            &(0..33)
                .map(|i| format!("[[settings]]\nkey=\"k{i}\"\nlabel=\"x\"\ntype=\"bool\"\n"))
                .collect::<String>()
        )
        .is_err());
    }

    #[test]
    fn payload_format_defaults_to_text_and_refuses_unknown_modes() {
        let parse = |line: &str| {
            let body = format!("[plugin]\nid=\"x\"\nname=\"X\"\nversion=\"1.0.0\"\n{line}");
            toml::from_str::<PluginManifest>(&body).map(|manifest| manifest.plugin.payload)
        };
        assert_eq!(parse("").unwrap(), PayloadFormat::Text);
        assert_eq!(parse("payload = \"json\"").unwrap(), PayloadFormat::Json);
        assert!(parse("payload = \"xml\"").is_err());
    }

    #[test]
    fn runtime_file_accepts_relative() {
        assert!(manifest_with_file("plugin.wasm").validate().is_ok());
        assert!(manifest_with_file("sub/plugin.wasm").validate().is_ok());
    }

    #[test]
    fn enabled_defaults_true_and_parses_top_level_false() {
        let on: PluginManifest =
            toml::from_str("[plugin]\nid=\"x\"\nname=\"X\"\nversion=\"1.0.0\"\n").unwrap();
        assert!(on.enabled, "missing key must default to enabled");
        let off: PluginManifest =
            toml::from_str("enabled=false\n[plugin]\nid=\"x\"\nname=\"X\"\nversion=\"1.0.0\"\n")
                .unwrap();
        assert!(
            !off.enabled,
            "top-level enabled=false must parse as disabled"
        );
    }

    #[test]
    fn runtime_file_rejects_escapes() {
        assert!(manifest_with_file("").validate().is_err());
        assert!(manifest_with_file("../escape.wasm").validate().is_err());
        assert!(manifest_with_file("/abs.wasm").validate().is_err());
        assert!(manifest_with_file(r"C:\win.wasm").validate().is_err());
        // windows drive-absolute with a forward slash parses as relative on
        // unix, so the explicit drive-prefix check rejects it on every OS —
        // a manifest is portable and can't smuggle an absolute path across
        assert!(manifest_with_file("C:/win.wasm").validate().is_err());
    }

    #[test]
    fn fetch_pattern_requires_a_concrete_host() {
        assert!(validate_fetch_pattern("https://api.example.com/*").is_ok());
        assert!(validate_fetch_pattern("https://api.example.com/v1/upload").is_ok());
        assert!(validate_fetch_pattern("https://hooks.slack.com/services/*").is_ok());
        // catch-all egress and non-https must be refused
        assert!(validate_fetch_pattern("https://*").is_err());
        assert!(validate_fetch_pattern("https://").is_err());
        assert!(validate_fetch_pattern("*").is_err());
        assert!(validate_fetch_pattern("http://api.example.com/*").is_err());
        assert!(validate_fetch_pattern("https://localhost/*").is_err());
        // wildcard glued to the host label matches api.example.com.evil.com
        assert!(validate_fetch_pattern("https://api.example.com*").is_err());
        // but a path-prefix wildcard keeps the host bounded and is fine
        assert!(validate_fetch_pattern("https://api.example.com/v1*").is_ok());
    }

    #[test]
    fn manifest_with_catch_all_fetch_fails_validation() {
        let mut m = manifest_with_file("plugin.wasm");
        m.capabilities
            .insert("fetch".into(), vec!["https://*".into()]);
        assert!(m.validate().is_err());
        m.capabilities
            .insert("fetch".into(), vec!["https://api.example.com/*".into()]);
        assert!(m.validate().is_ok());
    }
}
