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
use crate::upload::{CustomUploader, FtpTarget, UploadService};
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
    pub has_hdr: bool,
}

#[tauri::command]
pub fn get_config(state: State<AppState>) -> Result<Config, String> {
    Ok(state.config.lock().unwrap().clone())
}

#[tauri::command]
pub fn get_default_config() -> Config {
    Config::default()
}

/// Cheap-ish probe: reads PNG chunks until the `cICP` chunk or `IDAT`. The
/// editor calls this on load to know whether to warn the user that edits
/// will flatten HDR to SDR. Returns false on any read error.
#[tauri::command]
pub fn is_hdr_capture(path: String) -> bool {
    let path = std::path::PathBuf::from(path);
    crate::capture::read_cicp(&path)
        .map(|info| info.is_hdr())
        .unwrap_or(false)
}

#[tauri::command]
pub fn set_config(config: Config, app: AppHandle, state: State<AppState>) -> Result<(), String> {
    config.validate().map_err(|e| e.to_string())?;
    config.save().map_err(|e| e.to_string())?;
    crate::install_hdr_runtime_from_config(&config);
    state.send_hotkey_reload(config.capture_tasks.clone());
    let want_autostart = config.ui.auto_start;
    let output_dir = config.output.directory.clone();
    *state.config.lock().unwrap() = config;
    // Sync asset:// scope with the new output dir so History thumbnails keep
    // loading after the user changes the path mid-session.
    if let Err(e) = app.asset_protocol_scope().allow_directory(&output_dir, true) {
        tracing::warn!("asset scope allow_directory({:?}) failed: {e}", output_dir);
    }
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
        // catch_unwind so a panic in any capture sub-step (D3D11, GIF encoder,
        // image crate, plugin host) gets reported as a normal error instead
        // of silently killing the worker thread with no user feedback.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_capture_pipeline(mode, post, &app_handle)
        }));
        let outcome = match result {
            Ok(r) => r,
            Err(panic_info) => {
                let msg = if let Some(s) = panic_info.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "internal panic (no message)".to_string()
                };
                Err(anyhow::anyhow!("capture pipeline panicked: {msg}"))
            }
        };
        if let Err(e) = outcome {
            tracing::warn!("capture failed: {e:#}");
            let friendly = humanize_capture_error(&e);
            emit_error(&app_handle, "capture", &friendly);
            let state = app_handle.state::<AppState>();
            let show = state.config.lock().unwrap().ui.show_notifications;
            if show {
                let _ = show_notification("Capture failed", &friendly);
            }
        }
    });
    Ok(())
}

// Translate the raw anyhow chain into something a non-engineer can act on.
// We keep the original text as a suffix when none of the patterns match so
// debugging info isn't lost.
fn humanize_capture_error(e: &anyhow::Error) -> String {
    let raw = format!("{:#}", e);
    let s = raw.to_lowercase();
    if s.contains("d3d11") || s.contains("device") || s.contains("dxgi") {
        "GPU couldn't initialise the capture pipeline. Try updating your graphics driver or rebooting.".into()
    } else if s.contains("no monitor") || s.contains("no display") || s.contains("monitor not found") {
        "capscr couldn't find a display. If you just unplugged a monitor, try again or restart capscr.".into()
    } else if s.contains("access is denied") || s.contains("permission") || s.contains("denied") {
        "Windows blocked the capture. Run capscr as administrator or check Settings > Privacy > Screen recording.".into()
    } else if s.contains("hdr") {
        "HDR capture failed. Turn HDR off in Windows display settings, or disable HDR in capscr Settings > capture > hdr.".into()
    } else if s.contains("shader") || s.contains("compile") {
        "Shader compilation failed. Update your graphics driver. (If this keeps happening, file an issue with your GPU model.)".into()
    } else if s.contains("clipboard") {
        "Couldn't write to the clipboard. Another app may be holding it open — try the capture again.".into()
    } else if s.contains("region") && (s.contains("invalid") || s.contains("zero")) {
        "Selected region is too small or off-screen. Drag a larger area.".into()
    } else {
        raw
    }
}

pub fn run_capture_pipeline(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
) -> anyhow::Result<()> {
    use std::sync::atomic::Ordering;
    let gate_state = app.state::<AppState>();
    // Drop the trigger when a previous capture is still in flight (likely
    // hung on a stuck D3D11 device or held selector). Without this gate a
    // user repeatedly mashing the hotkey accumulates stalled worker threads.
    if gate_state
        .capture_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        tracing::info!("capture already in progress; dropping new trigger");
        return Ok(());
    }
    // Reset the gate on every exit (success, error, panic) so the user never
    // has to restart capscr to unstick the trigger.
    struct CaptureGate<'a>(&'a std::sync::atomic::AtomicBool);
    impl<'a> Drop for CaptureGate<'a> {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _gate = CaptureGate(&gate_state.capture_in_progress);

    let selection = match mode {
        CaptureModeArg::Region | CaptureModeArg::Window | CaptureModeArg::Fullscreen => {
            UnifiedSelector::select()
        }
        CaptureModeArg::ActiveMonitor => SelectionResult::FullScreen,
    };

    // Honour the configured pre-capture delay (used to set up menus / hover
    // states between picking the region and the actual grab). Skip when the
    // selection was cancelled or a color was picked — neither produces a
    // pixel capture.
    if matches!(
        selection,
        SelectionResult::Region(_) | SelectionResult::Window(_) | SelectionResult::FullScreen
    ) {
        let delay_ms = app
            .state::<AppState>()
            .config
            .lock()
            .unwrap()
            .capture
            .delay_ms;
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms as u64));
        }
    }

    let (mut image, hdr_bitmap, screen_origin): (image::RgbaImage, Option<crate::capture::HdrBitmap>, Option<(i32, i32)>) = match selection {
        SelectionResult::Cancelled => return Ok(()),
        SelectionResult::Region(rect) => (
            RegionCapture::new(rect).capture()?,
            None,
            Some((rect.x, rect.y)),
        ),
        SelectionResult::Window(hwnd) => {
            let img = WindowCapture::new(hwnd).capture()?;
            // Look up the window's screen origin so the cursor composite can
            // land at the right offset within the captured pixels.
            let origin = window_screen_origin(hwnd);
            (img, None, origin)
        }
        SelectionResult::FullScreen => {
            let (img, hdr) = capture_active_monitor_with_hdr()?;
            let origin = active_monitor_origin();
            (img, hdr, origin)
        }
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

    // Honour the show_cursor toggle by painting the live cursor into the
    // captured pixels at its screen-relative position. Skipped if the
    // capture didn't expose a screen origin (e.g. an unknown selection
    // variant). Failures inside composite_system_cursor are silent — they
    // never take down the capture.
    {
        let show = state.config.lock().unwrap().capture.show_cursor;
        if show {
            if let Some(origin) = screen_origin {
                crate::capture::composite_system_cursor(&mut image, origin);
            }
        }
    }

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
        maybe_write_hdr_sidecar(&path, &hdr_bitmap, &config);
        *state.last_save.lock().unwrap() = Some(path.clone());
        let _ = app.emit("capscr://capture-saved", path.to_string_lossy().to_string());
        open_editor_window(app, &path.to_string_lossy())
            .map_err(|e| anyhow::anyhow!(e))?;
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

    let result = run_post_action(app, &state, &image, post_action);
    // Drop the HDR sidecar next to the file we just wrote — never against
    // a previous capture's path, which is what reading state.last_save
    // would surface for clipboard-only / upload-only actions.
    if let Ok(Some(sdr_path)) = &result {
        let cfg = state.config.lock().unwrap().clone();
        maybe_write_hdr_sidecar(sdr_path, &hdr_bitmap, &cfg);
        // Notify the hub so the History tab live-refreshes — otherwise the
        // user has to hit "reload" manually after every capture.
        let _ = app.emit("capscr://capture-saved", sdr_path.to_string_lossy().to_string());
    }
    result.map(|_| ())
}

fn build_upload_service(config: &Config) -> UploadService {
    match config.upload.destination {
        UploadDestination::Imgur => {
            let cid = config.upload.imgur_client_id.trim();
            if cid.is_empty() || cid == "546c25a59c58ad7" {
                UploadService::Imgur
            } else {
                UploadService::ImgurWithClientId(cid.to_string())
            }
        }
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

// Returns the tonemapped SDR image alongside the raw HDR bitmap when the
// source display is HDR. Region / Window captures go through GDI BitBlt and
// can't produce HDR data, so only ActiveMonitor / Fullscreen call this.
// Targets the monitor under the cursor; the primary monitor was previously
// hardcoded and surprised multi-display users.
fn capture_active_monitor_with_hdr(
) -> anyhow::Result<(RgbaImage, Option<crate::capture::HdrBitmap>)> {
    use crate::capture::HdrCapture;
    let target = cursor_position();
    if HdrCapture::is_hdr_available() {
        let hdr = HdrCapture::new();
        if let Ok(pair) = hdr.capture_with_hdr_at(target) {
            return Ok(pair);
        }
    }
    let capture = match target {
        Some((x, y)) => ScreenCapture::at_point(x, y).unwrap_or_else(|_| {
            ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new())
        }),
        None => ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new()),
    };
    Ok((capture.capture()?, None))
}

#[cfg(windows)]
fn cursor_position() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}

#[cfg(not(windows))]
fn cursor_position() -> Option<(i32, i32)> {
    None
}

fn window_screen_origin(window_id: u32) -> Option<(i32, i32)> {
    let windows = xcap::Window::all().ok()?;
    windows
        .into_iter()
        .find(|w| w.id() == window_id)
        .map(|w| (w.x(), w.y()))
}

fn active_monitor_origin() -> Option<(i32, i32)> {
    let (cx, cy) = cursor_position()?;
    xcap::Monitor::from_point(cx, cy)
        .ok()
        .map(|m| (m.x(), m.y()))
        .or_else(|| {
            // Fallback to primary if the cursor lookup failed.
            xcap::Monitor::all()
                .ok()?
                .into_iter()
                .find(|m| m.is_primary())
                .map(|m| (m.x(), m.y()))
        })
}

// If the user opted into HDR preservation and the source produced an HDR
// bitmap, write a `<basename>.hdr.png` sidecar next to the SDR file. Failures
// are reported via tracing but never fail the overall capture — the SDR file
// is the source of truth.
fn maybe_write_hdr_sidecar(
    sdr_path: &std::path::Path,
    hdr: &Option<crate::capture::HdrBitmap>,
    config: &Config,
) {
    if !config.output.preserve_hdr {
        return;
    }
    let Some(bitmap) = hdr.as_ref() else { return };
    let stem = match sdr_path.file_stem() {
        Some(s) => s.to_os_string(),
        None => return,
    };
    let mut sidecar_name = stem;
    sidecar_name.push(".hdr.png");
    let sidecar_path = sdr_path.with_file_name(sidecar_name);
    if let Err(e) = crate::capture::encode_hdr_png(&sidecar_path, bitmap) {
        tracing::warn!("hdr sidecar write failed for {sidecar_path:?}: {e}");
    }
}

// Returns Some(path) when a fresh file was written *this call*. Callers
// rely on this to tie the HDR sidecar to the right basename — reading
// `state.last_save` would surface a previous capture's path when this
// action was clipboard-only.
fn run_post_action(
    app: &AppHandle,
    state: &AppState,
    image: &RgbaImage,
    action: PostCaptureAction,
) -> anyhow::Result<Option<PathBuf>> {
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

    let do_upload = || -> anyhow::Result<crate::upload::UploadResult> {
        let uploader = crate::upload::shared_uploader()?;
        let service = build_upload_service(&config);
        let result = uploader.upload(image, &service)?;
        *state.last_upload.lock().unwrap() = Some(UploadRecord {
            url: result.url.clone(),
            delete_url: result.delete_url.clone(),
        });
        if config.upload.copy_url_to_clipboard {
            let _ = crate::upload::copy_url_to_clipboard(&result.url);
        }
        Ok(result)
    };

    match action {
        PostCaptureAction::SaveToFile => {
            let path = do_save()?;
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Capture saved", &path.to_string_lossy());
            }
            Ok(Some(path))
        }
        PostCaptureAction::CopyToClipboard => {
            do_clipboard()?;
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Copied", "Capture on clipboard");
            }
            Ok(None)
        }
        PostCaptureAction::SaveAndCopy => {
            let path = do_save()?;
            // Don't claim "+ copied" if the clipboard step actually failed —
            // surface the partial success honestly. Save is the source of
            // truth; clipboard is best-effort because another app could be
            // holding it open.
            let clipboard_ok = do_clipboard().is_ok();
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let title = if clipboard_ok {
                    "Capture saved + copied"
                } else {
                    "Capture saved (clipboard busy)"
                };
                let _ = show_notification(title, &path.to_string_lossy());
            }
            Ok(Some(path))
        }
        PostCaptureAction::Upload => {
            let result = do_upload()?;
            Sound::Upload.play_if_enabled(config.post_capture.play_sound);
            emit_upload_success(app, &result);
            Ok(None)
        }
        PostCaptureAction::PromptUser => {
            let path = do_save()?;
            let clipboard_ok = do_clipboard().is_ok();
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let title = if clipboard_ok {
                    "Capture saved + copied"
                } else {
                    "Capture saved (clipboard busy)"
                };
                let _ = show_notification(title, &path.to_string_lossy());
            }
            Ok(Some(path))
        }
    }
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

    // First pass: collect every present filename so we can mark each SDR
    // entry's `has_hdr` by looking up a `<stem>.hdr.png` sidecar.
    let mut filenames: std::collections::HashSet<String> = std::collections::HashSet::new();
    let read = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    let dir_entries: Vec<_> = read.flatten().collect();
    for entry in &dir_entries {
        if let Some(name) = entry.file_name().to_str() {
            filenames.insert(name.to_string());
        }
    }

    let mut entries: Vec<HistoryEntry> = Vec::new();
    for entry in &dir_entries {
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
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_string();
        // Hide raw HDR sidecars from the History grid — they're paired with
        // an SDR file and surface via the `has_hdr` badge on that entry.
        if filename.ends_with(".hdr.png") {
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

        let has_hdr = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| filenames.contains(&format!("{stem}.hdr.png")))
            .unwrap_or(false);

        entries.push(HistoryEntry {
            path: path.to_string_lossy().to_string(),
            filename,
            size_bytes: metadata.len(),
            modified_unix,
            is_gif: ext == "gif",
            has_hdr,
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
    // Also remove the `<stem>.hdr.png` sidecar if present, so deleting a
    // capture from History doesn't leave orphan HDR data on disk.
    if let Some(stem) = canonical.file_stem().and_then(|s| s.to_str()) {
        let sidecar = canonical.with_file_name(format!("{stem}.hdr.png"));
        if sidecar.exists() && sidecar != canonical {
            let _ = std::fs::remove_file(&sidecar);
        }
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
    app: AppHandle,
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
    let uploader = crate::upload::shared_uploader().map_err(|e| e.to_string())?;
    let service = build_upload_service(&config);
    let result = uploader.upload(&rgba, &service).map_err(|e| e.to_string())?;
    *state.last_upload.lock().unwrap() = Some(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    if config.upload.copy_url_to_clipboard {
        let _ = crate::upload::copy_url_to_clipboard(&result.url);
    }
    emit_upload_success(&app, &result);
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

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version.to_string(),
        current_version: u.current_version.to_string(),
        notes: u.body.clone(),
    }))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

const HUB_LABEL: &str = "hub";

// Called in setup() so the WebView2 instance is warm before the user opens
// the tray. Without this, the first tray click pays the full WebView2 cold-
// boot cost (multi-second on most machines, >1min on some).
pub fn prewarm_hub_window(app: &tauri::App) -> tauri::Result<()> {
    if app.get_webview_window(HUB_LABEL).is_some() {
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(900.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .decorations(false)
        .visible(false)
        .build()?;
    Ok(())
}

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

const EDITOR_LABEL: &str = "editor";

pub fn open_editor_window(app: &AppHandle, image_path: &str) -> tauri::Result<()> {
    let state = app.state::<AppState>();
    *state.editor_image_path.lock().unwrap() = Some(image_path.to_string());

    if let Some(window) = app.get_webview_window(EDITOR_LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("capscr://editor-load", image_path.to_string());
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    tauri::WebviewWindowBuilder::new(app, EDITOR_LABEL, url)
        .title("capscr — edit")
        .inner_size(1200.0, 800.0)
        .min_inner_size(800.0, 600.0)
        .resizable(true)
        .decorations(false)
        .visible(true)
        .build()?;
    Ok(())
}

#[tauri::command]
pub fn get_editor_image_path(state: State<AppState>) -> Option<String> {
    state.editor_image_path.lock().unwrap().clone()
}

#[tauri::command]
pub fn open_editor(path: String, app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let cfg = state.config.lock().unwrap().clone();
    let dir_canonical =
        std::fs::canonicalize(&cfg.output.directory).map_err(|e| e.to_string())?;
    if !canonical.starts_with(&dir_canonical) {
        return Err("Path is outside the configured output directory".into());
    }
    open_editor_window(&app, &canonical.to_string_lossy()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_edited_image(
    bytes: Vec<u8>,
    target_path: String,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    let buf = PathBuf::from(&target_path);
    let parent = buf
        .parent()
        .ok_or_else(|| "target_path has no parent".to_string())?;
    let config = state.config.lock().unwrap().clone();
    let canonical_parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    let dir_canonical =
        std::fs::canonicalize(&config.output.directory).map_err(|e| e.to_string())?;
    if !canonical_parent.starts_with(&dir_canonical) {
        return Err("Path is outside the configured output directory".into());
    }
    if bytes.len() > 100 * 1024 * 1024 {
        return Err("Image too large to save".into());
    }
    // Atomic write: stage to a sibling temp file, then rename. A disk-full
    // or permission-denied mid-write would otherwise truncate the original
    // — the user would lose the un-edited capture too.
    let mut tmp = buf.clone();
    let stem = buf
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("edited");
    tmp.set_file_name(format!(".{stem}.editing.tmp"));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write failed: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &buf) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename failed: {e}"));
    }
    // Surface the edit to the History tab so its tile picks up the new
    // modified time without a manual reload.
    let _ = app.emit("capscr://capture-saved", buf.to_string_lossy().to_string());
    Ok(())
}

#[tauri::command]
pub fn copy_edited_image_to_clipboard(bytes: Vec<u8>) -> Result<(), String> {
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
    cb.copy_image(&rgba).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn upload_file(
    path: String,
    app: AppHandle,
    state: State<AppState>,
) -> Result<UploadResponse, String> {
    let buf = PathBuf::from(&path);
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    if !canonical.is_file() {
        return Err("not a regular file".into());
    }

    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "" => return Err("file has no extension; cannot detect type".into()),
        other => return Err(format!("unsupported file type: .{}", other)),
    };

    let metadata = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
    if metadata.len() > 100 * 1024 * 1024 {
        return Err("file too large to upload (>100 MB)".into());
    }

    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    let file_name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload")
        .to_string();

    let config = state.config.lock().unwrap().clone();
    let uploader = crate::upload::shared_uploader().map_err(|e| e.to_string())?;
    let service = build_upload_service(&config);
    let result = uploader
        .upload_raw(&bytes, mime, &file_name, &service)
        .map_err(|e| e.to_string())?;

    *state.last_upload.lock().unwrap() = Some(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    if config.upload.copy_url_to_clipboard {
        let _ = crate::upload::copy_url_to_clipboard(&result.url);
    }
    emit_upload_success(&app, &result);

    Ok(UploadResponse {
        url: result.url,
        delete_url: result.delete_url,
    })
}

#[tauri::command]
pub fn upload_edited_image(
    bytes: Vec<u8>,
    app: AppHandle,
    state: State<AppState>,
) -> Result<UploadResponse, String> {
    let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let config = state.config.lock().unwrap().clone();
    let uploader = crate::upload::shared_uploader().map_err(|e| e.to_string())?;
    let service = build_upload_service(&config);
    let result = uploader.upload(&rgba, &service).map_err(|e| e.to_string())?;
    *state.last_upload.lock().unwrap() = Some(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    if config.upload.copy_url_to_clipboard {
        let _ = crate::upload::copy_url_to_clipboard(&result.url);
    }
    emit_upload_success(&app, &result);
    Ok(UploadResponse {
        url: result.url,
        delete_url: result.delete_url,
    })
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
    set_tray_tooltip(app, &format!("capscr · recording '{}'", task.name));

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
                let _ = app.emit("capscr://capture-saved", path.to_string_lossy().to_string());
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
    set_tray_tooltip(app, "capscr");
}

fn set_tray_tooltip(app: &AppHandle, tooltip: &str) {
    if let Some(tray) = app.tray_by_id("capscr-tray") {
        let _ = tray.set_tooltip(Some(tooltip));
    }
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
            let _ = open_editor_window(app, &path.to_string_lossy());
        }
        TaskPostAction::Upload => {
            let app2 = app.clone();
            let path = path.to_path_buf();
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        emit_error(&app2, "upload", &e.to_string());
                        return;
                    }
                };
                let uploader = match crate::upload::shared_uploader() {
                    Ok(u) => u,
                    Err(e) => {
                        emit_error(&app2, "upload", &e.to_string());
                        return;
                    }
                };
                let service = build_upload_service(&cfg);
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("capture.gif");
                match uploader.upload_raw(&bytes, "image/gif", file_name, &service) {
                    Ok(result) => {
                        let st = app2.state::<AppState>();
                        *st.last_upload.lock().unwrap() = Some(UploadRecord {
                            url: result.url.clone(),
                            delete_url: result.delete_url.clone(),
                        });
                        if cfg.upload.copy_url_to_clipboard {
                            let _ = crate::upload::copy_url_to_clipboard(&result.url);
                        }
                        Sound::Upload.play_if_enabled(cfg.post_capture.play_sound);
                        emit_upload_success(&app2, &result);
                    }
                    Err(e) => emit_error(&app2, "upload", &e.to_string()),
                }
            });
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

#[derive(Debug, Clone, Serialize)]
pub struct UploadSuccessPayload {
    pub url: String,
    pub delete_url: Option<String>,
}

pub fn emit_upload_success(app: &AppHandle, result: &crate::upload::UploadResult) {
    let _ = app.emit(
        "capscr://upload-success",
        UploadSuccessPayload {
            url: result.url.clone(),
            delete_url: result.delete_url.clone(),
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

// Wrapper exported for setup-time pre-creation in main.rs.
pub fn resolve_plugins_dir() -> Result<PathBuf, String> {
    plugins_dir()
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

#[tauri::command]
pub async fn marketplace_browse(state: State<'_, AppState>) -> Result<Vec<crate::marketplace::RegistryEntry>, String> {
    let url = state.config.lock().unwrap().marketplace.registry_url.clone();
    // reqwest::blocking inside an async command — push to a worker thread so
    // we don't park the tokio runtime.
    tokio::task::spawn_blocking(move || crate::marketplace::fetch_registry(&url))
        .await
        .map_err(|e| e.to_string())?
        .map(|reg| reg.plugins)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn marketplace_install(id: String, state: State<'_, AppState>) -> Result<(), String> {
    let url = state.config.lock().unwrap().marketplace.registry_url.clone();
    let plugins = plugins_dir()?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let registry = crate::marketplace::fetch_registry(&url)?;
        let entry = registry
            .plugins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow::anyhow!("plugin '{}' not in registry", id))?;
        crate::marketplace::install_plugin(&plugins, entry)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn marketplace_uninstall(id: String) -> Result<(), String> {
    let plugins = plugins_dir()?;
    crate::marketplace::uninstall_plugin(&plugins, &id).map_err(|e| e.to_string())
}
