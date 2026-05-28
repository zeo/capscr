#![allow(dead_code, clippy::enum_variant_names)]

mod manifest;
#[cfg(feature = "plugin-runtime")]
mod wasm;

pub use manifest::PluginManifest;

use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum PluginEvent {
    PostCapture {
        image: Arc<RgbaImage>,
        mode: CaptureType,
    },
    PostSave {
        path: PathBuf,
    },
    PostUpload {
        url: String,
    },
}

impl PluginEvent {
    /// the hook name plugins must export to subscribe to this event
    fn hook_name(&self) -> &'static str {
        match self {
            PluginEvent::PostCapture { .. } => "on_capture",
            PluginEvent::PostSave { .. } => "on_capture_saved",
            PluginEvent::PostUpload { .. } => "on_upload_success",
        }
    }

    /// UTF-8 payload for the fire-and-forget notify hooks. PostCapture does not
    /// use this — it takes the binary image blob via dispatch_capture — but the
    /// match stays exhaustive
    fn payload(&self) -> String {
        match self {
            PluginEvent::PostCapture { mode, .. } => format!("{:?}", mode),
            PluginEvent::PostSave { path } => path.to_string_lossy().to_string(),
            PluginEvent::PostUpload { url } => url.clone(),
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

#[derive(Debug, Clone)]
pub enum PluginResponse {
    Continue,
    ModifiedImage(Arc<RgbaImage>),
    Cancel,
}

pub struct PluginManager {
    lazy_loading: bool,
    plugins_dir: Option<PathBuf>,
    #[cfg(feature = "plugin-runtime")]
    host: Option<wasm::SharedWasmHost>,
    #[cfg(feature = "plugin-runtime")]
    loaded: Vec<wasm::WasmPlugin>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            lazy_loading: true,
            plugins_dir: default_plugins_dir(),
            #[cfg(feature = "plugin-runtime")]
            host: None,
            #[cfg(feature = "plugin-runtime")]
            loaded: Vec::new(),
        }
    }

    pub fn set_lazy_loading(&mut self, lazy: bool) {
        self.lazy_loading = lazy;
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
            if !path.is_dir() {
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
            tracing::info!("plugin '{}' disabled; not instantiating", manifest.plugin.id);
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

    #[cfg(feature = "plugin-runtime")]
    pub fn dispatch(&mut self, event: &PluginEvent) -> PluginResponse {
        // PostCapture has a richer pipeline (pixels in, cancel/replace out);
        // the notify events are fire-and-forget string payloads
        if let PluginEvent::PostCapture { image, mode } = event {
            return self.dispatch_capture(image, *mode);
        }
        let hook_name = event.hook_name();
        let payload = event.payload();
        for plugin in &self.loaded {
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
    pub fn dispatch(&mut self, _event: &PluginEvent) -> PluginResponse {
        PluginResponse::Continue
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
    let pd = directories::ProjectDirs::from("com", "capscr", "capscr")?;
    Some(pd.data_dir().join("plugins"))
}
