// Marketplace client. Fetches a JSON registry, downloads + verifies + extracts
// plugin zips into the per-user plugins directory.
//
// Default registry: https://rot.lt/capscr/registry.json (overridable via the
// `marketplace.registry_url` config field).
//
// Wire-format contract — what the registry endpoint MUST serve. Documented
// here so the server side and the client stay in sync. Bump `version` when
// the shape changes incompatibly.
//
// ```json
// {
//   "version": 1,
//   "updated_unix": 1715990400,
//   "plugins": [
//     {
//       "id": "ocr-tesseract",
//       "name": "OCR (Tesseract)",
//       "version": "1.0.0",
//       "description": "Extract text from captures via Tesseract.",
//       "author": "rot",
//       "homepage": "https://rot.lt/capscr/plugins/ocr-tesseract",
//       "download_url": "https://rot.lt/capscr/plugins/ocr-tesseract-1.0.0.zip",
//       "sha256": "abc123...",
//       "size_bytes": 12345,
//       "tags": ["ocr", "text"],
//       "min_capscr_version": "0.3.28",
//       "license": "MIT"
//     }
//   ]
// }
// ```
//
// Each `id` must match `^[a-z0-9][a-z0-9_-]{0,63}$` — used as the on-disk
// folder name, so we reject anything that could escape the plugins dir.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const REGISTRY_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const PLUGIN_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REGISTRY_BYTES: usize = 2 * 1024 * 1024; // 2 MB — generous for a few hundred plugins
const MAX_PLUGIN_ZIP_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
const MAX_PLUGIN_FILES: usize = 256;
const MAX_PLUGIN_FILE_BYTES: u64 = 16 * 1024 * 1024; // per-file cap inside the zip
const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    pub updated_unix: u64,
    pub plugins: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,
    pub download_url: String,
    pub sha256: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub min_capscr_version: String,
    #[serde(default)]
    pub license: String,
}

pub fn fetch_registry(registry_url: &str) -> Result<Registry> {
    if !registry_url.starts_with("https://") {
        bail!("registry URL must be https (got {})", registry_url);
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(REGISTRY_FETCH_TIMEOUT)
        .user_agent(concat!("capscr/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client.get(registry_url).send()?;
    if !resp.status().is_success() {
        bail!("registry fetch failed: HTTP {}", resp.status());
    }
    let bytes = resp.bytes()?;
    if bytes.len() > MAX_REGISTRY_BYTES {
        bail!(
            "registry exceeds size cap ({} > {} bytes)",
            bytes.len(),
            MAX_REGISTRY_BYTES
        );
    }
    let registry: Registry = serde_json::from_slice(&bytes)?;
    if registry.version != REGISTRY_SCHEMA_VERSION {
        bail!(
            "registry schema version {} unsupported (this build expects {})",
            registry.version,
            REGISTRY_SCHEMA_VERSION
        );
    }
    for entry in &registry.plugins {
        validate_id(&entry.id)?;
    }
    Ok(registry)
}

pub fn install_plugin(plugins_dir: &Path, entry: &RegistryEntry) -> Result<()> {
    validate_id(&entry.id)?;
    if !entry.download_url.starts_with("https://") {
        bail!("plugin download_url must be https");
    }
    if entry.size_bytes > MAX_PLUGIN_ZIP_BYTES {
        bail!(
            "plugin payload exceeds size cap ({} > {})",
            entry.size_bytes,
            MAX_PLUGIN_ZIP_BYTES
        );
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(PLUGIN_DOWNLOAD_TIMEOUT)
        .user_agent(concat!("capscr/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let resp = client.get(&entry.download_url).send()?;
    if !resp.status().is_success() {
        bail!("plugin download failed: HTTP {}", resp.status());
    }
    if let Some(content_length) = resp.content_length() {
        if content_length > MAX_PLUGIN_ZIP_BYTES {
            bail!(
                "plugin payload server-reported size exceeds cap ({} > {})",
                content_length,
                MAX_PLUGIN_ZIP_BYTES
            );
        }
    }

    // Stream-read to enforce the cap and compute sha256 in one pass.
    let mut reader = resp.take(MAX_PLUGIN_ZIP_BYTES + 1);
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_PLUGIN_ZIP_BYTES {
        bail!("plugin payload exceeded size cap mid-stream");
    }

    let mut hasher = Sha256::new();
    hasher.update(&buf);
    let got = hex::encode(hasher.finalize());
    let want = entry.sha256.trim().to_lowercase();
    if got != want {
        bail!(
            "sha256 mismatch — got {}, registry expected {}",
            got,
            want
        );
    }

    // Stage into a temp dir, then atomic-rename into place. If anything
    // fails we leave the existing install untouched.
    std::fs::create_dir_all(plugins_dir)?;
    let final_dir = plugins_dir.join(&entry.id);
    let staging = plugins_dir.join(format!(".staging-{}", entry.id));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)?;

    let cursor = std::io::Cursor::new(&buf);
    let mut archive = zip::ZipArchive::new(cursor)?;
    if archive.len() > MAX_PLUGIN_FILES {
        let _ = std::fs::remove_dir_all(&staging);
        bail!(
            "plugin zip has too many files ({} > {})",
            archive.len(),
            MAX_PLUGIN_FILES
        );
    }

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let raw_name = match file.enclosed_name() {
            Some(n) => n.to_path_buf(),
            None => {
                let _ = std::fs::remove_dir_all(&staging);
                bail!("zip entry has unsafe path: {}", file.name());
            }
        };
        // Defense-in-depth on top of enclosed_name (which already rejects
        // `..` traversal): reject absolute paths and component-level `..`.
        if raw_name.is_absolute()
            || raw_name
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir))
        {
            let _ = std::fs::remove_dir_all(&staging);
            bail!("zip entry escapes plugin folder: {:?}", raw_name);
        }
        let out_path = staging.join(&raw_name);
        if file.name().ends_with('/') {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if file.size() > MAX_PLUGIN_FILE_BYTES {
            let _ = std::fs::remove_dir_all(&staging);
            bail!(
                "zip entry too large: {:?} ({} bytes)",
                raw_name,
                file.size()
            );
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)?;
        std::io::copy(&mut file, &mut out)?;
    }

    // Manifest must exist. Without it the listing path won't see the plugin
    // and we'd have a silently broken install.
    if !staging.join("plugin.toml").exists() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("plugin zip missing plugin.toml at the root");
    }

    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&staging, &final_dir)?;
    Ok(())
}

pub fn uninstall_plugin(plugins_dir: &Path, id: &str) -> Result<()> {
    validate_id(id)?;
    let dir = plugins_dir.join(id);
    if !dir.exists() {
        return Ok(()); // already gone — treat as success
    }
    let canonical = std::fs::canonicalize(&dir)?;
    let parent = std::fs::canonicalize(plugins_dir)?;
    if !canonical.starts_with(&parent) {
        bail!("plugin path escapes plugins dir — refusing");
    }
    std::fs::remove_dir_all(&canonical)?;
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 {
        return Err(anyhow!("invalid plugin id length: {}", id.len()));
    }
    let first = id.chars().next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(anyhow!(
            "plugin id must start with [a-z0-9], got {:?}",
            first
        ));
    }
    for c in id.chars() {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return Err(anyhow!("plugin id has invalid char {:?}", c));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_validation_accepts_normal() {
        validate_id("ocr-tesseract").unwrap();
        validate_id("abc123").unwrap();
        validate_id("a_b_c").unwrap();
    }

    #[test]
    fn id_validation_rejects_dangerous() {
        assert!(validate_id("../etc/passwd").is_err());
        assert!(validate_id("").is_err());
        assert!(validate_id("UPPERCASE").is_err());
        assert!(validate_id("with spaces").is_err());
        assert!(validate_id("-leading-dash").is_err());
        assert!(validate_id(&"x".repeat(65)).is_err());
    }

    #[test]
    fn http_registry_rejected() {
        let err = fetch_registry("http://insecure.example/registry.json").unwrap_err();
        assert!(err.to_string().contains("https"));
    }
}
