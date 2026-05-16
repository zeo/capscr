#![allow(dead_code)]

use crate::capture::{Capture, RegionCapture, ScreenCapture, WindowCapture};
use crate::clipboard::{get_unique_filepath, save_image, show_notification, ClipboardManager};
use crate::config::{
    CaptureTask, Config, PostCaptureAction, TaskCaptureMode, TaskPostAction, UploadDestination,
};
use crate::overlay::{SelectionResult, UnifiedSelector};
use crate::plugin::{CaptureType, PluginEvent, PluginResponse};
use crate::sound::Sound;
use crate::state::{AppState, UploadRecord};
use crate::upload::{CustomUploader, FtpTarget, ImageUploader, UploadService};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

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
pub fn set_config(config: Config, state: State<AppState>) -> Result<(), String> {
    config.validate().map_err(|e| e.to_string())?;
    config.save().map_err(|e| e.to_string())?;
    crate::install_hdr_runtime_from_config(&config);
    state.send_hotkey_reload(config.capture_tasks.clone());
    *state.config.lock().unwrap() = config;
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
        .decorations(true)
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
    std::thread::spawn(move || {
        if let Err(e) = run_task(&task, &app_handle) {
            tracing::warn!("task '{}' failed: {e}", task.id);
            let state = app_handle.state::<AppState>();
            let show = state.config.lock().unwrap().ui.show_notifications;
            if show {
                let _ = show_notification(&format!("Task '{}' failed", task.name), &e.to_string());
            }
        }
    });
}

pub fn run_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    let mode = match task.capture_mode {
        TaskCaptureMode::Region | TaskCaptureMode::Window | TaskCaptureMode::Fullscreen => {
            CaptureModeArg::from_task_mode(task.capture_mode)
        }
        TaskCaptureMode::ActiveMonitor => CaptureModeArg::ActiveMonitor,
        TaskCaptureMode::RegionGif => {
            // GIF recording is async; phase 2 ships still-image only.
            // Phase 3 stub: treat as still region capture for now.
            CaptureModeArg::Region
        }
    };
    let post = PostActionArg::from_task_action(task.post_action);
    run_capture_pipeline(mode, post, app)
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
