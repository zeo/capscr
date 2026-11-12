use crate::capture::{Capture, Rectangle, RegionCapture, ScreenCapture, WindowCapture};
use crate::clipboard::{get_unique_filepath, save_image, show_notification, ClipboardManager};
use crate::config::{CaptureTask, Config, TaskCaptureMode, TaskPostAction, UploadDestination};
use crate::overlay::{SelectionResult, UnifiedSelector};
use crate::plugin::{CaptureType, PluginEvent, PluginResponse};
use crate::recording::{GifRecorder, RecordingSettings, RecordingState, StopReason};
use crate::sound::Sound;
use crate::state::{AppState, HotkeyStatus, UploadRecord};
use crate::upload::{CustomUploader, UploadService};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::any::Any;
#[cfg(any(windows, target_os = "linux"))]
use std::io::Read;
use std::path::PathBuf;
#[cfg(any(windows, target_os = "linux"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_window_state::{StateFlags as WindowStateFlags, WindowExt};

mod app;
mod capture;
mod config;
mod editor;
mod history;
mod hotkeys;
mod hub;
mod ocr;
mod pin;
mod plugins;
mod recording;
mod update;
mod upload;

pub use app::*;
pub use capture::*;
pub use config::*;
pub use editor::*;
pub use history::*;
pub use hotkeys::*;
pub use hub::*;
pub use ocr::*;
pub use pin::*;
pub use plugins::*;
pub use recording::*;
pub use update::*;
pub use upload::*;

pub fn history_dir() -> Option<PathBuf> {
    Config::config_dir().map(|d| d.join("history"))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureModeArg {
    Region,
    RegionLast,
    Window,
    Fullscreen,
    ActiveMonitor,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub modified_unix: u64,
    pub is_gif: bool,
    pub is_mp4: bool,
    pub has_hdr: bool,
    pub uploads: Vec<UploadSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadSummary {
    pub url: String,
    pub destination: String,
    pub uploaded_unix: u64,
    pub deletable: bool,
}

pub(crate) fn is_path_allowed(canonical: &std::path::Path, config: &Config) -> bool {
    let roots: Vec<PathBuf> = std::iter::once(config.output.directory.clone())
        .chain(history_dir())
        .collect();
    is_path_within(canonical, &roots)
}

fn is_path_within(canonical: &std::path::Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        std::fs::canonicalize(root).is_ok_and(|root| canonical.starts_with(root))
    })
}

#[cfg(test)]
mod path_guard_tests {
    use super::*;

    #[test]
    fn is_path_within_only_accepts_existing_roots() {
        let base = tempfile::tempdir().unwrap();
        let out = base.path().join("out");
        let sibling = base.path().join("out2");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let inside = std::fs::canonicalize(&out).unwrap().join("shot.png");
        let beside = std::fs::canonicalize(&sibling).unwrap().join("shot.png");

        assert!(is_path_within(&inside, std::slice::from_ref(&out)));
        assert!(!is_path_within(&beside, std::slice::from_ref(&out)));
        assert!(!is_path_within(&inside, &[base.path().join("missing")]));
        assert!(!is_path_within(&inside, &[]));
    }

    #[test]
    fn set_config_keeps_backend_owned_fields() {
        let mut stored = Config::default();
        stored.output.directory = PathBuf::from("/stored/captures");
        stored.ui.grown_windows = vec!["hub".into()];
        let mut draft = Config::default();
        draft.output.directory = PathBuf::from("/somewhere/else");
        draft.output.quality = 42;

        pin_backend_owned(&mut draft, &stored);

        assert_eq!(draft.output.directory, stored.output.directory);
        assert_eq!(draft.ui.grown_windows, stored.ui.grown_windows);
        assert_eq!(draft.output.quality, 42);
    }
}

impl CaptureModeArg {
    pub fn from_task_mode(mode: TaskCaptureMode) -> Self {
        match mode {
            TaskCaptureMode::Region => CaptureModeArg::Region,
            TaskCaptureMode::RegionLast => CaptureModeArg::RegionLast,
            TaskCaptureMode::Window => CaptureModeArg::Window,
            TaskCaptureMode::Fullscreen => CaptureModeArg::Fullscreen,
            TaskCaptureMode::ActiveMonitor => CaptureModeArg::ActiveMonitor,
            TaskCaptureMode::RegionGif | TaskCaptureMode::RegionMp4 => CaptureModeArg::Region,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEventPayload {
    pub kind: String,
    pub msg: String,
}

pub fn emit_error(app: &AppHandle, kind: &str, msg: &str) {
    let _ = app.emit(
        "capscr://error",
        ErrorEventPayload {
            kind: kind.to_string(),
            msg: msg.to_string(),
        },
    );
}

// single funnel for "a capture file was written": notifies the History tab and
// fires the plugin on_capture_saved hook. every save path routes through here so
// the hook can't silently miss a save site
pub fn notify_capture_saved(app: &AppHandle, save: crate::plugin::SaveInfo) {
    let _ = app.emit(
        "capscr://capture-saved",
        save.path.to_string_lossy().to_string(),
    );
    let state = app.state::<AppState>();
    let pm = state.plugin_manager.read().unwrap();
    let _ = pm.dispatch(&PluginEvent::PostSave(save));
}

pub(crate) fn sized_save(
    path: &std::path::Path,
    size: Option<(u32, u32)>,
) -> crate::plugin::SaveInfo {
    crate::plugin::SaveInfo {
        width: size.map(|(width, _)| width),
        height: size.map(|(_, height)| height),
        ..crate::plugin::SaveInfo::new(path)
    }
}

// gifs are scaled on save, so their size comes from the written header
pub(crate) fn recording_save(
    recorder: &crate::recording::GifRecorder,
    path: &std::path::Path,
    task: Option<&str>,
) -> crate::plugin::SaveInfo {
    let (duration, mp4_size) = recorder.saved_details();
    let size = mp4_size.or_else(|| image::image_dimensions(path).ok());
    crate::plugin::SaveInfo {
        task: task.map(str::to_string),
        duration_ms: duration.map(|duration| duration.as_millis() as u64),
        ..sized_save(path, size)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadSuccessPayload {
    pub url: String,
    pub delete_url: Option<String>,
}

pub fn emit_upload_success(app: &AppHandle, upload: crate::plugin::UploadInfo) {
    let _ = app.emit(
        "capscr://upload-success",
        UploadSuccessPayload {
            url: upload.url.clone(),
            delete_url: upload.delete_url.clone(),
        },
    );
    let state = app.state::<AppState>();
    let pm = state.plugin_manager.read().unwrap();
    let _ = pm.dispatch(&PluginEvent::PostUpload(upload));
}
