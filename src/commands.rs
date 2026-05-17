#![allow(dead_code)]

use crate::capture::{Capture, Rectangle, RegionCapture, ScreenCapture, WindowCapture};
use crate::clipboard::{get_unique_filepath, save_image, show_notification, ClipboardManager};
use crate::config::{
    CaptureTask, Config, PostCaptureAction, TaskCaptureMode, TaskPostAction, UploadDestination,
};
use crate::overlay::{RecordingOverlay, SelectionResult, UnifiedSelector};
use crate::plugin::{CaptureType, PluginEvent, PluginResponse};
use crate::recording::{GifRecorder, RecordingSettings, RecordingState};
use crate::sound::Sound;
use crate::state::{AppState, UploadRecord};
use crate::upload::{CustomUploader, FtpTarget, ImageUploader, UploadService};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_opener::OpenerExt;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureModeArg {
    Region,
    Window,
    Fullscreen,
    ActiveMonitor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PostActionArg {
    Clipboard,
    SaveFile,
    Upload,
    SaveAndClipboard,
    OpenEditor,
    Prompt,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub modified_unix: u64,
    pub is_gif: bool,
}

#[tauri::command]
pub fn get_config(state: State<AppState>) -> Result<Config, String> {
    Ok(state.config.lock().unwrap().clone())
}

#[tauri::command]
pub fn set_config(config: Config, app: AppHandle, state: State<AppState>) -> Result<(), String> {
    config.validate().map_err(|e| e.to_string())?;
    config.save().map_err(|e| e.to_string())?;
    crate::install_hdr_runtime_from_config(&config);
    state.send_hotkey_reload(config.capture_tasks.clone());
    let want_autostart = config.ui.auto_start;
    *state.config.lock().unwrap() = config;
    let manager = app.autolaunch();
    let current = manager.is_enabled().unwrap_or(false);
    if current != want_autostart {
        let res = if want_autostart {
            manager.enable()
        } else {
            manager.disable()
        };
        if let Err(e) = res {
            tracing::warn!("autostart toggle failed: {e}");
        }
    }
    Ok(())
}

#[tauri::command]
pub fn take_screenshot(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: AppHandle,
) -> Result<(), String> {
    let app_handle = app;
    std::thread::spawn(move || {
        if let Err(e) = run_capture_pipeline(mode, post, &app_handle) {
            tracing::warn!("capture failed: {e}");
            emit_error(&app_handle, "capture", &e.to_string());
            let state = app_handle.state::<AppState>();
            let show = state.config.lock().unwrap().ui.show_notifications;
            if show {
                let _ = show_notification("Capture failed", &e.to_string());
            }
        }
    });
    Ok(())
}

pub fn run_capture_pipeline(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
) -> anyhow::Result<()> {
    let selection = match mode {
        CaptureModeArg::Region | CaptureModeArg::Window | CaptureModeArg::Fullscreen => {
            UnifiedSelector::select()
        }
        CaptureModeArg::ActiveMonitor => SelectionResult::FullScreen,
    };

    let image = match selection {
        SelectionResult::Cancelled => return Ok(()),
        SelectionResult::Region(rect) => RegionCapture::new(rect).capture()?,
        SelectionResult::Window(hwnd) => WindowCapture::new(hwnd).capture()?,
        SelectionResult::FullScreen => capture_active_monitor()?,
        SelectionResult::PickedColor(r, g, b) => {
            let hex = format!("#{:02X}{:02X}{:02X}", r, g, b);
            let mut cb = ClipboardManager::new()?;
            cb.copy_text(&hex)?;
            let state = app.state::<AppState>();
            let show = state.config.lock().unwrap().ui.show_notifications;
            if show {
                let _ = show_notification("Color picked", &hex);
            }
            return Ok(());
        }
    };

    let state = app.state::<AppState>();

    let capture_type = match mode {
        CaptureModeArg::Region => CaptureType::Region,
        CaptureModeArg::Window => CaptureType::Window,
        CaptureModeArg::Fullscreen | CaptureModeArg::ActiveMonitor => CaptureType::FullScreen,
    };

    let mut image = Arc::new(image);
    {
        let mut plugin_manager = state.plugin_manager.lock().unwrap();
        let event = PluginEvent::PostCapture {
            image: image.clone(),
            mode: capture_type,
        };
        match plugin_manager.dispatch(&event) {
            PluginResponse::Cancel => return Ok(()),
            PluginResponse::ModifiedImage(modified) => image = modified,
            PluginResponse::Continue => {}
        }
    }

    if matches!(post, PostActionArg::OpenEditor) {
        let config = state.config.lock().unwrap().clone();
        let base = config.output_path();
        let path = get_unique_filepath(&base);
        std::fs::create_dir_all(&config.output.directory).ok();
        crate::clipboard::save_image(&image, &path, config.output.format, config.output.quality)?;
        *state.last_save.lock().unwrap() = Some(path.clone());
        open_in_default_image_editor(&path)?;
        Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
        if config.ui.show_notifications {
            let _ = show_notification("Capture opened", &path.to_string_lossy());
        }
        return Ok(());
    }

    let post_action = match post {
        PostActionArg::Clipboard => PostCaptureAction::CopyToClipboard,
        PostActionArg::SaveFile => PostCaptureAction::SaveToFile,
        PostActionArg::Upload => PostCaptureAction::Upload,
        PostActionArg::SaveAndClipboard => PostCaptureAction::SaveAndCopy,
        PostActionArg::OpenEditor => unreachable!(),
        PostActionArg::Prompt => PostCaptureAction::PromptUser,
    };

    run_post_action(&state, &image, post_action)
}

fn build_upload_service(config: &Config) -> UploadService {
    match config.upload.destination {
        UploadDestination::Imgur => UploadService::Imgur,
        UploadDestination::Custom => UploadService::Custom(CustomUploader {
            name: "Custom".to_string(),
            request_url: config.upload.custom_url.clone(),
            file_form_name: config.upload.custom_form_name.clone(),
            response_url_path: config.upload.custom_response_path.clone(),
        }),
        UploadDestination::Ftp => UploadService::Ftp(FtpTarget {
            host: config.upload.ftp.host.clone(),
            port: config.upload.ftp.port,
            username: config.upload.ftp.username.clone(),
            password: config.upload.ftp.password.clone(),
            remote_dir: config.upload.ftp.remote_dir.clone(),
            use_tls: config.upload.ftp.use_tls,
            public_url_template: config.upload.ftp.public_url_template.clone(),
        }),
    }
}

fn capture_active_monitor() -> anyhow::Result<RgbaImage> {
    use crate::capture::list_monitors;
    let monitors = list_monitors().unwrap_or_default();
    let capture = if let Some(primary) = monitors.iter().find(|m| m.is_primary) {
        ScreenCapture::with_monitor(primary.id)
    } else {
        ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new())
    };
    capture.capture()
}

fn run_post_action(
    state: &AppState,
    image: &RgbaImage,
    action: PostCaptureAction,
) -> anyhow::Result<()> {
    let config = state.config.lock().unwrap().clone();

    let do_save = || -> anyhow::Result<PathBuf> {
        let base = config.output_path();
        let path = get_unique_filepath(&base);
        std::fs::create_dir_all(&config.output.directory).ok();
        save_image(image, &path, config.output.format, config.output.quality)?;
        *state.last_save.lock().unwrap() = Some(path.clone());
        Ok(path)
    };

    let do_clipboard = || -> anyhow::Result<()> {
        let mut cb = ClipboardManager::new()?;
        cb.copy_image(image)?;
        Ok(())
    };

    let do_upload = || -> anyhow::Result<UploadRecord> {
        let uploader = ImageUploader::new()?;
        let service = build_upload_service(&config);
        let result = uploader.upload(image, &service)?;
        let record = UploadRecord {
            url: result.url.clone(),
            delete_url: result.delete_url.clone(),
        };
        *state.last_upload.lock().unwrap() = Some(record.clone());
        if config.upload.copy_url_to_clipboard {
            let _ = crate::upload::copy_url_to_clipboard(&result.url);
        }
        Ok(record)
    };

    match action {
        PostCaptureAction::SaveToFile => {
            let path = do_save()?;
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Capture saved", &path.to_string_lossy());
            }
        }
        PostCaptureAction::CopyToClipboard => {
            do_clipboard()?;
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Copied", "Capture on clipboard");
            }
        }
        PostCaptureAction::SaveAndCopy => {
            let path = do_save()?;
            let _ = do_clipboard();
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification(
                    "Capture saved + copied",
                    &path.to_string_lossy(),
                );
            }
        }
        PostCaptureAction::Upload => {
            let record = do_upload()?;
            Sound::Upload.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let body = match record.delete_url.as_ref() {
                    Some(del) => format!("{}\nDelete: {}", record.url, del),
                    None => record.url.clone(),
                };
                let _ = show_notification("Uploaded", &body);
            }
        }
        PostCaptureAction::PromptUser => {
            let path = do_save()?;
            let _ = do_clipboard();
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification(
                    "Capture saved + copied",
                    &path.to_string_lossy(),
                );
            }
        }
    }
    Ok(())
}

pub fn open_in_default_image_editor(path: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "\"\"", "/B"])
            .arg(path)
            .spawn()?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

#[tauri::command]
pub fn list_captures(state: State<AppState>) -> Result<Vec<HistoryEntry>, String> {
    let config = state.config.lock().unwrap().clone();
    let dir = config.output.directory.clone();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<HistoryEntry> = Vec::new();
    let read = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified_unix = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        entries.push(HistoryEntry {
            path: path.to_string_lossy().to_string(),
            filename: path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string(),
            size_bytes: metadata.len(),
            modified_unix,
            is_gif: ext == "gif",
        });
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.modified_unix));
    Ok(entries)
}

#[tauri::command]
pub fn delete_capture(path: String, state: State<AppState>) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let dir_canonical =
        std::fs::canonicalize(&config.output.directory).map_err(|e| e.to_string())?;
    if !canonical.starts_with(&dir_canonical) {
        return Err("Path is outside the configured output directory".into());
    }
    std::fs::remove_file(&canonical).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn copy_capture_to_clipboard(path: String, state: State<AppState>) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let dir_canonical =
        std::fs::canonicalize(&config.output.directory).map_err(|e| e.to_string())?;
    if !canonical.starts_with(&dir_canonical) {
        return Err("Path is outside the configured output directory".into());
    }
    let img = image::open(&canonical).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
    cb.copy_image(&rgba).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reupload_capture(
    path: String,
    state: State<AppState>,
) -> Result<UploadResponse, String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let dir_canonical =
        std::fs::canonicalize(&config.output.directory).map_err(|e| e.to_string())?;
    if !canonical.starts_with(&dir_canonical) {
        return Err("Path is outside the configured output directory".into());
    }
    let img = image::open(&canonical).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let uploader = ImageUploader::new().map_err(|e| e.to_string())?;
    let service = build_upload_service(&config);
    let result = uploader.upload(&rgba, &service).map_err(|e| e.to_string())?;
    *state.last_upload.lock().unwrap() = Some(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    if config.upload.copy_url_to_clipboard {
        let _ = crate::upload::copy_url_to_clipboard(&result.url);
    }
    Ok(UploadResponse {
        url: result.url,
        delete_url: result.delete_url,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResponse {
    pub url: String,
    pub delete_url: Option<String>,
}

#[tauri::command]
pub fn open_in_explorer(path: String, state: State<AppState>) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let dir_canonical =
        std::fs::canonicalize(&config.output.directory).map_err(|e| e.to_string())?;
    if !canonical.starts_with(&dir_canonical) {
        return Err("Path is outside the configured output directory".into());
    }
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg("/select,")
            .arg(&canonical)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let parent = canonical.parent().unwrap_or(&canonical);
        open::that_in_background(parent);
    }
    Ok(())
}

#[tauri::command]
pub fn exit_app(app: AppHandle) {
    app.exit(0);
}

const HUB_LABEL: &str = "hub";

pub fn open_hub_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(HUB_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(900.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .decorations(false)
        .visible(true)
        .build()?;
    Ok(())
}

pub fn trigger_task(app: &AppHandle, task_id: &str) {
    let task = {
        let state = app.state::<AppState>();
        let config = state.config.lock().unwrap();
        config
            .capture_tasks
            .iter()
            .find(|t| t.id == task_id)
            .cloned()
    };
    let Some(task) = task else {
        tracing::warn!("hotkey fired for unknown task id: {task_id}");
        return;
    };
    let app_handle = app.clone();
    let task_label = task.name.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_task(&task, &app_handle) {
            tracing::warn!("task '{}' failed: {e}", task.id);
            emit_error(
                &app_handle,
                "task",
                &format!("{}: {}", task_label, e),
            );
            let state = app_handle.state::<AppState>();
            let show = state.config.lock().unwrap().ui.show_notifications;
            if show {
                let _ = show_notification(&format!("Task '{}' failed", task_label), &e.to_string());
            }
        }
    });
}

pub fn run_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    if matches!(task.capture_mode, TaskCaptureMode::RegionGif) {
        return run_gif_task(task, app);
    }
    let mode = match task.capture_mode {
        TaskCaptureMode::Region | TaskCaptureMode::Window | TaskCaptureMode::Fullscreen => {
            CaptureModeArg::from_task_mode(task.capture_mode)
        }
        TaskCaptureMode::ActiveMonitor => CaptureModeArg::ActiveMonitor,
        TaskCaptureMode::RegionGif => unreachable!("handled above"),
    };
    let post = PostActionArg::from_task_action(task.post_action);
    run_capture_pipeline(mode, post, app)
}

fn run_gif_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let current = *state.recording_state.lock().unwrap();
    let active_id = state.recording_task_id.lock().unwrap().clone();

    if matches!(current, RecordingState::Recording) {
        // Same task hotkey re-pressed → user wants to stop.
        // Different task hotkey while recording → ignore to avoid losing the in-progress recording.
        if active_id.as_deref() == Some(task.id.as_str()) {
            stop_gif_recording(app);
        } else {
            tracing::info!(
                "ignoring gif start request from '{}': '{:?}' is already recording",
                task.id,
                active_id
            );
        }
        return Ok(());
    }

    if matches!(current, RecordingState::Processing) {
        // Mid-save from a previous run; skip.
        return Ok(());
    }

    let selection = UnifiedSelector::select();
    let region = match selection {
        SelectionResult::Region(r) => r,
        SelectionResult::Cancelled => return Ok(()),
        _ => {
            tracing::info!("gif task '{}' aborted: needs a region selection", task.id);
            return Ok(());
        }
    };

    start_gif_recording(task, app, region)
}

fn start_gif_recording(
    task: &CaptureTask,
    app: &AppHandle,
    region: Rectangle,
) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();

    let settings = RecordingSettings {
        fps: cfg.capture.gif_fps,
        max_duration: Duration::from_secs(cfg.capture.gif_max_duration_secs as u64),
        quality: cfg.output.quality,
    };

    let mut recorder = GifRecorder::new(settings).with_region(region);
    recorder.start()?;

    *state.gif_recorder.lock().unwrap() = Some(recorder);
    *state.recording_state.lock().unwrap() = RecordingState::Recording;
    *state.recording_task_id.lock().unwrap() = Some(task.id.clone());

    RecordingOverlay::start(region);
    let _ = app.emit("capscr://recording-started", task.id.clone());

    let app2 = app.clone();
    let task_owned = task.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let st = app2.state::<AppState>();

            let user_stopped = st.recording_task_id.lock().unwrap().is_none();
            let recorder_done = {
                let rec = st.gif_recorder.lock().unwrap();
                match rec.as_ref() {
                    Some(r) => !matches!(r.state(), RecordingState::Recording),
                    None => true,
                }
            };

            if user_stopped || recorder_done {
                break;
            }
        }

        finalize_gif_recording(&task_owned, &app2);
    });

    Ok(())
}

fn stop_gif_recording(app: &AppHandle) {
    let state = app.state::<AppState>();
    *state.recording_task_id.lock().unwrap() = None;
    let mut guard = state.gif_recorder.lock().unwrap();
    if let Some(rec) = guard.as_mut() {
        rec.stop();
    }
}

fn finalize_gif_recording(task: &CaptureTask, app: &AppHandle) {
    RecordingOverlay::stop();

    let state = app.state::<AppState>();
    *state.recording_state.lock().unwrap() = RecordingState::Processing;

    let cfg = state.config.lock().unwrap().clone();
    let mut recorder = state.gif_recorder.lock().unwrap().take();

    if let Some(ref mut rec) = recorder {
        rec.stop();
        std::thread::sleep(Duration::from_millis(250));

        let mut path = cfg.output_path();
        path.set_extension("gif");
        let path = get_unique_filepath(&path);
        std::fs::create_dir_all(&cfg.output.directory).ok();

        match rec.save(&path) {
            Ok(()) => {
                *state.last_save.lock().unwrap() = Some(path.clone());
                Sound::Screenshot.play_if_enabled(cfg.post_capture.play_sound);
                if cfg.ui.show_notifications {
                    let _ = show_notification("GIF saved", &path.to_string_lossy());
                }
                apply_gif_post_action(task, app, &path, &cfg);
            }
            Err(e) => {
                tracing::warn!("gif save failed: {e}");
                emit_error(app, "gif-save", &e.to_string());
            }
        }
    }

    *state.recording_state.lock().unwrap() = RecordingState::Idle;
    *state.recording_task_id.lock().unwrap() = None;
    let _ = app.emit("capscr://recording-stopped", task.id.clone());
}

fn apply_gif_post_action(task: &CaptureTask, app: &AppHandle, path: &std::path::Path, cfg: &Config) {
    match task.post_action {
        TaskPostAction::Clipboard | TaskPostAction::SaveAndClipboard => {
            // Clipboard support for animated GIF varies wildly across OSes/apps.
            // For now: copy the file path text so the user can paste into anything path-aware.
            if let Ok(mut cb) = ClipboardManager::new() {
                let _ = cb.copy_text(&path.to_string_lossy());
            }
        }
        TaskPostAction::OpenEditor => {
            let _ = open_in_default_image_editor(path);
        }
        TaskPostAction::Upload => {
            // GIF upload bypasses the still-image path (which re-encodes to PNG).
            // Plumbing raw-bytes upload through Imgur/Custom/FTP is its own commit;
            // for now surface the limitation so the user knows the file is on disk.
            let _ = cfg;
            emit_error(
                app,
                "gif-upload",
                "gif upload not wired yet — file saved to disk",
            );
        }
        TaskPostAction::SaveFile | TaskPostAction::Prompt => {
            // Already saved to disk; nothing further.
        }
    }
}

impl CaptureModeArg {
    pub fn from_task_mode(mode: TaskCaptureMode) -> Self {
        match mode {
            TaskCaptureMode::Region => CaptureModeArg::Region,
            TaskCaptureMode::Window => CaptureModeArg::Window,
            TaskCaptureMode::Fullscreen => CaptureModeArg::Fullscreen,
            TaskCaptureMode::ActiveMonitor => CaptureModeArg::ActiveMonitor,
            TaskCaptureMode::RegionGif => CaptureModeArg::Region,
        }
    }
}

impl PostActionArg {
    pub fn from_task_action(action: TaskPostAction) -> Self {
        match action {
            TaskPostAction::Clipboard => PostActionArg::Clipboard,
            TaskPostAction::SaveFile => PostActionArg::SaveFile,
            TaskPostAction::Upload => PostActionArg::Upload,
            TaskPostAction::SaveAndClipboard => PostActionArg::SaveAndClipboard,
            TaskPostAction::OpenEditor => PostActionArg::OpenEditor,
            TaskPostAction::Prompt => PostActionArg::Prompt,
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

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool, state: State<AppState>) -> Result<(), String> {
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    let mut config = state.config.lock().unwrap();
    config.ui.auto_start = enabled;
    config.save().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

fn plugins_dir() -> Result<PathBuf, String> {
    let project = directories::ProjectDirs::from("com", "capscr", "capscr")
        .ok_or_else(|| "cannot resolve plugins directory".to_string())?;
    Ok(project.data_dir().to_path_buf().join("plugins"))
}

#[tauri::command]
pub fn list_installed_plugins() -> Result<Vec<InstalledPlugin>, String> {
    let dir = plugins_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            continue;
        }
        let body = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let manifest: PluginManifest = match toml::from_str(&body) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("plugin {:?}: bad manifest: {e}", path.file_name());
                continue;
            }
        };
        out.push(InstalledPlugin {
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            enabled: manifest.enabled,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[tauri::command]
pub fn open_plugins_folder(app: AppHandle) -> Result<(), String> {
    let dir = plugins_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}
