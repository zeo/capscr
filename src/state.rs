#![allow(dead_code)]

use crate::config::{CaptureTask, Config};
use crate::plugin::PluginManager;
use crate::recording::{GifRecorder, RecordingState};
use crossbeam_channel::Sender;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

const RECENT_UPLOADS_CAP: usize = 5;

pub enum HotkeyCommand {
    Reload { tasks: Vec<CaptureTask> },
}

// per-task hotkey registration status, surfaced in the hub Tasks view + the
// hotkey_diagnostics command. populated by record_hotkey_status() each time
// the hotkey thread flushes a new binding set.
#[derive(Debug, Clone)]
pub enum HotkeyStatus {
    Live,
    Failed { reason: String },
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
    pub recent_uploads: Mutex<VecDeque<UploadRecord>>,
    pub editor_image_path: Mutex<Option<String>>,
    pub capture_in_progress: AtomicBool,
    // user-toggled global kill switch. mirrors config.hotkeys.disabled_globally
    // for in-memory speed; AppState::new restores it from disk so the toggle
    // survives restart.
    pub hotkeys_disabled: AtomicBool,
    // task_id → live/failed status. updated by the hotkey thread after every
    // register/reload pass.
    pub hotkey_status: Mutex<HashMap<String, HotkeyStatus>>,
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

        let disabled = config.hotkeys.disabled_globally;
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
            hotkeys_disabled: AtomicBool::new(disabled),
            hotkey_status: Mutex::new(HashMap::new()),
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
