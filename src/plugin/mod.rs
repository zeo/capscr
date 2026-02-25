#![allow(dead_code)]

mod loader;
mod manifest;
#[cfg(feature = "wasm-plugins")]
mod wasm_runtime;

pub use loader::PluginLoader;
pub use manifest::{PluginManifest, PluginType};
#[cfg(feature = "wasm-plugins")]
pub use wasm_runtime::WasmPlugin;

use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum PluginEvent {
    PreCapture {
        mode: CaptureType,
    },
    PostCapture {
        image: Arc<RgbaImage>,
        mode: CaptureType,
    },
    PreSave {
        image: Arc<RgbaImage>,
        path: PathBuf,
    },
    PostSave {
        path: PathBuf,
    },
    PreUpload {
        image: Arc<RgbaImage>,
    },
    PostUpload {
        url: String,
    },
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

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn description(&self) -> &str;

    fn on_event(&mut self, event: &PluginEvent) -> PluginResponse {
        let _ = event;
        PluginResponse::Continue
    }

    fn on_load(&mut self) {}
    fn on_unload(&mut self) {}
}

pub type CreatePluginFn = fn() -> Box<dyn Plugin>;

pub enum PluginHandle {
    Native {
        plugin: Box<dyn Plugin>,
        _library: libloading::Library,
    },
    #[cfg(feature = "wasm-plugins")]
    Wasm {
        plugin: WasmPlugin,
    },
    #[cfg(test)]
    Test {
        plugin: Box<dyn Plugin>,
    },
}

pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub handle: PluginHandle,
}

struct DiscoveredPlugin {
    manifest: PluginManifest,
    directory: PathBuf,
}

pub struct PluginManager {
    plugins: Vec<LoadedPlugin>,
    discovered: Vec<DiscoveredPlugin>,
    enabled: bool,
    lazy_loading: bool,
    plugins_dir: PathBuf,
}

impl PluginManager {
    pub fn new() -> Self {
        let plugins_dir = directories::ProjectDirs::from("", "", "capscr")
            .map(|d| d.config_dir().join("plugins"))
            .unwrap_or_else(|| PathBuf::from("plugins"));

        Self {
            plugins: Vec::new(),
            discovered: Vec::new(),
            enabled: true,
            lazy_loading: true,
            plugins_dir,
        }
    }

    pub fn with_plugins_dir(plugins_dir: PathBuf) -> Self {
        Self {
            plugins: Vec::new(),
            discovered: Vec::new(),
            enabled: true,
            lazy_loading: true,
            plugins_dir,
        }
    }

    pub fn plugins_dir(&self) -> &PathBuf {
        &self.plugins_dir
    }

    pub fn set_lazy_loading(&mut self, lazy_loading: bool) {
        self.lazy_loading = lazy_loading;
    }

    pub fn is_lazy_loading(&self) -> bool {
        self.lazy_loading
    }

    fn discover_from_directory(&mut self, dir: &Path) -> Result<(), String> {
        let manifest = PluginManifest::from_directory(dir)?;
        manifest.validate()?;
        manifest.is_compatible()?;
        let library_name = manifest
            .library_filename()
            .ok_or_else(|| "No library specified for current platform".to_string())?;
        let library_path = dir.join(library_name);
        if !library_path.exists() {
            return Err(format!(
                "Library file not found: {}",
                library_path.display()
            ));
        }
        let plugin_id = manifest.plugin.id.clone();
        if self
            .plugins
            .iter()
            .any(|loaded| loaded.manifest.plugin.id == plugin_id)
            || self
                .discovered
                .iter()
                .any(|pending| pending.manifest.plugin.id == plugin_id)
        {
            return Err(format!("Plugin with id '{}' is already loaded", plugin_id));
        }
        self.discovered.push(DiscoveredPlugin {
            manifest,
            directory: dir.to_path_buf(),
        });
        Ok(())
    }

    pub fn load_all(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        self.discovered.clear();

        if !self.plugins_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&self.plugins_dir) {
                errors.push(format!("Failed to create plugins directory: {}", e));
                return errors;
            }
        }

        let entries = match std::fs::read_dir(&self.plugins_dir) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("Failed to read plugins directory: {}", e));
                return errors;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                let result = if self.lazy_loading {
                    self.discover_from_directory(&path)
                } else {
                    self.load_from_directory(&path)
                };
                if let Err(e) = result {
                    errors.push(format!("{}: {}", path.display(), e));
                }
            } else if path.extension().is_some_and(|ext| ext == "zip") {
                let loader = PluginLoader::new(self.plugins_dir.clone());
                match loader.install_from_zip(&path) {
                    Ok(plugin_dir) => {
                        let result = if self.lazy_loading {
                            self.discover_from_directory(&plugin_dir)
                        } else {
                            self.load_from_directory(&plugin_dir)
                        };
                        if let Err(e) = result {
                            errors.push(format!("{}: {}", plugin_dir.display(), e));
                        }
                    }
                    Err(e) => errors.push(format!("{}: {}", path.display(), e)),
                }
            }
        }

        if !self.lazy_loading {
            errors.extend(self.load_pending());
        }

        errors
    }

    pub fn load_pending(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        let pending = std::mem::take(&mut self.discovered);
        for discovered in pending {
            if self
                .plugins
                .iter()
                .any(|p| p.manifest.plugin.id == discovered.manifest.plugin.id)
            {
                continue;
            }

            if let Err(e) = self.load_from_directory(&discovered.directory) {
                errors.push(format!("{}: {}", discovered.directory.display(), e));
            }
        }
        errors
    }

    pub fn install_from_zip(&mut self, zip_path: &PathBuf) -> Result<(), String> {
        let loader = PluginLoader::new(self.plugins_dir.clone());
        let plugin_dir = loader.install_from_zip(zip_path)?;
        self.load_from_directory(&plugin_dir)
    }

    pub fn load_from_directory(&mut self, dir: &Path) -> Result<(), String> {
        let loader = PluginLoader::new(self.plugins_dir.clone());
        let mut loaded = loader.load_from_directory(dir)?;
        let plugin_id = loaded.manifest.plugin.id.clone();

        if self
            .plugins
            .iter()
            .any(|existing| existing.manifest.plugin.id == plugin_id)
        {
            return Err(format!("Plugin with id '{}' is already loaded", plugin_id));
        }

        match &mut loaded.handle {
            PluginHandle::Native { plugin, .. } => plugin.on_load(),
            #[cfg(feature = "wasm-plugins")]
            PluginHandle::Wasm { plugin } => plugin.on_load(),
            #[cfg(test)]
            PluginHandle::Test { plugin } => plugin.on_load(),
        }

        self.discovered
            .retain(|pending| pending.manifest.plugin.id != plugin_id);
        self.plugins.push(loaded);
        Ok(())
    }

    pub fn unload(&mut self, plugin_id: &str) -> bool {
        self.discovered
            .retain(|pending| pending.manifest.plugin.id != plugin_id);
        if let Some(pos) = self
            .plugins
            .iter()
            .position(|p| p.manifest.plugin.id == plugin_id)
        {
            let mut loaded = self.plugins.remove(pos);
            match &mut loaded.handle {
                PluginHandle::Native { plugin, .. } => plugin.on_unload(),
                #[cfg(feature = "wasm-plugins")]
                PluginHandle::Wasm { plugin } => plugin.on_unload(),
                #[cfg(test)]
                PluginHandle::Test { plugin } => plugin.on_unload(),
            }
            true
        } else {
            false
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn dispatch(&mut self, event: &PluginEvent) -> PluginResponse {
        if !self.enabled {
            return PluginResponse::Continue;
        }

        if self.lazy_loading {
            for err in self.load_pending() {
                tracing::warn!("Plugin load error: {}", err);
            }
        }

        let mut current_image: Option<Arc<RgbaImage>> = None;

        for loaded in &mut self.plugins {
            let response = match &mut loaded.handle {
                PluginHandle::Native { plugin, .. } => plugin.on_event(event),
                #[cfg(feature = "wasm-plugins")]
                PluginHandle::Wasm { plugin } => plugin.on_event(event),
                #[cfg(test)]
                PluginHandle::Test { plugin } => plugin.on_event(event),
            };
            match response {
                PluginResponse::Cancel => return PluginResponse::Cancel,
                PluginResponse::ModifiedImage(img) => {
                    current_image = Some(img);
                }
                PluginResponse::Continue => {}
            }
        }

        if let Some(img) = current_image {
            PluginResponse::ModifiedImage(img)
        } else {
            PluginResponse::Continue
        }
    }

    pub fn list(&self) -> Vec<&PluginManifest> {
        let mut manifests: Vec<&PluginManifest> =
            self.plugins.iter().map(|p| &p.manifest).collect();
        manifests.extend(self.discovered.iter().map(|p| &p.manifest));
        manifests
    }

    pub fn count(&self) -> usize {
        self.plugins.len() + self.discovered.len()
    }

    pub fn get(&self, plugin_id: &str) -> Option<&PluginManifest> {
        if let Some(manifest) = self
            .plugins
            .iter()
            .find(|p| p.manifest.plugin.id == plugin_id)
            .map(|p| &p.manifest)
        {
            return Some(manifest);
        }

        self.discovered
            .iter()
            .find(|p| p.manifest.plugin.id == plugin_id)
            .map(|p| &p.manifest)
    }

    #[cfg(test)]
    fn add_test_plugin(&mut self, plugin_id: &str, plugin: Box<dyn Plugin>) {
        let manifest = PluginManifest {
            plugin: manifest::PluginInfo {
                id: plugin_id.to_string(),
                name: plugin.name().to_string(),
                version: plugin.version().to_string(),
                author: "test".to_string(),
                description: plugin.description().to_string(),
                license: None,
                website: None,
                repository: None,
            },
            compatibility: manifest::PluginCompatibility {
                capscr: ">=0.1.0".to_string(),
                platforms: vec!["windows".to_string(), "linux".to_string()],
            },
            library: manifest::PluginLibrary {
                wasm: Some("test.wasm".to_string()),
                windows: None,
                linux: None,
            },
        };

        self.plugins.push(LoadedPlugin {
            manifest,
            handle: PluginHandle::Test { plugin },
        });
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        for loaded in &mut self.plugins {
            match &mut loaded.handle {
                PluginHandle::Native { plugin, .. } => plugin.on_unload(),
                #[cfg(feature = "wasm-plugins")]
                PluginHandle::Wasm { plugin } => plugin.on_unload(),
                #[cfg(test)]
                PluginHandle::Test { plugin } => plugin.on_unload(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ContinuePlugin;

    impl Plugin for ContinuePlugin {
        fn name(&self) -> &str {
            "continue"
        }

        fn version(&self) -> &str {
            "1.0.0"
        }

        fn description(&self) -> &str {
            "continue"
        }
    }

    struct CancelPlugin;

    impl Plugin for CancelPlugin {
        fn name(&self) -> &str {
            "cancel"
        }

        fn version(&self) -> &str {
            "1.0.0"
        }

        fn description(&self) -> &str {
            "cancel"
        }

        fn on_event(&mut self, _event: &PluginEvent) -> PluginResponse {
            PluginResponse::Cancel
        }
    }

    struct ModifyPlugin;

    impl Plugin for ModifyPlugin {
        fn name(&self) -> &str {
            "modify"
        }

        fn version(&self) -> &str {
            "1.0.0"
        }

        fn description(&self) -> &str {
            "modify"
        }

        fn on_event(&mut self, _event: &PluginEvent) -> PluginResponse {
            let mut image = RgbaImage::new(1, 1);
            image.put_pixel(0, 0, image::Rgba([10, 20, 30, 255]));
            PluginResponse::ModifiedImage(Arc::new(image))
        }
    }

    fn post_capture_event() -> PluginEvent {
        PluginEvent::PostCapture {
            image: Arc::new(RgbaImage::new(1, 1)),
            mode: CaptureType::FullScreen,
        }
    }

    #[test]
    fn test_dispatch_continue() {
        let mut manager = PluginManager::new();
        manager.add_test_plugin("continue", Box::new(ContinuePlugin));
        let response = manager.dispatch(&post_capture_event());
        assert!(matches!(response, PluginResponse::Continue));
    }

    #[test]
    fn test_dispatch_modified_image() {
        let mut manager = PluginManager::new();
        manager.add_test_plugin("modify", Box::new(ModifyPlugin));
        let response = manager.dispatch(&post_capture_event());
        match response {
            PluginResponse::ModifiedImage(img) => {
                assert_eq!(img.get_pixel(0, 0).0, [10, 20, 30, 255]);
            }
            _ => panic!("expected modified image"),
        }
    }

    #[test]
    fn test_dispatch_cancel() {
        let mut manager = PluginManager::new();
        manager.add_test_plugin("cancel", Box::new(CancelPlugin));
        let response = manager.dispatch(&post_capture_event());
        assert!(matches!(response, PluginResponse::Cancel));
    }
}
