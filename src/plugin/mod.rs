#![allow(dead_code, clippy::enum_variant_names)]

use image::RgbaImage;
use std::path::PathBuf;
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
    #[allow(dead_code)]
    ModifiedImage(Arc<RgbaImage>),
    #[allow(dead_code)]
    Cancel,
}

pub struct PluginManager {
    lazy_loading: bool,
}

impl PluginManager {
    pub fn new() -> Self {
        Self { lazy_loading: true }
    }

    pub fn set_lazy_loading(&mut self, lazy: bool) {
        self.lazy_loading = lazy;
    }

    pub fn load_all(&mut self) -> Vec<String> {
        Vec::new()
    }

    pub fn dispatch(&mut self, _event: &PluginEvent) -> PluginResponse {
        PluginResponse::Continue
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
