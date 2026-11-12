// Marketplace client. Fetches a JSON registry, downloads + verifies + extracts
// plugin zips into the per-user plugins directory.
//
// default registry: https://rot.lt/capscr/registry.json (overridable via the
// `marketplace.registry_url` config field).
//
// wire-format contract — what the registry endpoint MUST serve. Documented
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
// each `id` must match `^[a-z0-9][a-z0-9_-]{0,63}$` — used as the on-disk
// folder name, so we reject anything that could escape the plugins dir.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::plugin::{PluginManifest, SettingSpec, MAX_CONFIG_BYTES};

const REGISTRY_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const PLUGIN_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REGISTRY_BYTES: usize = 2 * 1024 * 1024; // 2 MB — generous for a few hundred plugins
const MAX_PLUGIN_ZIP_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
const MAX_PLUGIN_FILES: usize = 256;
const MAX_PLUGIN_FILE_BYTES: u64 = 16 * 1024 * 1024; // per-file cap inside the zip
const MAX_PLUGIN_TOTAL_BYTES: u64 = 200 * 1024 * 1024; // total extracted cap
const MAX_SIGNATURE_BYTES: usize = 4 * 1024;
const REGISTRY_SCHEMA_VERSION: u32 = 1;
const MAX_REDIRECTS: usize = 5;
// minisign key 11771420AA1039A7; rot.lt's deploy signs registry.json with the secret half
const REGISTRY_PUBLIC_KEY: &str = "RWSnORCqIBR3ET9Jgj3WNDgSK4im/Psdq0kO57GGQquALIZdjFeaT6s5";
static PLUGIN_MUTATION_LOCK: Mutex<()> = Mutex::new(());

fn lock_plugin_mutations() -> Result<MutexGuard<'static, ()>> {
    PLUGIN_MUTATION_LOCK
        .lock()
        .map_err(|_| anyhow!("plugin mutation lock poisoned"))
}

fn https_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        if crate::upload::validate_outbound_url(attempt.url().as_str()).is_err() {
            return attempt.error("redirect target blocked");
        }
        attempt.follow()
    })
}

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
    // filled in by fetch_registry from min_capscr_version, never read off the wire
    #[serde(default, skip_deserializing)]
    pub compatible: bool,
}

pub fn fetch_registry(registry_url: &str) -> Result<Registry> {
    if !registry_url.starts_with("https://") {
        bail!("registry URL must be https (got {})", registry_url);
    }
    crate::upload::validate_outbound_url(registry_url)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(REGISTRY_FETCH_TIMEOUT)
        .user_agent(concat!("capscr/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .no_proxy()
        .dns_resolver(crate::upload::ssrf_validating_resolver())
        .redirect(https_redirect_policy())
        .build()?;
    let bytes = fetch_capped(&client, registry_url, MAX_REGISTRY_BYTES, "registry")?;
    let mut signature_url = reqwest::Url::parse(registry_url)?;
    signature_url.set_path(&format!("{}.minisig", signature_url.path()));
    let signature = fetch_capped(
        &client,
        signature_url.as_str(),
        MAX_SIGNATURE_BYTES,
        "registry signature",
    )?;
    verify_registry(
        &bytes,
        &String::from_utf8_lossy(&signature),
        REGISTRY_PUBLIC_KEY,
    )?;
    let mut registry: Registry = serde_json::from_slice(&bytes)?;
    if registry.version != REGISTRY_SCHEMA_VERSION {
        bail!(
            "registry schema version {} unsupported (this build expects {})",
            registry.version,
            REGISTRY_SCHEMA_VERSION
        );
    }
    for entry in &mut registry.plugins {
        validate_id(&entry.id)?;
        entry.compatible = version_at_least(env!("CARGO_PKG_VERSION"), &entry.min_capscr_version);
    }
    Ok(registry)
}

fn fetch_capped(
    client: &reqwest::blocking::Client,
    url: &str,
    cap: usize,
    what: &str,
) -> Result<Vec<u8>> {
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        bail!("{what} fetch failed: HTTP {}", resp.status());
    }
    let mut bytes = Vec::new();
    resp.take(cap as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        bail!("{what} exceeds size cap ({} > {cap} bytes)", bytes.len());
    }
    Ok(bytes)
}

// prehashed signatures only, which minisign 0.12 makes by default
fn verify_registry(bytes: &[u8], signature: &str, public_key: &str) -> Result<()> {
    let public_key = minisign_verify::PublicKey::from_base64(public_key)
        .map_err(|error| anyhow!("registry public key unreadable: {error}"))?;
    minisign_verify::Signature::decode(signature)
        .and_then(|signature| public_key.verify(bytes, &signature, false))
        .map_err(|error| anyhow!("registry signature invalid: {error}"))
}

// dotted numeric compare; missing parts are 0 and any non-numeric part fails
fn version_at_least(current: &str, min: &str) -> bool {
    let min = min.trim();
    if min.is_empty() {
        return true;
    }
    let parse = |version: &str| {
        version
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()
    };
    let (Some(mut current), Some(mut min)) = (parse(current), parse(min)) else {
        return false;
    };
    let len = current.len().max(min.len());
    current.resize(len, 0);
    min.resize(len, 0);
    current >= min
}

/// installs the plugin and returns true if it was left disabled pending the
/// user's review of its declared capabilities
pub fn install_plugin(plugins_dir: &Path, entry: &RegistryEntry) -> Result<bool> {
    validate_id(&entry.id)?;
    if !version_at_least(env!("CARGO_PKG_VERSION"), &entry.min_capscr_version) {
        bail!(
            "{} needs capscr {} or newer",
            entry.name,
            entry.min_capscr_version.trim()
        );
    }
    let _mutation_guard = lock_plugin_mutations()?;
    if !entry.download_url.starts_with("https://") {
        bail!("plugin download_url must be https");
    }
    crate::upload::validate_outbound_url(&entry.download_url)?;
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
        .https_only(true)
        .no_proxy()
        .dns_resolver(crate::upload::ssrf_validating_resolver())
        .redirect(https_redirect_policy())
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

    // stream-read to enforce the cap and compute sha256 in one pass.
    let mut reader = resp.take(MAX_PLUGIN_ZIP_BYTES + 1);
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_PLUGIN_ZIP_BYTES {
        bail!("plugin payload exceeded size cap mid-stream");
    }
    install_from_bytes(plugins_dir, &entry.id, &entry.sha256, &buf)
}

/// removes a staging dir that never got renamed into place
struct StagingDir(PathBuf);

impl Drop for StagingDir {
    fn drop(&mut self) {
        if self.0.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.0) {
                tracing::warn!("could not remove plugin staging dir {:?}: {e}", self.0);
            }
        }
    }
}

fn install_from_bytes(plugins_dir: &Path, id: &str, sha256: &str, buf: &[u8]) -> Result<bool> {
    let mut hasher = Sha256::new();
    hasher.update(buf);
    let got = hex::encode(hasher.finalize());
    let want = sha256.trim().to_lowercase();
    if got != want {
        bail!("sha256 mismatch — got {}, registry expected {}", got, want);
    }

    // stage into a temp dir, then atomic-rename into place. If anything
    // fails we leave the existing install untouched.
    std::fs::create_dir_all(plugins_dir)?;
    let final_dir = plugins_dir.join(id);
    let staging = StagingDir(plugins_dir.join(format!(".staging-{id}")));
    if staging.0.exists() {
        std::fs::remove_dir_all(&staging.0)?;
    }
    std::fs::create_dir_all(&staging.0)?;

    let cursor = std::io::Cursor::new(buf);
    let mut archive = zip::ZipArchive::new(cursor)?;
    if archive.len() > MAX_PLUGIN_FILES {
        bail!(
            "plugin zip has too many files ({} > {})",
            archive.len(),
            MAX_PLUGIN_FILES
        );
    }

    let mut total_extracted: u64 = 0;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name()?.into_owned();
        validate_archive_name(&name)?;
        let Some(raw_name) = file.enclosed_name().map(|n| n.to_path_buf()) else {
            bail!("zip entry has unsafe path: {}", name);
        };
        // defense-in-depth on top of enclosed_name (which already rejects
        // `..` traversal): reject absolute paths and component-level `..`.
        if raw_name.is_absolute()
            || raw_name.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            })
        {
            bail!("zip entry escapes plugin folder: {:?}", raw_name);
        }
        let out_path = staging.0.join(&raw_name);
        if name.ends_with('/') {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        // a cheap early reject on the declared size, then the real enforcement:
        // stream through a byte-limited reader and count what was actually
        // written. size() is the archive's uncompressed_size — attacker
        // controlled — so a deflate bomb can declare a tiny size and expand to
        // gigabytes; only the actual byte count can stop that.
        if file.size() > MAX_PLUGIN_FILE_BYTES {
            bail!(
                "zip entry too large: {:?} ({} bytes)",
                raw_name,
                file.size()
            );
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let limit =
            MAX_PLUGIN_FILE_BYTES.min(MAX_PLUGIN_TOTAL_BYTES.saturating_sub(total_extracted));
        let mut out = std::fs::File::create(&out_path)?;
        // copy at most limit+1 bytes; crossing limit means the entry (or the
        // running total) blew the cap regardless of the declared size
        let written = std::io::copy(&mut std::io::Read::take(&mut file, limit + 1), &mut out)?;
        if written > limit {
            bail!(
                "plugin zip decompresses past the {}-byte cap",
                MAX_PLUGIN_TOTAL_BYTES
            );
        }
        total_extracted += written;
    }

    // manifest must exist. Without it the listing path won't see the plugin
    // and we'd have a silently broken install.
    let manifest_path = staging.0.join("plugin.toml");
    if !manifest_path.exists() {
        bail!("plugin zip missing plugin.toml at the root");
    }

    // consent gate: a plugin that declares capabilities (image read, network
    // fetch, clipboard, notifications) is installed disabled, so its code never
    // runs until the user has seen those capabilities in the plugins tab and
    // enabled it. capability-free plugins install ready to go.
    let needs_review = manifest_declares_capabilities(&manifest_path);
    if needs_review {
        force_manifest_disabled(&manifest_path)?;
    }

    activate_staged_plugin(&final_dir, &staging.0)?;
    Ok(needs_review)
}

fn validate_archive_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('/') || name.contains('\\') {
        bail!("zip entry has a non-portable path: {name:?}");
    }
    let name = name.trim_end_matches('/');
    if name.is_empty() {
        bail!("zip entry has an empty path");
    }
    for component in name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("zip entry has an unsafe path component: {name:?}");
        }
        if component.contains(':') {
            bail!("zip entry has a colon path component: {name:?}");
        }
    }
    Ok(())
}

fn activate_staged_plugin(final_dir: &Path, staging: &Path) -> Result<()> {
    activate_staged_plugin_with(final_dir, staging, |from, to| std::fs::rename(from, to))
}

fn activate_staged_plugin_with(
    final_dir: &Path,
    staging: &Path,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    match std::fs::symlink_metadata(final_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            rename(staging, final_dir).map_err(|e| anyhow!("activating plugin failed: {e}"))?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            bail!("installed plugin path must be a regular directory");
        }
        Ok(_) => {}
    }

    let user_config = final_dir.join("config.toml");
    if std::fs::symlink_metadata(&user_config)
        .is_ok_and(|metadata| metadata.file_type().is_file())
    {
        std::fs::copy(&user_config, staging.join("config.toml"))
            .map_err(|e| anyhow!("preserving plugin config failed: {e}"))?;
    }

    let name = final_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("plugin install path has no valid directory name"))?;
    let backup = final_dir.with_file_name(format!(
        ".backup-{name}-{}",
        uuid::Uuid::new_v4().as_simple()
    ));
    rename(final_dir, &backup).map_err(|e| anyhow!("backing up installed plugin failed: {e}"))?;
    if let Err(activate_error) = rename(staging, final_dir) {
        return match rename(&backup, final_dir) {
            Ok(()) => Err(anyhow!(
                "activating plugin failed: {activate_error}; previous install restored"
            )),
            Err(rollback_error) => Err(anyhow!(
                "activating plugin failed: {activate_error}; restoring previous install failed: \
                 {rollback_error}; previous files remain at {}",
                backup.display()
            )),
        };
    }
    if let Err(error) = std::fs::remove_dir_all(&backup) {
        tracing::warn!(
            "plugin update succeeded but old backup at {} could not be removed: {error}",
            backup.display()
        );
    }
    Ok(())
}

/// true if the plugin's manifest declares a non-empty `[capabilities]` table
fn manifest_declares_capabilities(manifest_path: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(manifest_path) else {
        return false;
    };
    let Ok(table) = toml::from_str::<toml::Table>(&body) else {
        return false;
    };
    table
        .get("capabilities")
        .and_then(|c| c.as_table())
        .map(|t| !t.is_empty())
        .unwrap_or(false)
}

/// rewrite the manifest with enabled=false so the plugin stays inert until the
/// user reviews its capabilities and enables it
fn force_manifest_disabled(manifest_path: &Path) -> Result<()> {
    let body = std::fs::read_to_string(manifest_path)?;
    let mut table: toml::Table = toml::from_str(&body)?;
    table.insert("enabled".to_string(), toml::Value::Boolean(false));
    let new_body = toml::to_string(&table)?;
    std::fs::write(manifest_path, new_body)?;
    Ok(())
}

pub fn uninstall_plugin(plugins_dir: &Path, id: &str) -> Result<()> {
    validate_id(id)?;
    let _mutation_guard = lock_plugin_mutations()?;
    let dir = plugins_dir.join(id);
    match std::fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            bail!("plugin path must be a regular directory");
        }
        Ok(_) => {}
    }
    let canonical = std::fs::canonicalize(&dir)?;
    let parent = std::fs::canonicalize(plugins_dir)?;
    if !canonical.starts_with(&parent) {
        bail!("plugin path escapes plugins dir — refusing");
    }
    std::fs::remove_dir_all(&canonical)?;
    Ok(())
}

pub fn set_plugin_enabled(plugins_dir: &Path, id: &str, enabled: bool) -> Result<()> {
    validate_id(id)?;
    let _mutation_guard = lock_plugin_mutations()?;
    let manifest_path = installed_plugin_dir(plugins_dir, id)?.join("plugin.toml");
    if !std::fs::symlink_metadata(&manifest_path)?
        .file_type()
        .is_file()
    {
        bail!("plugin manifest must be a regular file");
    }
    let body = std::fs::read_to_string(&manifest_path)?;
    let mut table: toml::Table = toml::from_str(&body)?;
    table.insert("enabled".to_string(), toml::Value::Boolean(enabled));
    replace_file_atomically(&manifest_path, toml::to_string(&table)?.as_bytes())?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct PluginSettings {
    pub specs: Vec<SettingSpec>,
    pub values: serde_json::Map<String, serde_json::Value>,
}

pub fn plugin_settings(plugins_dir: &Path, id: &str) -> Result<PluginSettings> {
    validate_id(id)?;
    let dir = installed_plugin_dir(plugins_dir, id)?;
    let specs = PluginManifest::load(&dir)?.settings;
    let values = settings_values(&specs, &read_plugin_config(&dir)?);
    Ok(PluginSettings { specs, values })
}

pub fn set_plugin_settings(
    plugins_dir: &Path,
    id: &str,
    incoming: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    validate_id(id)?;
    let _mutation_guard = lock_plugin_mutations()?;
    let dir = installed_plugin_dir(plugins_dir, id)?;
    let specs = PluginManifest::load(&dir)?.settings;
    let table = merge_settings(&specs, read_plugin_config(&dir)?, incoming)?;
    replace_file_atomically(
        &dir.join("config.toml"),
        toml::to_string(&table)?.as_bytes(),
    )
}

// stored values that still fit their spec, the declared default otherwise
fn settings_values(
    specs: &[SettingSpec],
    config: &toml::Table,
) -> serde_json::Map<String, serde_json::Value> {
    specs
        .iter()
        .map(|spec| {
            let value = config
                .get(&spec.key)
                .filter(|value| spec.accepts(value))
                .cloned()
                .unwrap_or_else(|| spec.default_value());
            let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
            (spec.key.clone(), json)
        })
        .collect()
}

// undeclared incoming keys are dropped; hand-written keys already in config.toml stay
fn merge_settings(
    specs: &[SettingSpec],
    mut config: toml::Table,
    incoming: &serde_json::Map<String, serde_json::Value>,
) -> Result<toml::Table> {
    for spec in specs {
        let Some(value) = incoming.get(&spec.key) else {
            continue;
        };
        let value = match value {
            serde_json::Value::String(text) => Some(toml::Value::String(text.clone())),
            serde_json::Value::Bool(flag) => Some(toml::Value::Boolean(*flag)),
            serde_json::Value::Number(number) => number
                .as_i64()
                .map(toml::Value::Integer)
                .or_else(|| number.as_f64().map(toml::Value::Float)),
            _ => None,
        }
        .filter(|value| spec.accepts(value))
        .ok_or_else(|| anyhow!("'{}' got a value of the wrong type", spec.label))?;
        config.insert(spec.key.clone(), value);
    }
    if toml::to_string(&config)?.len() as u64 > MAX_CONFIG_BYTES {
        bail!("plugin settings exceed {} KiB", MAX_CONFIG_BYTES / 1024);
    }
    Ok(config)
}

fn read_plugin_config(dir: &Path) -> Result<toml::Table> {
    let path = dir.join("config.toml");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("plugin config.toml must be a regular file");
        }
        Ok(metadata) if metadata.len() > MAX_CONFIG_BYTES => {
            bail!("plugin config.toml is over {} KiB", MAX_CONFIG_BYTES / 1024);
        }
        Ok(_) => {}
    }
    let body = std::fs::read_to_string(&path)?;
    toml::from_str(&body).map_err(|error| anyhow!("parsing {}: {error}", path.display()))
}

// the plugin's own directory, refused when missing, a link, or outside the plugins dir
fn installed_plugin_dir(plugins_dir: &Path, id: &str) -> Result<PathBuf> {
    let plugin_dir = plugins_dir.join(id);
    match std::fs::symlink_metadata(&plugin_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("plugin '{id}' not found");
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            bail!("plugin path must be a regular directory");
        }
        Ok(_) => {}
    }
    let canonical_plugin = std::fs::canonicalize(&plugin_dir)?;
    let canonical_plugins = std::fs::canonicalize(plugins_dir)?;
    if !canonical_plugin.starts_with(&canonical_plugins) {
        bail!("plugin path escapes plugins dir");
    }
    Ok(canonical_plugin)
}

fn replace_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    replace_file_atomically_with(path, contents, |from, to| std::fs::rename(from, to))
}

fn replace_file_atomically_with(
    path: &Path,
    contents: &[u8],
    rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<()> {
    let tmp =
        path.with_extension(format!("toml.{}.tmp", uuid::Uuid::new_v4().as_simple()));
    if let Err(error) = std::fs::write(&tmp, contents) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("plugin file temp write failed: {error}"));
    }
    if let Err(error) = rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("plugin file atomic rename failed: {error}"));
    }
    Ok(())
}

pub fn validate_id(id: &str) -> Result<()> {
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

    const PLAIN_MANIFEST: &[u8] = b"name = \"Sample\"\nenabled = true\n";

    fn plugin_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn install_zip(plugins: &Path, zip: &[u8]) -> Result<bool> {
        install_from_bytes(plugins, "sample", &hex::encode(Sha256::digest(zip)), zip)
    }

    fn assert_rejected_cleanly(plugins: &Path, result: Result<bool>, reason: &str) {
        let error = result.expect_err("install should fail").to_string();
        assert!(error.contains(reason), "{error}");
        let leftovers: Vec<_> = std::fs::read_dir(plugins)
            .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn install_rejects_a_sha256_mismatch_before_staging() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let zip = plugin_zip(&[("plugin.toml", PLAIN_MANIFEST)]);
        let result = install_from_bytes(&plugins, "sample", &"0".repeat(64), &zip);
        assert_rejected_cleanly(&plugins, result, "sha256 mismatch");
    }

    #[test]
    fn install_rejects_a_zip_without_a_manifest_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let result = install_zip(&plugins, &plugin_zip(&[("plugin.wasm", b"\0asm")]));
        assert_rejected_cleanly(&plugins, result, "missing plugin.toml");
    }

    #[test]
    fn install_rejects_an_oversized_entry_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let big = vec![0u8; MAX_PLUGIN_FILE_BYTES as usize + 1];
        let zip = plugin_zip(&[("plugin.toml", PLAIN_MANIFEST), ("big.bin", &big)]);
        let result = install_zip(&plugins, &zip);
        assert_rejected_cleanly(&plugins, result, "too large");
    }

    #[test]
    fn install_rejects_a_parent_dir_entry_and_writes_nothing_outside() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let zip = plugin_zip(&[("plugin.toml", PLAIN_MANIFEST), ("../evil.txt", b"x")]);
        let result = install_zip(&plugins, &zip);
        assert_rejected_cleanly(&plugins, result, "unsafe path component");
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn install_leaves_a_plugin_with_capabilities_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let manifest =
            b"name = \"Sample\"\nenabled = true\n[capabilities]\nclipboard = [\"write\"]\n";
        let needs_review =
            install_zip(&plugins, &plugin_zip(&[("plugin.toml", manifest)])).unwrap();

        assert!(needs_review);
        let body = std::fs::read_to_string(plugins.join("sample/plugin.toml")).unwrap();
        let table: toml::Table = toml::from_str(&body).unwrap();
        assert_eq!(table.get("enabled"), Some(&toml::Value::Boolean(false)));
        assert!(!plugins.join(".staging-sample").exists());
    }

    #[test]
    fn install_enables_a_capability_free_plugin_as_shipped() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let needs_review =
            install_zip(&plugins, &plugin_zip(&[("plugin.toml", PLAIN_MANIFEST)])).unwrap();

        assert!(!needs_review);
        let body = std::fs::read(plugins.join("sample/plugin.toml")).unwrap();
        assert_eq!(body, PLAIN_MANIFEST);
        assert!(!plugins.join(".staging-sample").exists());
    }

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
    fn plugin_toggle_replaces_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plugin = dir.path().join("sample");
        std::fs::create_dir(&plugin).expect("plugin dir");
        std::fs::write(
            plugin.join("plugin.toml"),
            "name = \"Sample\"\nenabled = false\n",
        )
        .expect("manifest");

        set_plugin_enabled(dir.path(), "sample", true).expect("enable");

        let body = std::fs::read_to_string(plugin.join("plugin.toml")).expect("read manifest");
        let table: toml::Table = toml::from_str(&body).expect("parse manifest");
        assert_eq!(table.get("enabled"), Some(&toml::Value::Boolean(true)));
    }

    #[test]
    fn failed_manifest_replace_preserves_original_and_removes_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = dir.path().join("plugin.toml");
        std::fs::write(&manifest, b"enabled = false\n").expect("manifest");

        let error = replace_file_atomically_with(&manifest, b"enabled = true\n", |_, _| {
            Err(std::io::Error::other("injected rename failure"))
        })
        .expect_err("rename should fail");

        assert!(error.to_string().contains("atomic rename failed"));
        assert_eq!(
            std::fs::read_to_string(&manifest).expect("read manifest"),
            "enabled = false\n"
        );
        assert!(
            std::fs::read_dir(dir.path())
                .expect("read dir")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }

    #[test]
    fn http_registry_rejected() {
        let err = fetch_registry("http://insecure.example/registry.json").unwrap_err();
        assert!(err.to_string().contains("https"));
    }

    #[test]
    fn marketplace_rejects_private_literal_urls_before_connecting() {
        for url in ["https://127.0.0.2/registry.json", "https://[::1]/plugin.zip"] {
            let error = fetch_registry(url).expect_err(url);
            assert!(error.to_string().contains("Private IP"), "got: {error}");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = RegistryEntry {
            id: "sample".into(),
            name: "Sample".into(),
            version: "1.0.0".into(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            download_url: "https://127.0.0.2/plugin.zip".into(),
            sha256: String::new(),
            size_bytes: 0,
            tags: Vec::new(),
            min_capscr_version: String::new(),
            license: String::new(),
            compatible: true,
        };
        let error = install_plugin(dir.path(), &entry).expect_err("private plugin URL");
        assert!(error.to_string().contains("Private IP"), "got: {error}");

        let future = RegistryEntry {
            min_capscr_version: "999.0".into(),
            ..entry
        };
        let error = install_plugin(dir.path(), &future).expect_err("future min version");
        assert!(
            error.to_string().contains("needs capscr 999.0"),
            "got: {error}"
        );
    }

    // throwaway keypair; the secret half was deleted after signing the fixture
    const TEST_PUBLIC_KEY: &str = "RWQt31Y22MY1vGqs7y5tXct+QUpO8BTP+Y66SsuAGppgjjJgUgzvu1eQ";
    const TEST_REGISTRY: &[u8] = b"{\"version\":1,\"plugins\":[]}\n";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUQt31Y22MY1vMeraPjBsFC8NSwkmQO4Knd8EAbPI/mxgKSefLshqJucFN+R43LJLmeWb3fQbN3DJ3FV10IiTmRcmZMFryuhOQ0=
trusted comment: capscr test fixture
uQLSi5BTRwXMnq7p9yxQRTkKD5xBy2nRZs+4LPfsmMwZED5C73V7DFltcJy5ksjcGpEBLqmBlcUfzchRsxTvDQ==
";

    #[test]
    fn registry_signature_must_match_the_bytes_and_the_key() {
        verify_registry(TEST_REGISTRY, TEST_SIGNATURE, TEST_PUBLIC_KEY).expect("valid");

        let tampered = b"{\"version\":1,\"plugins\":[{}]}\n";
        assert!(verify_registry(tampered, TEST_SIGNATURE, TEST_PUBLIC_KEY).is_err());

        let forged_comment = TEST_SIGNATURE.replace("capscr test fixture", "capscr test fixturE");
        assert!(verify_registry(TEST_REGISTRY, &forged_comment, TEST_PUBLIC_KEY).is_err());

        assert!(verify_registry(TEST_REGISTRY, "not a signature", TEST_PUBLIC_KEY).is_err());
        assert!(verify_registry(TEST_REGISTRY, TEST_SIGNATURE, REGISTRY_PUBLIC_KEY).is_err());
    }

    #[test]
    #[ignore = "fetches the live registry over the network"]
    fn live_registry_signature_verifies() {
        fetch_registry("https://rot.lt/capscr/registry.json").expect("live registry");
    }

    #[test]
    fn min_version_compares_dotted_numbers() {
        for (current, min, expected) in [
            ("0.6.0", "", true),
            ("0.6.0", "  ", true),
            ("0.6.0", "0.5.0", true),
            ("0.6.0", "0.6", true),
            ("0.6.0", "0.6.0.0", true),
            ("0.6.0", "0.6.1", false),
            ("0.6.0", "0.10.0", false),
            ("0.10.0", "0.9.9", true),
            ("1.0.0", "0.99.99", true),
            ("0.6.0", "0.6.0-beta", false),
            ("0.6.0", "v0.6.0", false),
        ] {
            assert_eq!(
                version_at_least(current, min),
                expected,
                "{current} >= {min}"
            );
        }
    }

    #[test]
    fn archive_names_follow_portable_path_rules() {
        for accepted in ["plugin.toml", "assets/icon.png", "assets/nested/"] {
            validate_archive_name(accepted).expect(accepted);
        }
        for rejected in [
            "../plugin.toml",
            "assets/../../plugin.toml",
            r"..\plugin.toml",
            r"assets\..\plugin.toml",
            "C:/plugin.toml",
            r"C:\plugin.toml",
            "/plugin.toml",
            "assets//plugin.toml",
            "plugin.toml:stream",
        ] {
            assert!(
                validate_archive_name(rejected).is_err(),
                "{rejected:?} should be rejected"
            );
        }
    }

    #[test]
    fn update_preserves_user_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let final_dir = dir.path().join("plugin");
        let staging = dir.path().join(".staging-plugin");
        std::fs::create_dir_all(&final_dir).expect("old install");
        std::fs::create_dir_all(&staging).expect("staging");
        std::fs::write(final_dir.join("plugin.wasm"), b"old").expect("old wasm");
        std::fs::write(final_dir.join("config.toml"), b"theme = \"user\"\n").expect("user config");
        std::fs::write(staging.join("plugin.wasm"), b"new").expect("new wasm");
        std::fs::write(staging.join("config.toml"), b"theme = \"default\"\n")
            .expect("default config");

        activate_staged_plugin(&final_dir, &staging).expect("activate");

        assert_eq!(
            std::fs::read(final_dir.join("plugin.wasm")).expect("read wasm"),
            b"new"
        );
        assert_eq!(
            std::fs::read_to_string(final_dir.join("config.toml")).expect("read config"),
            "theme = \"user\"\n"
        );
    }

    #[test]
    fn activation_rejects_a_non_directory_install_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let final_dir = dir.path().join("plugin");
        let staging = dir.path().join(".staging-plugin");
        std::fs::write(&final_dir, b"occupied").expect("occupied path");
        std::fs::create_dir(&staging).expect("staging");

        let error = activate_staged_plugin(&final_dir, &staging)
            .expect_err("non-directory install path should fail");

        assert!(error.to_string().contains("regular directory"));
        assert_eq!(std::fs::read(&final_dir).expect("occupied path"), b"occupied");
        assert!(staging.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn mutations_reject_plugin_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::create_dir(&target).expect("target");
        std::fs::write(target.join("plugin.toml"), "enabled = true\n").expect("manifest");
        symlink(&target, dir.path().join("alias")).expect("symlink");

        assert!(uninstall_plugin(dir.path(), "alias").is_err());
        assert!(set_plugin_enabled(dir.path(), "alias", false).is_err());
        assert!(target.exists());
    }

    #[test]
    fn failed_activation_restores_previous_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let final_dir = dir.path().join("plugin");
        let staging = dir.path().join(".staging-plugin");
        std::fs::create_dir_all(&final_dir).expect("old install");
        std::fs::create_dir_all(&staging).expect("staging");
        std::fs::write(final_dir.join("plugin.wasm"), b"old").expect("old wasm");
        std::fs::write(staging.join("plugin.wasm"), b"new").expect("new wasm");
        let mut calls = 0;

        let error = activate_staged_plugin_with(&final_dir, &staging, |from, to| {
            calls += 1;
            if calls == 2 {
                Err(std::io::Error::other("injected activation failure"))
            } else {
                std::fs::rename(from, to)
            }
        })
        .expect_err("activation must fail");

        assert!(error.to_string().contains("previous install restored"));
        assert_eq!(
            std::fs::read(final_dir.join("plugin.wasm")).expect("read old wasm"),
            b"old"
        );
        assert_eq!(
            std::fs::read(staging.join("plugin.wasm")).expect("read staged wasm"),
            b"new"
        );
    }

    const SETTINGS_MANIFEST: &str = r#"
[plugin]
id = "sample"
name = "Sample"
version = "1.0.0"

[[settings]]
key = "webhook"
label = "Webhook URL"
type = "string"

[[settings]]
key = "loud"
label = "Loud"
type = "bool"
default = true

[[settings]]
key = "size"
label = "Size"
type = "choice"
choices = ["small", "large"]
"#;

    fn sample_specs() -> Vec<SettingSpec> {
        toml::from_str::<PluginManifest>(SETTINGS_MANIFEST)
            .unwrap()
            .settings
    }

    fn json(body: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        body.as_object().unwrap().clone()
    }

    #[test]
    fn merge_drops_undeclared_keys_and_keeps_hand_written_ones() {
        let existing: toml::Table = toml::from_str("extra = 3\nloud = false\n").unwrap();
        let incoming = json(serde_json::json!({ "webhook": "https://x.test/h", "other": 1 }));
        let merged = merge_settings(&sample_specs(), existing, &incoming).unwrap();
        assert_eq!(
            merged.get("webhook").and_then(|v| v.as_str()),
            Some("https://x.test/h")
        );
        assert_eq!(merged.get("extra").and_then(|v| v.as_integer()), Some(3));
        assert_eq!(merged.get("loud").and_then(|v| v.as_bool()), Some(false));
        assert!(!merged.contains_key("other"));
    }

    #[test]
    fn merge_rejects_wrong_types_and_unknown_choices() {
        let specs = sample_specs();
        for bad in [
            serde_json::json!({ "loud": "yes" }),
            serde_json::json!({ "webhook": 5 }),
            serde_json::json!({ "size": "huge" }),
        ] {
            assert!(merge_settings(&specs, toml::Table::new(), &json(bad)).is_err());
        }
    }

    #[test]
    fn merge_refuses_a_config_over_the_size_cap() {
        let huge = "x".repeat(MAX_CONFIG_BYTES as usize);
        let incoming = json(serde_json::json!({ "webhook": huge }));
        assert!(merge_settings(&sample_specs(), toml::Table::new(), &incoming).is_err());
    }

    #[test]
    fn settings_round_trip_through_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("sample");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.toml"), SETTINGS_MANIFEST).unwrap();
        std::fs::write(plugin.join("config.toml"), "loud = \"not a bool\"\n").unwrap();

        let before = plugin_settings(dir.path(), "sample").unwrap();
        assert_eq!(before.values["loud"], serde_json::json!(true));
        assert_eq!(before.values["size"], serde_json::json!("small"));

        let incoming = json(serde_json::json!({ "loud": false, "size": "large" }));
        set_plugin_settings(dir.path(), "sample", &incoming).unwrap();
        let after = plugin_settings(dir.path(), "sample").unwrap();
        assert_eq!(after.values["loud"], serde_json::json!(false));
        assert_eq!(after.values["size"], serde_json::json!("large"));
        assert_eq!(after.values["webhook"], serde_json::json!(""));
        assert!(plugin_settings(dir.path(), "../sample").is_err());
    }

    #[test]
    fn plugin_mutations_are_serialized() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let active = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let active = active.clone();
            let peak = peak.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                let _guard = lock_plugin_mutations().expect("lock");
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().expect("thread");
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }
}
