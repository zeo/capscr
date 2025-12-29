#![allow(dead_code)]

use image::RgbaImage;
use std::path::PathBuf;
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

pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
    enabled: bool,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            enabled: true,
        }
    }

    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        let mut plugin = plugin;
        plugin.on_load();
        self.plugins.push(plugin);
    }

    pub fn unregister(&mut self, name: &str) {
        if let Some(pos) = self.plugins.iter().position(|p| p.name() == name) {
            let mut plugin = self.plugins.remove(pos);
            plugin.on_unload();
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

        let mut current_image: Option<Arc<RgbaImage>> = None;

        for plugin in &mut self.plugins {
            let response = plugin.on_event(event);
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

    pub fn list(&self) -> Vec<(&str, &str, &str)> {
        self.plugins
            .iter()
            .map(|p| (p.name(), p.version(), p.description()))
            .collect()
    }

    pub fn count(&self) -> usize {
        self.plugins.len()
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        for plugin in &mut self.plugins {
            plugin.on_unload();
        }
    }
}
