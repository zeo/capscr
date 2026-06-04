#![allow(dead_code)]

use crate::config::{CaptureTask, Config};
use crate::plugin::PluginManager;
use crate::recording::{GifRecorder, RecordingState};
use crossbeam_channel::Sender;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

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
    // RwLock (not Mutex): dispatch takes &self and only reads, so concurrent
    // dispatches share a read lock — a slow hook can't block another event's
    // dispatch. load_all runs once at startup before this lock is constructed.
    pub plugin_manager: RwLock<PluginManager>,
    // human-readable plugin load failures captured at startup (load_all runs
    // once in new()); surfaced to the plugins tab via the plugin_load_errors
    // command so a broken plugin isn't a silent no-op
    pub plugin_load_errors: Mutex<Vec<String>>,
    // flipped true once the background plugin load (load_plugins) finishes its
    // swap. capture dispatch briefly waits on this so a capture fired right
    // after launch (e.g. a jump-list/hotkey shot) still runs on_capture hooks
    // instead of racing the load and silently skipping every plugin.
    pub plugins_ready: AtomicBool,
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
        // plugins are loaded off-thread after construction (see load_plugins),
        // so the tray icon appears immediately instead of waiting on the
        // cranelift JIT compile of every enabled WASM plugin. until the
        // background load swaps the manager in, dispatch sees zero plugins,
        // which matches the existing no-plugin behaviour.
        let plugin_manager = PluginManager::new();

        let disabled = config.hotkeys.disabled_globally;
        Self {
            config: Mutex::new(config),
            plugin_manager: RwLock::new(plugin_manager),
            plugin_load_errors: Mutex::new(Vec::new()),
            plugins_ready: AtomicBool::new(false),
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

    /// load and instantiate plugins, then atomically swap them into the live
    /// manager. Built off-lock so the (potentially slow) cranelift compile of
    /// each WASM plugin never holds the dispatch read-lock; only the final swap
    /// takes the write lock and it's instant. Intended to run on a background
    /// thread spawned at startup so the tray isn't blocked.
    pub fn load_plugins(&self) {
        let lazy = self.config.lock().unwrap().performance.lazy_init_plugins;
        let mut pm = PluginManager::new();
        pm.set_lazy_loading(lazy);
        let errors = pm.load_all();
        for err in &errors {
            tracing::warn!("Plugin load error: {}", err);
        }
        *self.plugin_load_errors.lock().unwrap() = errors;
        *self.plugin_manager.write().unwrap() = pm;
        self.plugins_ready.store(true, Ordering::SeqCst);
    }

    /// block up to `max` for the background plugin load to finish so a capture
    /// dispatched right after launch still sees the loaded plugins. interactive
    /// captures effectively never wait — the user's region/window selection time
    /// already covers the sub-second load — so this only costs anything for an
    /// instant capture fired during the load window, and it's bounded so a slow
    /// or stuck load can never hang the capture.
    pub fn await_plugins_ready(&self, max: std::time::Duration) {
        if self.plugins_ready.load(Ordering::SeqCst) {
            return;
        }
        let start = std::time::Instant::now();
        while !self.plugins_ready.load(Ordering::SeqCst) {
            if start.elapsed() >= max {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
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
