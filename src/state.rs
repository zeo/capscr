#![allow(dead_code)]

use crate::config::{CaptureTask, Config};
use crate::plugin::PluginManager;
use crate::recording::{GifRecorder, RecordingState};
use crossbeam_channel::Sender;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

const RECENT_UPLOADS_CAP: usize = 5;

pub enum HotkeyCommand {
    Reload { tasks: Vec<CaptureTask> },
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub plugin_manager: Mutex<PluginManager>,
    pub hotkey_tx: Mutex<Option<Sender<HotkeyCommand>>>,
    pub gif_recorder: Mutex<Option<GifRecorder>>,
    pub recording_state: Mutex<RecordingState>,
    pub recording_task_id: Mutex<Option<String>>,
    pub last_save: Mutex<Option<PathBuf>>,
    pub last_upload: Mutex<Option<UploadRecord>>,
    // most-recent first, cap RECENT_UPLOADS_CAP. populated by record_upload.
    // surfaced in the tray's Recent uploads submenu
    pub recent_uploads: Mutex<VecDeque<UploadRecord>>,
    pub editor_image_path: Mutex<Option<String>>,
    // true while a capture pipeline is in flight. Gates new triggers so a
    // user mashing the hotkey while a previous capture is hung doesn't
    // accumulate stalled worker threads.
    pub capture_in_progress: AtomicBool,
    // set true when the user toggles "Disable hotkeys" from the tray. Survives
    // until they toggle back on. Config tasks stay intact; we just unregister
    // them with the OS.
    pub hotkeys_disabled: AtomicBool,
}

#[derive(Clone, Debug)]
pub struct UploadRecord {
    pub url: String,
    pub delete_url: Option<String>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let mut plugin_manager = PluginManager::new();
        plugin_manager.set_lazy_loading(config.performance.lazy_init_plugins);
        for err in plugin_manager.load_all() {
            tracing::warn!("Plugin load error: {}", err);
        }

        Self {
            config: Mutex::new(config),
            plugin_manager: Mutex::new(plugin_manager),
            hotkey_tx: Mutex::new(None),
            gif_recorder: Mutex::new(None),
            recording_state: Mutex::new(RecordingState::Idle),
            recording_task_id: Mutex::new(None),
            last_save: Mutex::new(None),
            last_upload: Mutex::new(None),
            recent_uploads: Mutex::new(VecDeque::with_capacity(RECENT_UPLOADS_CAP)),
            editor_image_path: Mutex::new(None),
            capture_in_progress: AtomicBool::new(false),
            hotkeys_disabled: AtomicBool::new(false),
        }
    }

    pub fn send_hotkey_reload(&self, tasks: Vec<CaptureTask>) {
        if let Some(tx) = self.hotkey_tx.lock().unwrap().as_ref() {
            let _ = tx.send(HotkeyCommand::Reload { tasks });
        }
    }

    /// push a new upload onto the recent-uploads ring (most-recent-first, cap 5)
    /// and also set last_upload for back-compat with the existing copy-last-url
    /// tray path
    pub fn record_upload(&self, record: UploadRecord) {
        *self.last_upload.lock().unwrap() = Some(record.clone());
        let mut recent = self.recent_uploads.lock().unwrap();
        // drop any existing entry with the same url so the new one bubbles to
        // the front without duplicates
        recent.retain(|r| r.url != record.url);
        recent.push_front(record);
        while recent.len() > RECENT_UPLOADS_CAP {
            recent.pop_back();
        }
    }
}
