#![allow(dead_code, clippy::enum_variant_names)]

mod manifest;
#[cfg(feature = "plugin-runtime")]
mod wasm;

pub use manifest::{PayloadFormat, PluginManifest, SettingSpec};

/// cap on <plugin_dir>/config.toml so a malicious marketplace plugin can't ship
/// a huge config to bloat host memory at load
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum PluginEvent {
    PostCapture {
        image: Arc<RgbaImage>,
        mode: CaptureType,
    },
    PostSave(SaveInfo),
    PostUpload(UploadInfo),
}

// what the saving code knows about a written capture; unknown fields stay None
#[derive(Debug, Clone, Default)]
pub struct SaveInfo {
    pub path: PathBuf,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub task: Option<String>,
    pub duration_ms: Option<u64>,
}

impl SaveInfo {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct UploadInfo {
    pub url: String,
    pub path: Option<PathBuf>,
    pub destination: String,
    pub delete_url: Option<String>,
}

impl PluginEvent {
    /// the hook name plugins must export to subscribe to this event
    fn hook_name(&self) -> &'static str {
        match self {
            PluginEvent::PostCapture { .. } => "on_capture",
            PluginEvent::PostSave(_) => "on_capture_saved",
            PluginEvent::PostUpload(_) => "on_upload_success",
        }
    }

    /// UTF-8 payload for the fire-and-forget notify hooks. PostCapture does not
    /// use this (it takes the binary image blob via dispatch_capture), but the
    /// match stays exhaustive. delete_url only reaches plugins granted upload:delete_url
    fn payload(&self, format: PayloadFormat, delete_url_granted: bool) -> String {
        let lossy = |path: &Path| path.to_string_lossy().into_owned();
        match (self, format) {
            (PluginEvent::PostCapture { mode, .. }, _) => format!("{:?}", mode),
            (PluginEvent::PostSave(save), PayloadFormat::Text) => lossy(&save.path),
            (PluginEvent::PostUpload(upload), PayloadFormat::Text) => upload.url.clone(),
            (PluginEvent::PostSave(save), PayloadFormat::Json) => serde_json::json!({
                "path": lossy(&save.path),
                "kind": save
                    .path
                    .extension()
                    .map(|ext| ext.to_string_lossy().to_ascii_lowercase()),
                "width": save.width,
                "height": save.height,
                "task": save.task,
                "duration_ms": save.duration_ms,
            })
            .to_string(),
            (PluginEvent::PostUpload(upload), PayloadFormat::Json) => {
                let mut body = serde_json::json!({
                    "url": upload.url,
                    "path": upload.path.as_deref().map(lossy),
                    "destination": upload.destination,
                });
                if let Some(delete_url) = upload.delete_url.as_ref().filter(|_| delete_url_granted)
                {
                    body["delete_url"] = delete_url.as_str().into();
                }
                body.to_string()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureType {
    FullScreen,
    Window,
    Region,
    Gif,
}

/// a loaded plugin that takes uploads
#[derive(Debug, Clone, serde::Serialize)]
pub struct UploadPlugin {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum PluginResponse {
    Continue,
    ModifiedImage(Arc<RgbaImage>),
    Cancel,
}

pub struct PluginManager {
    plugins_dir: Option<PathBuf>,
    #[cfg(feature = "plugin-runtime")]
    host: Option<wasm::SharedWasmHost>,
    #[cfg(feature = "plugin-runtime")]
    loaded: Vec<wasm::WasmPlugin>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins_dir: default_plugins_dir(),
            #[cfg(feature = "plugin-runtime")]
            host: None,
            #[cfg(feature = "plugin-runtime")]
            loaded: Vec::new(),
        }
    }

    /// scan the plugins dir, parse every plugin.toml, and (under the
    /// `plugin-runtime` feature) instantiate WASM plugins. Returns a list
    /// of human-readable load errors so the caller can surface them.
    pub fn load_all(&mut self) -> Vec<String> {
        let dir = match &self.plugins_dir {
            Some(d) => d.clone(),
            None => return Vec::new(),
        };
        if !dir.exists() {
            return Vec::new();
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return vec![format!("read plugins dir: {e}")],
        };
        let mut errors = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                continue;
            }
            // skip dotfiles: the marketplace stages installs into `.staging-<id>`
            // dirs, and a crash mid-install could leave one behind — loading it
            // as a real plugin would surface a phantom half-installed entry
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            if let Err(e) = self.load_one(&path) {
                errors.push(format!(
                    "{}: {}",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("<bad name>"),
                    e
                ));
            }
        }
        errors
    }

    #[cfg(not(feature = "plugin-runtime"))]
    fn load_one(&mut self, dir: &Path) -> Result<(), anyhow::Error> {
        // metadata-only: confirm the manifest parses so the marketplace tab
        // can list the plugin, but don't try to execute anything
        let _ = PluginManifest::load(dir)?;
        Ok(())
    }

    #[cfg(feature = "plugin-runtime")]
    fn load_one(&mut self, dir: &Path) -> Result<(), anyhow::Error> {
        let manifest = PluginManifest::load(dir)?;
        if !manifest.enabled {
            tracing::info!(
                "plugin '{}' disabled; not instantiating",
                manifest.plugin.id
            );
            return Ok(()); // user toggled it off — must not execute its code
        }
        if !manifest.is_wasm() {
            return Ok(()); // metadata-only plugin, listed but not executed
        }
        let host = match &self.host {
            Some(h) => h.clone(),
            None => {
                let h = std::sync::Arc::new(wasm::WasmHost::new()?);
                self.host = Some(h.clone());
                h
            }
        };
        let plugin = host.load(dir, &manifest)?;
        self.loaded.push(plugin);
        Ok(())
    }

    // &self (not &mut): dispatch only reads self.loaded; each plugin serialises
    // its own calls via its store mutex. this lets the caller hold a shared
    // (read) lock so a slow hook (e.g. a webhook fetch in on_upload_success)
    // doesn't block a concurrent capture's on_capture dispatch.
    #[cfg(feature = "plugin-runtime")]
    pub fn dispatch(&self, event: &PluginEvent) -> PluginResponse {
        // PostCapture has a richer pipeline (pixels in, cancel/replace out);
        // the notify events are fire-and-forget string payloads
        if let PluginEvent::PostCapture { image, mode } = event {
            return self.dispatch_capture(image, *mode);
        }
        let hook_name = event.hook_name();
        for plugin in &self.loaded {
            let payload = event.payload(plugin.payload, plugin.upload_delete_url);
            if let Err(e) = plugin.call_hook(hook_name, &payload) {
                tracing::warn!("plugin '{}' hook '{}' failed: {e}", plugin.id, hook_name);
            }
        }
        PluginResponse::Continue
    }

    /// thread the captured image through every plugin that subscribes to
    /// on_capture (with image:read): each may continue, cancel, or replace it,
    /// and a replacement feeds the next plugin so filters compose in order
    #[cfg(feature = "plugin-runtime")]
    fn dispatch_capture(&self, image: &Arc<RgbaImage>, mode: CaptureType) -> PluginResponse {
        let mut current = image.clone();
        let mut modified = false;
        for plugin in &self.loaded {
            if !plugin.wants_capture() {
                continue;
            }
            let blob = build_capture_blob(&current, mode);
            match plugin.call_capture_hook(&blob) {
                Ok(wasm::CaptureOutcome::Continue) => {}
                Ok(wasm::CaptureOutcome::Cancel) => return PluginResponse::Cancel,
                Ok(wasm::CaptureOutcome::Modified(img)) => {
                    current = Arc::new(img);
                    modified = true;
                }
                Err(e) => tracing::warn!("plugin '{}' on_capture failed: {e}", plugin.id),
            }
        }
        if modified {
            PluginResponse::ModifiedImage(current)
        } else {
            PluginResponse::Continue
        }
    }

    #[cfg(not(feature = "plugin-runtime"))]
    pub fn dispatch(&self, _event: &PluginEvent) -> PluginResponse {
        PluginResponse::Continue
    }

    #[cfg(feature = "plugin-runtime")]
    pub fn upload_destinations(&self) -> Vec<UploadPlugin> {
        self.loaded
            .iter()
            .filter(|plugin| plugin.is_upload_destination())
            .map(|plugin| UploadPlugin {
                id: plugin.id.clone(),
                name: plugin.name.clone(),
            })
            .collect()
    }

    #[cfg(not(feature = "plugin-runtime"))]
    pub fn upload_destinations(&self) -> Vec<UploadPlugin> {
        Vec::new()
    }

    #[cfg(feature = "plugin-runtime")]
    pub fn upload(
        &self,
        id: &str,
        data: &[u8],
        mime: &str,
        file_name: &str,
    ) -> anyhow::Result<crate::upload::UploadResult> {
        self.loaded
            .iter()
            .find(|plugin| plugin.id == id && plugin.is_upload_destination())
            .ok_or_else(|| anyhow::anyhow!("upload plugin '{id}' is not installed or disabled"))?
            .call_upload_hook(file_name, mime, data)
    }

    #[cfg(not(feature = "plugin-runtime"))]
    pub fn upload(
        &self,
        id: &str,
        _data: &[u8],
        _mime: &str,
        _file_name: &str,
    ) -> anyhow::Result<crate::upload::UploadResult> {
        Err(anyhow::anyhow!(
            "upload plugin '{id}' needs a build with the plugin runtime"
        ))
    }
}

/// pack a capture for the on_capture hook: `[w:u32 LE][h:u32 LE][mode:u32 LE][rgba…]`.
/// mode mirrors CaptureType's discriminants (FullScreen=0, Window=1, Region=2, Gif=3)
#[cfg(feature = "plugin-runtime")]
fn build_capture_blob(image: &RgbaImage, mode: CaptureType) -> Vec<u8> {
    let raw = image.as_raw();
    let mut blob = Vec::with_capacity(12 + raw.len());
    blob.extend_from_slice(&image.width().to_le_bytes());
    blob.extend_from_slice(&image.height().to_le_bytes());
    blob.extend_from_slice(&(mode as u32).to_le_bytes());
    blob.extend_from_slice(raw);
    blob
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

fn default_plugins_dir() -> Option<PathBuf> {
    let pd = crate::config::project_dirs()?;
    Some(pd.data_dir().join("plugins"))
}

#[cfg(all(test, feature = "plugin-runtime"))]
mod tests {
    use super::*;

    fn manager_for(dir: &Path) -> PluginManager {
        let mut pm = PluginManager::new();
        pm.plugins_dir = Some(dir.to_path_buf());
        assert!(pm.load_all().is_empty());
        pm
    }

    fn upload() -> PluginEvent {
        PluginEvent::PostUpload(UploadInfo {
            url: "https://i.example/a.png".into(),
            path: Some(PathBuf::from("shots/a.png")),
            destination: "imgur".into(),
            delete_url: Some("https://i.example/delete/xyz".into()),
        })
    }

    #[test]
    fn text_payloads_stay_the_bare_path_or_url() {
        let save = PluginEvent::PostSave(SaveInfo {
            width: Some(3),
            ..SaveInfo::new(Path::new("shots/a.png"))
        });
        assert_eq!(save.payload(PayloadFormat::Text, true), "shots/a.png");
        assert_eq!(
            upload().payload(PayloadFormat::Text, true),
            "https://i.example/a.png"
        );
    }

    #[test]
    fn json_payloads_carry_the_known_fields() {
        let save = PluginEvent::PostSave(SaveInfo {
            width: Some(640),
            height: Some(480),
            task: Some("gif-quick".into()),
            duration_ms: Some(2500),
            ..SaveInfo::new(Path::new("shots/a.GIF"))
        });
        let body: serde_json::Value =
            serde_json::from_str(&save.payload(PayloadFormat::Json, false)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "path": "shots/a.GIF", "kind": "gif", "width": 640, "height": 480,
                "task": "gif-quick", "duration_ms": 2500,
            })
        );

        let still = PluginEvent::PostSave(SaveInfo::new(Path::new("noext")));
        let body: serde_json::Value =
            serde_json::from_str(&still.payload(PayloadFormat::Json, false)).unwrap();
        assert!(body["kind"].is_null() && body["width"].is_null() && body["task"].is_null());
    }

    #[test]
    fn delete_url_needs_the_capability() {
        let hidden: serde_json::Value =
            serde_json::from_str(&upload().payload(PayloadFormat::Json, false)).unwrap();
        assert_eq!(hidden["destination"], "imgur");
        assert_eq!(hidden["path"], "shots/a.png");
        assert!(hidden.get("delete_url").is_none());
        let shown: serde_json::Value =
            serde_json::from_str(&upload().payload(PayloadFormat::Json, true)).unwrap();
        assert_eq!(shown["delete_url"], "https://i.example/delete/xyz");
    }

    #[test]
    fn reload_drops_a_disabled_plugin() {
        let root = tempfile::tempdir().expect("tempdir");
        let plugin_dir = root.path().join("sample");
        std::fs::create_dir(&plugin_dir).expect("plugin dir");
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "[plugin]\nid = \"sample\"\nname = \"Sample\"\nversion = \"1.0.0\"\n\n[runtime]\ntype = \"wasm\"\n",
        )
        .expect("manifest");
        let wasm = wat::parse_str(
            r#"(module
                 (memory (export "memory") 1)
                 (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024)))"#,
        )
        .expect("wat");
        std::fs::write(plugin_dir.join("plugin.wasm"), wasm).expect("wasm");

        assert_eq!(manager_for(root.path()).loaded.len(), 1);
        crate::marketplace::set_plugin_enabled(root.path(), "sample", false).expect("disable");
        assert_eq!(manager_for(root.path()).loaded.len(), 0);
        crate::marketplace::set_plugin_enabled(root.path(), "sample", true).expect("enable");
        assert_eq!(manager_for(root.path()).loaded.len(), 1);
    }
}
