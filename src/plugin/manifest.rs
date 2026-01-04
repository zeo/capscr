use serde::Deserialize;

const MANIFEST_FILENAME: &str = "manifest.toml";

#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub plugin: PluginInfo,
    pub compatibility: PluginCompatibility,
    pub library: PluginLibrary,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginCompatibility {
    pub capscr: String,
    #[serde(default = "default_platforms")]
    pub platforms: Vec<String>,
}

fn default_platforms() -> Vec<String> {
    vec!["windows".to_string(), "linux".to_string()]
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginLibrary {
    #[serde(default)]
    pub wasm: Option<String>,
    #[serde(default)]
    pub windows: Option<String>,
    #[serde(default)]
    pub linux: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    Wasm,
    Native,
}

impl PluginManifest {
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read manifest: {}", e))?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self, String> {
        toml::from_str(content).map_err(|e| format!("Failed to parse manifest: {}", e))
    }

    pub fn from_directory(dir: &std::path::Path) -> Result<Self, String> {
        let manifest_path = dir.join(MANIFEST_FILENAME);
        if !manifest_path.exists() {
            return Err(format!("No {} found in plugin directory", MANIFEST_FILENAME));
        }
        Self::from_file(&manifest_path)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.plugin.id.is_empty() {
            return Err("Plugin ID cannot be empty".to_string());
        }

        if !self.plugin.id.chars().all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit()) {
            return Err("Plugin ID must be lowercase alphanumeric with hyphens only".to_string());
        }

        if self.plugin.id.len() > 64 {
            return Err("Plugin ID cannot exceed 64 characters".to_string());
        }

        if self.plugin.name.is_empty() {
            return Err("Plugin name cannot be empty".to_string());
        }

        if self.plugin.name.len() > 128 {
            return Err("Plugin name cannot exceed 128 characters".to_string());
        }

        if self.plugin.version.is_empty() {
            return Err("Plugin version cannot be empty".to_string());
        }

        if semver::Version::parse(&self.plugin.version).is_err() {
            return Err("Plugin version must be valid semver (e.g., 1.0.0)".to_string());
        }

        if self.plugin.author.is_empty() {
            return Err("Plugin author cannot be empty".to_string());
        }

        if self.plugin.author.len() > 128 {
            return Err("Plugin author cannot exceed 128 characters".to_string());
        }

        if self.plugin.description.len() > 512 {
            return Err("Plugin description cannot exceed 512 characters".to_string());
        }

        if semver::VersionReq::parse(&self.compatibility.capscr).is_err() {
            return Err("Compatibility version must be valid semver requirement".to_string());
        }

        if self.compatibility.platforms.is_empty() {
            return Err("At least one platform must be specified".to_string());
        }

        for platform in &self.compatibility.platforms {
            if !["windows", "linux"].contains(&platform.as_str()) {
                return Err(format!("Invalid platform: {}", platform));
            }
        }

        Ok(())
    }

    pub fn is_compatible(&self) -> Result<(), String> {
        let current_version = env!("CARGO_PKG_VERSION");
        let current = semver::Version::parse(current_version)
            .map_err(|e| format!("Invalid capscr version: {}", e))?;

        let requirement = semver::VersionReq::parse(&self.compatibility.capscr)
            .map_err(|e| format!("Invalid compatibility requirement: {}", e))?;

        if !requirement.matches(&current) {
            return Err(format!(
                "Plugin requires capscr {}, but {} is installed",
                self.compatibility.capscr, current_version
            ));
        }

        let current_platform = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else {
            "unknown"
        };

        if !self.compatibility.platforms.iter().any(|p| p == current_platform) {
            return Err(format!(
                "Plugin does not support {} (supports: {})",
                current_platform,
                self.compatibility.platforms.join(", ")
            ));
        }

        Ok(())
    }

    pub fn plugin_type(&self) -> PluginType {
        if self.library.wasm.is_some() {
            PluginType::Wasm
        } else {
            PluginType::Native
        }
    }

    pub fn library_filename(&self) -> Option<&str> {
        // Prefer WASM for universal compatibility
        if let Some(ref wasm) = self.library.wasm {
            return Some(wasm);
        }

        // Fall back to native
        if cfg!(windows) {
            self.library.windows.as_deref()
        } else if cfg!(target_os = "linux") {
            self.library.linux.as_deref()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest() {
        let content = r#"
[plugin]
id = "test-plugin"
name = "Test Plugin"
version = "1.0.0"
author = "Test Author"
description = "A test plugin"

[compatibility]
capscr = ">=0.2.0"
platforms = ["windows", "linux"]

[library]
windows = "test_plugin.dll"
linux = "libtest_plugin.so"
"#;

        let manifest = PluginManifest::parse(content).unwrap();
        assert_eq!(manifest.plugin.id, "test-plugin");
        assert_eq!(manifest.plugin.name, "Test Plugin");
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn test_invalid_id() {
        let content = r#"
[plugin]
id = "Test Plugin"
name = "Test"
version = "1.0.0"
author = "Author"
description = ""

[compatibility]
capscr = ">=0.2.0"

[library]
windows = "test.dll"
"#;

        let manifest = PluginManifest::parse(content).unwrap();
        assert!(manifest.validate().is_err());
    }
}
