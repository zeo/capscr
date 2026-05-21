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
    /// per-hook deadline. None = no fuel limit
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
            if rt.file.contains("..") || rt.file.starts_with('/') || rt.file.contains('\\') {
                bail!("runtime.file must be a forward-slash relative path");
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
