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
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const REGISTRY_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const PLUGIN_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REGISTRY_BYTES: usize = 2 * 1024 * 1024; // 2 MB — generous for a few hundred plugins
const MAX_PLUGIN_ZIP_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
const MAX_PLUGIN_FILES: usize = 256;
const MAX_PLUGIN_FILE_BYTES: u64 = 16 * 1024 * 1024; // per-file cap inside the zip
const MAX_PLUGIN_TOTAL_BYTES: u64 = 200 * 1024 * 1024; // total extracted cap
const REGISTRY_SCHEMA_VERSION: u32 = 1;
const MAX_REDIRECTS: usize = 5;
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
    let resp = client.get(registry_url).send()?;
    if !resp.status().is_success() {
        bail!("registry fetch failed: HTTP {}", resp.status());
    }
    let mut bytes = Vec::new();
    resp.take(MAX_REGISTRY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
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

/// installs the plugin and returns true if it was left disabled pending the
/// user's review of its declared capabilities
pub fn install_plugin(plugins_dir: &Path, entry: &RegistryEntry) -> Result<bool> {
    validate_id(&entry.id)?;
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

    let mut hasher = Sha256::new();
    hasher.update(&buf);
    let got = hex::encode(hasher.finalize());
    let want = entry.sha256.trim().to_lowercase();
    if got != want {
        bail!("sha256 mismatch — got {}, registry expected {}", got, want);
    }

    // stage into a temp dir, then atomic-rename into place. If anything
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

    let mut total_extracted: u64 = 0;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        if let Err(error) = validate_archive_name(file.name()) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let raw_name = match file.enclosed_name() {
            Some(n) => n.to_path_buf(),
            None => {
                let _ = std::fs::remove_dir_all(&staging);
                bail!("zip entry has unsafe path: {}", file.name());
            }
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
            let _ = std::fs::remove_dir_all(&staging);
            bail!("zip entry escapes plugin folder: {:?}", raw_name);
        }
        let out_path = staging.join(&raw_name);
        if file.name().ends_with('/') {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        // a cheap early reject on the declared size, then the real enforcement:
        // stream through a byte-limited reader and count what was actually
        // written. size() is the archive's uncompressed_size — attacker
        // controlled — so a deflate bomb can declare a tiny size and expand to
        // gigabytes; only the actual byte count can stop that.
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
        let limit =
            MAX_PLUGIN_FILE_BYTES.min(MAX_PLUGIN_TOTAL_BYTES.saturating_sub(total_extracted));
        let mut out = std::fs::File::create(&out_path)?;
        // copy at most limit+1 bytes; crossing limit means the entry (or the
        // running total) blew the cap regardless of the declared size
        let written = std::io::copy(&mut std::io::Read::take(&mut file, limit + 1), &mut out)?;
        if written > limit {
            let _ = std::fs::remove_dir_all(&staging);
            bail!(
                "plugin zip decompresses past the {}-byte cap",
                MAX_PLUGIN_TOTAL_BYTES
            );
        }
        total_extracted += written;
    }

    // manifest must exist. Without it the listing path won't see the plugin
    // and we'd have a silently broken install.
    let manifest_path = staging.join("plugin.toml");
    if !manifest_path.exists() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("plugin zip missing plugin.toml at the root");
    }

    // consent gate: a plugin that declares capabilities (image read, network
    // fetch, clipboard, notifications) is installed disabled, so its code never
    // runs until the user has seen those capabilities in the plugins tab and
    // enabled it. capability-free plugins install ready to go.
    let needs_review = manifest_declares_capabilities(&manifest_path);
    if needs_review {
        if let Err(e) = force_manifest_disabled(&manifest_path) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    }

    activate_staged_plugin(&final_dir, &staging)?;
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
    let manifest_path = canonical_plugin.join("plugin.toml");
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
        return Err(anyhow!("plugin manifest temp write failed: {error}"));
    }
    if let Err(error) = rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("plugin manifest atomic rename failed: {error}"));
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
        };
        let error = install_plugin(dir.path(), &entry).expect_err("private plugin URL");
        assert!(error.to_string().contains("Private IP"), "got: {error}");
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
