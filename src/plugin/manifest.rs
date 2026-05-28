// plugin.toml schema. Lives at <plugins_dir>/<id>/plugin.toml and tells the
// host how to load + run a plugin. Used by the marketplace install path and
// PluginManager::load_all.

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

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
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSpec {
    /// "wasm" today. "native" reserved for a future Rust-bundled host.
    #[serde(rename = "type")]
    pub runtime_type: String,
    /// path to the artefact, relative to the plugin's dir
    #[serde(default = "default_wasm_file")]
    pub file: String,
    /// per-instantiation memory cap, in bytes. None = wasmtime default
    #[serde(default)]
    pub memory_max_bytes: Option<usize>,
    /// tunes the per-hook epoch deadline (ms). None = ~500ms default. fuel is
    /// always capped regardless of this value
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
        let manifest: PluginManifest = toml::from_str(&raw)
            .map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
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
            // must stay inside it. a string check on '/' alone misses windows
            // drive-absolute paths like `C:/x` (Path::join would then replace
            // the base and load an arbitrary file), so validate the components
            let file = Path::new(&rt.file);
            if rt.file.is_empty()
                || rt.file.contains('\\')
                || file.is_absolute()
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
        Ok(())
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
        }
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
        assert!(!off.enabled, "top-level enabled=false must parse as disabled");
    }

    #[test]
    fn runtime_file_rejects_escapes() {
        assert!(manifest_with_file("").validate().is_err());
        assert!(manifest_with_file("../escape.wasm").validate().is_err());
        assert!(manifest_with_file("/abs.wasm").validate().is_err());
        assert!(manifest_with_file(r"C:\win.wasm").validate().is_err());
        // windows drive-absolute with forward slash — the case the old string
        // check missed. only parses as absolute on windows (this is a
        // windows-only app), so the assertion is gated to match
        #[cfg(windows)]
        assert!(manifest_with_file("C:/win.wasm").validate().is_err());
    }
}
