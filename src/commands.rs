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
use crate::state::{AppState, HotkeyStatus, UploadRecord};
use crate::upload::{CustomUploader, FtpTarget, UploadService};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_opener::OpenerExt;

pub fn history_dir() -> Option<PathBuf> {
    Config::config_dir().map(|d| d.join("history"))
}

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
    DoNothing,
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

/// cheap-ish probe: reads PNG chunks until the `cICP` chunk or `IDAT`. The
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
pub fn set_config(
    mut config: Config,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    // preserve encrypted secrets when the UI sent empty plaintext inputs —
    // without this, every Settings → Save would wipe the vault unless the
    // user retypes their secret each time. the frontend shows an empty input
    // when an encrypted blob exists, so empty here means "keep current"
    {
        let stored = state.config.lock().unwrap();
        if config.upload.ftp.password.is_empty()
            && config.upload.ftp.password_encrypted.is_empty()
            && !stored.upload.ftp.password_encrypted.is_empty()
        {
            config.upload.ftp.password_encrypted =
                stored.upload.ftp.password_encrypted.clone();
        }
        if config.upload.sftp.password.is_empty()
            && config.upload.sftp.password_encrypted.is_empty()
            && !stored.upload.sftp.password_encrypted.is_empty()
        {
            config.upload.sftp.password_encrypted =
                stored.upload.sftp.password_encrypted.clone();
        }
        if config.upload.sftp.private_key_passphrase.is_empty()
            && config.upload.sftp.private_key_passphrase_encrypted.is_empty()
            && !stored.upload.sftp.private_key_passphrase_encrypted.is_empty()
        {
            config.upload.sftp.private_key_passphrase_encrypted =
                stored.upload.sftp.private_key_passphrase_encrypted.clone();
        }
    }
    config.validate().map_err(|e| e.to_string())?;
    config.save().map_err(|e| e.to_string())?;
    crate::install_hdr_runtime_from_config(&config);
    // respect the tray's Disable-hotkeys toggle: when off, reload with an
    // empty task list so the new config doesn't silently re-register hotkeys
    use std::sync::atomic::Ordering;
    let tasks_to_register = if state.hotkeys_disabled.load(Ordering::SeqCst) {
        Vec::new()
    } else {
        config.capture_tasks.clone()
    };
    state.send_hotkey_reload(tasks_to_register);
    let want_autostart = config.ui.auto_start;
    let output_dir = config.output.directory.clone();
    *state.config.lock().unwrap() = config;
    if let Err(e) = app.asset_protocol_scope().allow_directory(&output_dir, true) {
        tracing::warn!("asset scope allow_directory({:?}) failed: {e}", output_dir);
    }
    if let Ok(dir_can) = std::fs::canonicalize(&output_dir) {
        let _ = app.asset_protocol_scope().allow_directory(&dir_can, true);
    }
    if let Some(h_dir) = history_dir() {
        if let Err(e) = app.asset_protocol_scope().allow_directory(&h_dir, true) {
            tracing::warn!("asset scope allow_directory({:?}) failed: {e}", h_dir);
        }
        if let Ok(h_can) = std::fs::canonicalize(&h_dir) {
            let _ = app.asset_protocol_scope().allow_directory(&h_can, true);
        }
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

// translate the raw anyhow chain into something a non-engineer can act on.
// we keep the original text as a suffix when none of the patterns match so
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
    } else if s.contains("window") && s.contains("not found") {
        "The selected window vanished before we could capture it (minimised, closed, or moved off-screen). Try the selector again.".into()
    } else {
        raw
    }
}

pub fn run_capture_pipeline(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
) -> anyhow::Result<()> {
    run_capture_pipeline_inner(mode, post, app, None)
}

pub fn run_capture_pipeline_with_target(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
    upload_target: Option<crate::config::TaskUploadTarget>,
) -> anyhow::Result<()> {
    run_capture_pipeline_inner(mode, post, app, upload_target)
}

fn run_capture_pipeline_inner(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
    upload_target: Option<crate::config::TaskUploadTarget>,
) -> anyhow::Result<()> {


    use std::sync::atomic::Ordering;
    let gate_state = app.state::<AppState>();
    // drop the trigger when a previous capture is still in flight (likely
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
    // reset the gate on every exit (success, error, panic) so the user never
    // has to restart capscr to unstick the trigger.
    struct CaptureGate<'a>(&'a std::sync::atomic::AtomicBool);
    impl<'a> Drop for CaptureGate<'a> {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let _gate = CaptureGate(&gate_state.capture_in_progress);

    // honour the configured pre-capture delay before capturing the freeze-frame
    // (used to set up menus / hover states before the snapshot is taken).
    {
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

    // kick window enumeration onto a background thread so it overlaps the
    // freeze-frame capture below instead of running serially on the selector's
    // critical path. only the selector-backed modes consume the result.
    #[cfg(windows)]
    if matches!(
        mode,
        CaptureModeArg::Region | CaptureModeArg::Window | CaptureModeArg::Fullscreen
    ) {
        UnifiedSelector::prewarm_window_list();
    }

    let frozen_frame = if matches!(
        mode,
        CaptureModeArg::Region | CaptureModeArg::Window | CaptureModeArg::Fullscreen
    ) {
        let t0 = std::time::Instant::now();
        match ScreenCapture::all_monitors() {
            Ok(img) => {
                tracing::info!("Captured full screen freeze-frame in {}ms", t0.elapsed().as_millis());
                Some(Arc::new(img))
            }
            Err(e) => {
                tracing::warn!("Failed to capture full screen freeze-frame: {e:#}");
                None
            }
        }
    } else {
        None
    };

    let selection = match mode {
        CaptureModeArg::Region | CaptureModeArg::Window | CaptureModeArg::Fullscreen => {
            UnifiedSelector::select(frozen_frame.clone())
        }
        CaptureModeArg::ActiveMonitor => SelectionResult::FullScreen,
    };

    tracing::info!("run_capture_pipeline_inner: selection = {selection:?}");

    let (mut image, mut hdr_bitmap, screen_origin): (image::RgbaImage, Option<crate::capture::HdrBitmap>, Option<(i32, i32)>) = match selection {
        SelectionResult::Cancelled => return Ok(()),
        SelectionResult::Region(rect) => {
            if let Some(frozen) = &frozen_frame {
                #[cfg(windows)]
                let (min_x, min_y) = {
                    if let Ok(monitors) = crate::capture::fast_list_monitors() {
                        let mx = monitors.iter().map(|m| m.x).min().unwrap_or(0);
                        let my = monitors.iter().map(|m| m.y).min().unwrap_or(0);
                        (mx, my)
                    } else {
                        (0, 0)
                    }
                };
                #[cfg(not(windows))]
                let (min_x, min_y) = (0, 0);

                let img_x = (rect.x - min_x).max(0) as u32;
                let img_y = (rect.y - min_y).max(0) as u32;
                let crop_width = rect.width.min(frozen.width().saturating_sub(img_x));
                let crop_height = rect.height.min(frozen.height().saturating_sub(img_y));
                if crop_width > 0 && crop_height > 0 {
                    let cropped = image::imageops::crop_imm(&**frozen, img_x, img_y, crop_width, crop_height).to_image();
                    (cropped, None, Some((rect.x, rect.y)))
                } else {
                    (
                        RegionCapture::new(rect).capture()?,
                        None,
                        Some((rect.x, rect.y)),
                    )
                }
            } else {
                (
                    RegionCapture::new(rect).capture()?,
                    None,
                    Some((rect.x, rect.y)),
                )
            }
        }
        SelectionResult::Window(hwnd) => {
            if let Some(frozen) = &frozen_frame {
                #[cfg(windows)]
                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
                    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
                    use windows::Win32::Foundation::{RECT, HWND};
                    let mut rect = RECT::default();
                    let hwnd_struct = HWND(hwnd as *mut _);
                    let dwm_ok = DwmGetWindowAttribute(
                        hwnd_struct,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        &mut rect as *mut RECT as *mut _,
                        std::mem::size_of::<RECT>() as u32,
                    ).is_ok();
                    if dwm_ok || GetWindowRect(hwnd_struct, &mut rect).is_ok() {
                        let (min_x, min_y) = if let Ok(monitors) = crate::capture::fast_list_monitors() {
                            let mx = monitors.iter().map(|m| m.x).min().unwrap_or(0);
                            let my = monitors.iter().map(|m| m.y).min().unwrap_or(0);
                            (mx, my)
                        } else {
                            (0, 0)
                        };

                        let w_x = rect.left;
                        let w_y = rect.top;
                        let w_w = (rect.right - rect.left).max(0) as u32;
                        let w_h = (rect.bottom - rect.top).max(0) as u32;

                        let img_x = (w_x - min_x).max(0) as u32;
                        let img_y = (w_y - min_y).max(0) as u32;
                        let crop_width = w_w.min(frozen.width().saturating_sub(img_x));
                        let crop_height = w_h.min(frozen.height().saturating_sub(img_y));

                        if crop_width > 0 && crop_height > 0 {
                            let cropped = image::imageops::crop_imm(&**frozen, img_x, img_y, crop_width, crop_height).to_image();
                            (cropped, None, Some((w_x, w_y)))
                        } else {
                            let cap = WindowCapture::new(hwnd);
                            let img = cap.capture()?;
                            let origin = window_screen_origin(hwnd);
                            (img, None, origin)
                        }
                    } else {
                        let cap = WindowCapture::new(hwnd);
                        let img = cap.capture()?;
                        let origin = window_screen_origin(hwnd);
                        (img, None, origin)
                    }
                }
                #[cfg(not(windows))]
                {
                    let cap = WindowCapture::new(hwnd);
                    let img = cap.capture()?;
                    let origin = window_screen_origin(hwnd);
                    (img, None, origin)
                }
            } else {
                let cap = WindowCapture::new(hwnd);
                let img = match cap.capture() {
                    Ok(img) => img,
                    Err(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        cap.capture()?
                    }
                };
                let origin = window_screen_origin(hwnd);
                (img, None, origin)
            }
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
            let (show, play_sound) = {
                let cfg = state.config.lock().unwrap();
                (cfg.ui.show_notifications, cfg.post_capture.play_sound)
            };
            Sound::Screenshot.play_if_enabled(play_sound);
            if show {
                let _ = show_notification("Color picked", &hex);
            }
            return Ok(());
        }
    };

    let state = app.state::<AppState>();

    // honour the show_cursor toggle by painting the live cursor into the
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
        // make sure the background plugin load has finished before dispatching
        // on_capture, so a capture fired right after launch still runs plugin
        // hooks instead of racing the load. interactive captures never wait —
        // the selection time covers the load — and the wait is bounded so a
        // slow load can't hang the capture.
        state.await_plugins_ready(Duration::from_secs(3));
        let plugin_manager = state.plugin_manager.read().unwrap();
        let event = PluginEvent::PostCapture {
            image: image.clone(),
            mode: capture_type,
        };
        match plugin_manager.dispatch(&event) {
            PluginResponse::Cancel => return Ok(()),
            PluginResponse::ModifiedImage(modified) => {
                image = modified;
                // the HDR sidecar holds the original captured pixels; a plugin
                // hands back 8-bit SDR, so keeping the sidecar would pair a
                // modified SDR image with mismatched HDR data. drop it (mirrors
                // the editor overwrite path in save_edited_image).
                hdr_bitmap = None;
            }
            PluginResponse::Continue => {}
        }
    }

    if matches!(post, PostActionArg::OpenEditor | PostActionArg::Prompt) {
        let config = state.config.lock().unwrap().clone();
        let base = config.output_path();
        let path = get_unique_filepath(&base);
        if let Err(e) = std::fs::create_dir_all(&config.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }
        crate::clipboard::save_image(&image, &path, config.output.format, config.output.quality)?;
        maybe_write_hdr_sidecar(&path, &hdr_bitmap, &config);
        *state.last_save.lock().unwrap() = Some(path.clone());
        notify_capture_saved(app, &path);
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
        PostActionArg::DoNothing => PostCaptureAction::DoNothing,
    };

    let result = run_post_action(
        app,
        &state,
        image.clone(),
        hdr_bitmap,
        post_action,
        upload_target,
    );
    result.map(|_| ())
}

fn build_ftp_service(config: &Config) -> UploadService {
    UploadService::Ftp(FtpTarget {
        host: config.upload.ftp.host.clone(),
        port: config.upload.ftp.port,
        username: config.upload.ftp.username.clone(),
        password: config.upload.ftp.password_plaintext(),
        remote_dir: config.upload.ftp.remote_dir.clone(),
        use_tls: config.upload.ftp.use_tls,
        public_url_template: config.upload.ftp.public_url_template.clone(),
    })
}

fn build_sftp_service(config: &Config) -> UploadService {
    UploadService::Sftp(crate::upload::SftpTarget {
        host: config.upload.sftp.host.clone(),
        port: config.upload.sftp.port,
        username: config.upload.sftp.username.clone(),
        password: config.upload.sftp.password_plaintext(),
        remote_dir: config.upload.sftp.remote_dir.clone(),
        public_url_template: config.upload.sftp.public_url_template.clone(),
        private_key_path: config.upload.sftp.private_key_path.clone(),
        private_key_passphrase: config.upload.sftp.private_key_passphrase_plaintext(),
    })
}

fn build_imgur_service(config: &Config) -> UploadService {
    let cid = config.upload.imgur_client_id.trim();
    if cid.is_empty() || cid == "546c25a59c58ad7" {
        UploadService::Imgur
    } else {
        UploadService::ImgurWithClientId(cid.to_string())
    }
}

// build an upload service, optionally overriding the global destination with a
// per-task target. `target_override = None` uses the global config destination.
fn build_upload_service(config: &Config) -> UploadService {
    build_upload_service_for_target(config, None)
}

fn build_upload_service_for_target(
    config: &Config,
    target_override: Option<crate::config::TaskUploadTarget>,
) -> UploadService {
    use crate::config::TaskUploadTarget;
    match target_override {
        None => match config.upload.destination {
            UploadDestination::Imgur => build_imgur_service(config),
            UploadDestination::Custom => UploadService::Custom(CustomUploader {
                name: "Custom".to_string(),
                request_url: config.upload.custom_url.clone(),
                file_form_name: config.upload.custom_form_name.clone(),
                response_url_path: config.upload.custom_response_path.clone(),
            }),
            UploadDestination::Ftp => build_ftp_service(config),
            UploadDestination::Sftp => build_sftp_service(config),
        },
        Some(TaskUploadTarget::Imgur) => build_imgur_service(config),
        Some(TaskUploadTarget::Custom) => UploadService::Custom(CustomUploader {
            name: "Custom".to_string(),
            request_url: config.upload.custom_url.clone(),
            file_form_name: config.upload.custom_form_name.clone(),
            response_url_path: config.upload.custom_response_path.clone(),
        }),
        Some(TaskUploadTarget::Ftp) => build_ftp_service(config),
        Some(TaskUploadTarget::Sftp) => build_sftp_service(config),
    }
}

// returns the tonemapped SDR image alongside the raw HDR bitmap when the
// source display is HDR. Region / Window captures go through GDI BitBlt and
// can't produce HDR data, so only ActiveMonitor / Fullscreen call this.
// targets the monitor under the cursor; the primary monitor was previously
// hardcoded and surprised multi-display users.
fn capture_active_monitor_with_hdr(
) -> anyhow::Result<(RgbaImage, Option<crate::capture::HdrBitmap>)> {
    tracing::info!("capture_active_monitor_with_hdr entry");
    use crate::capture::HdrCapture;
    let target = cursor_position();
    let wgc_on = crate::capture::wgc_enabled();
    let hdr_avail = HdrCapture::is_hdr_available();

    #[cfg(windows)]
    if hdr_avail {
        if let Some(t) = target {
            if wgc_on {
                match crate::capture::wgc_capture_at_point(t.0, t.1) {
                    Ok(img) => return Ok((img, None)),
                    Err(e) => tracing::warn!("active_monitor WGC failed — fallthrough: {e:#}"),
                }
            } else {
                let hdr = HdrCapture::new();
                match hdr.capture_with_hdr_at(Some(t)) {
                    Ok(pair) => return Ok(pair),
                    Err(e) => tracing::warn!("active_monitor CPU HDR failed — GDI fallback: {e:#}"),
                }
            }
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
            // fallback to primary if the cursor lookup failed.
            xcap::Monitor::all()
                .ok()?
                .into_iter()
                .find(|m| m.is_primary())
                .map(|m| (m.x(), m.y()))
        })
}

// if the user opted into HDR preservation and the source produced an HDR
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
    let transfer = match config.capture.hdr.output_format {
        crate::config::HdrOutputFormat::Pq => crate::capture::HdrTransfer::Pq,
        crate::config::HdrOutputFormat::Hlg => crate::capture::HdrTransfer::Hlg,
    };
    if let Err(e) = crate::capture::encode_hdr_png(&sidecar_path, bitmap, transfer) {
        tracing::warn!("hdr sidecar write failed for {sidecar_path:?}: {e}");
    }
}

// returns Some(path) when a fresh file was written *this call*. Callers
// rely on this to tie the HDR sidecar to the right basename — reading
// `state.last_save` would surface a previous capture's path when this
// action was clipboard-only.
fn run_post_action(
    app: &AppHandle,
    state: &AppState,
    image: Arc<RgbaImage>,
    hdr_bitmap: Option<crate::capture::HdrBitmap>,
    action: PostCaptureAction,
    upload_target: Option<crate::config::TaskUploadTarget>,
) -> anyhow::Result<Option<PathBuf>> {
    let config = state.config.lock().unwrap().clone();

    let do_save_async = |img: Arc<RgbaImage>, hdr: Option<crate::capture::HdrBitmap>, app_handle: AppHandle| -> anyhow::Result<PathBuf> {
        let base = config.output_path();
        let path = get_unique_filepath(&base);
        if let Err(e) = std::fs::create_dir_all(&config.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }
        *state.last_save.lock().unwrap() = Some(path.clone());
        
        let path_clone = path.clone();
        let format = config.output.format;
        let quality = config.output.quality;
        let config_clone = config.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            if let Err(e) = save_image(&img, &path_clone, format, quality) {
                tracing::error!("Background save_image failed: {e:#}");
            } else {
                maybe_write_hdr_sidecar(&path_clone, &hdr, &config_clone);
                tracing::info!("Background save completed in {}ms", t0.elapsed().as_millis());
                notify_capture_saved(&app_handle, &path_clone);
            }
        });
        Ok(path)
    };

    let do_save_to_history_async = |img: Arc<RgbaImage>, hdr: Option<crate::capture::HdrBitmap>, app_handle: AppHandle| -> Option<PathBuf> {
        if !config.ui.save_clipboard_to_history {
            return None;
        }
        let history_dir = history_dir()?;
        if let Err(e) = std::fs::create_dir_all(&history_dir) {
            tracing::warn!("failed to create history dir: {e}");
            return None;
        }
        let base = {
            let now = chrono::Local::now();
            let name = now.format(&config.output.filename_template).to_string();
            let ext = config.output.format.extension();
            history_dir.join(format!("{name}.{ext}"))
        };
        let path = get_unique_filepath(&base);
        
        let path_clone = path.clone();
        let format = config.output.format;
        let quality = config.output.quality;
        let config_clone = config.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            if let Err(e) = save_image(&img, &path_clone, format, quality) {
                tracing::error!("Background save_image to history failed: {e:#}");
            } else {
                maybe_write_hdr_sidecar(&path_clone, &hdr, &config_clone);
                tracing::info!("Background save to history completed in {}ms", t0.elapsed().as_millis());
                notify_capture_saved(&app_handle, &path_clone);
            }
        });
        Some(path)
    };

    let do_clipboard = || -> anyhow::Result<()> {
        let mut cb = ClipboardManager::new()?;
        cb.copy_image(&image)?;
        Ok(())
    };

    let do_upload = || -> anyhow::Result<crate::upload::UploadResult> {
        let uploader = crate::upload::shared_uploader()?;
        let service = build_upload_service_for_target(&config, upload_target);
        let result = uploader.upload(&image, &service)?;
        state.record_upload(UploadRecord {
            url: result.url.clone(),
            delete_url: result.delete_url.clone(),
        });
        crate::rebuild_tray_menu(app);
        if config.upload.copy_url_to_clipboard {
            let _ = crate::upload::copy_url_to_clipboard(&result.url);
        }
        Ok(result)
    };

    match action {
        PostCaptureAction::SaveToFile => {
            let path = do_save_async(image.clone(), hdr_bitmap.clone(), app.clone())?;
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Capture saved", &path.to_string_lossy());
            }
            Ok(Some(path))
        }
        PostCaptureAction::CopyToClipboard => {
            let history_path = do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
            let clipboard_ok = do_clipboard().is_ok();
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let title = if clipboard_ok {
                    "Copied"
                } else {
                    "Clipboard busy"
                };
                let body = if clipboard_ok {
                    "Capture on clipboard"
                } else {
                    "Capture ready but clipboard was occupied"
                };
                let _ = show_notification(title, body);
            }
            Ok(history_path)
        }
        PostCaptureAction::SaveAndCopy => {
            let path = do_save_async(image.clone(), hdr_bitmap.clone(), app.clone())?;
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
            let history_path = do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
            let result = do_upload()?;
            Sound::Upload.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Uploaded", &result.url);
            }
            emit_upload_success(app, &result);
            Ok(history_path)
        }
        PostCaptureAction::PromptUser => {
            let path = do_save_async(image.clone(), hdr_bitmap.clone(), app.clone())?;
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
        PostCaptureAction::DoNothing => {
            let history_path = do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Capture complete", "Screenshot taken successfully.");
            }
            Ok(history_path)
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

    let mut filenames: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut dir_entries: Vec<std::fs::DirEntry> = Vec::new();

    if dir.exists() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    filenames.insert(name.to_string());
                }
                dir_entries.push(entry);
            }
        }
    }

    if let Some(h_dir) = history_dir() {
        if h_dir.exists() {
            if let Ok(read) = std::fs::read_dir(&h_dir) {
                for entry in read.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        if !filenames.contains(name) {
                            filenames.insert(name.to_string());
                            dir_entries.push(entry);
                        }
                    }
                }
            }
        }
    }

    let mut entries: Vec<HistoryEntry> = Vec::new();
    for entry in &dir_entries {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let path = entry.path();
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
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let has_hdr = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|stem| filenames.contains(&format!("{stem}.hdr.png")))
            .unwrap_or(false);

        let path_str = path.to_string_lossy().to_string();
        let path_clean = if let Some(stripped) = path_str.strip_prefix(r"\\?\") {
            stripped.to_string()
        } else {
            path_str
        };

        entries.push(HistoryEntry {
            path: path_clean,
            filename,
            size_bytes: metadata.len(),
            modified_unix,
            is_gif: ext == "gif",
            has_hdr,
        });
    }

    entries.sort_by_key(|e| std::cmp::Reverse(e.modified_unix));
    entries.truncate(1000);
    tracing::info!(
        "list_captures: {} entries collected (output: {}, history cache)",
        entries.len(),
        dir.display()
    );
    Ok(entries)
}

fn is_path_allowed(canonical: &std::path::Path, config: &Config) -> bool {
    if let Ok(dir_canonical) = std::fs::canonicalize(&config.output.directory) {
        if canonical.starts_with(&dir_canonical) {
            return true;
        }
    }
    if let Some(h_dir) = history_dir() {
        if let Ok(h_canonical) = std::fs::canonicalize(&h_dir) {
            if canonical.starts_with(&h_canonical) {
                return true;
            }
        }
    }
    false
}

#[tauri::command]
pub fn delete_capture(path: String, state: State<AppState>) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    // also remove the `<stem>.hdr.png` sidecar if present, so deleting a
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
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
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
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    // GIF files contain animation data that image::open drops to the first frame.
    // upload the raw file bytes via a dedicated path instead of re-encoding.
    let ext = canonical.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if ext == "gif" {
        return Err("GIF reupload is not yet supported — open the file manually and upload it from there".into());
    }
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    };
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    let file_name = canonical.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("capture")
        .to_string();
    let uploader = crate::upload::shared_uploader().map_err(|e| e.to_string())?;
    let service = build_upload_service(&config);
    let result = uploader.upload_raw(&bytes, mime, &file_name, &service).map_err(|e| e.to_string())?;
    state.record_upload(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    crate::rebuild_tray_menu(&app);
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
    // save any active gif recording before exiting so the user doesn't lose frames
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let mut recorder = state.gif_recorder.lock().unwrap().take();
    if let Some(ref mut rec) = recorder {
        rec.stop();
        let mut path = cfg.output_path();
        path.set_extension("gif");
        let path = get_unique_filepath(&path);
        if let Err(e) = std::fs::create_dir_all(&cfg.output.directory) {
            tracing::warn!("failed to create output dir on exit: {e}");
        }
        match rec.save(&path) {
            Ok(()) => {
                notify_capture_saved(&app, &path);
                if cfg.ui.show_notifications {
                    let _ = show_notification("GIF saved", &path.to_string_lossy());
                }
            }
            Err(e) => tracing::warn!("gif save on exit failed: {e}"),
        }
    }
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
    // wait for any in-flight capture to finish so we don't restart mid-encode
    {
        let state = app.state::<AppState>();
        let mut waited = 0u32;
        while state.capture_in_progress.load(std::sync::atomic::Ordering::SeqCst) && waited < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 1;
        }
    }
    app.restart();
}

const HUB_LABEL: &str = "hub";

// called in setup() so the WebView2 instance is warm before the user opens
// the tray. Without this, the first tray click pays the full WebView2 cold-
// boot cost (multi-second on most machines, >1min on some).
pub fn prewarm_hub_window(app: &tauri::App) -> tauri::Result<()> {
    if app.get_webview_window(HUB_LABEL).is_some() {
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let window = tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(900.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .decorations(false)
        .visible(false)
        .build()?;
    // intercept the close button so the WebView2 process stays alive for the
    // next tray-click. Without this we pay multi-second cold-boot every time
    // the user closes and re-opens the hub, even after the startup prewarm.
    intercept_hub_close(window);
    Ok(())
}

fn intercept_hub_close(window: tauri::WebviewWindow) {
    let app = window.app_handle().clone();
    window.clone().on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let state = app.state::<AppState>();
            let close_behavior = {
                let cfg = state.config.lock().unwrap();
                cfg.ui.close_behavior
            };
            match close_behavior {
                crate::config::CloseBehavior::MinimizeToTray => {
                    let _ = window.hide();
                }
                crate::config::CloseBehavior::MinimizeToTaskbar => {
                    let _ = window.minimize();
                }
                crate::config::CloseBehavior::Exit => {
                    exit_app(app.clone());
                }
            }
        }
    });
}

pub fn open_hub_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(HUB_LABEL) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let window = tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(900.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .decorations(false)
        .visible(true)
        .build()?;
    intercept_hub_close(window);
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
    if bytes.is_empty() {
        return Err("Image data is empty".into());
    }
    if bytes.len() > 100 * 1024 * 1024 {
        return Err("Image too large to save".into());
    }
    // atomic write: stage to a sibling temp file, then rename. A disk-full
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
    // the hdr sidecar (<stem>.hdr.png) was captured from the original unedited
    // pixels — once we overwrite the sdr file the sidecar no longer represents
    // the image content, so remove it rather than leaving a misleading orphan
    if let Some(stem) = buf.file_stem().and_then(|s| s.to_str()) {
        let sidecar = buf.with_file_name(format!("{stem}.hdr.png"));
        if sidecar.exists() {
            let _ = std::fs::remove_file(&sidecar);
        }
    }
    // surface the edit to the History tab so its tile picks up the new mtime
    notify_capture_saved(&app, &buf);
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

    state.record_upload(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    crate::rebuild_tray_menu(&app);
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
    state.record_upload(UploadRecord {
        url: result.url.clone(),
        delete_url: result.delete_url.clone(),
    });
    crate::rebuild_tray_menu(&app);
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
    run_capture_pipeline_with_target(mode, post, app, task.target_destination)
}

fn run_gif_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    let state = app.state::<AppState>();
    let current = *state.recording_state.lock().unwrap();
    let active_id = state.recording_task_id.lock().unwrap().clone();

    if matches!(current, RecordingState::Recording) {
        // same task hotkey re-pressed → user wants to stop.
        // different task hotkey while recording → reject and tell the user.
        if active_id.as_deref() == Some(task.id.as_str()) {
            stop_gif_recording(app);
        } else {
            let active_name = {
                let cfg = state.config.lock().unwrap();
                active_id.as_deref()
                    .and_then(|id| cfg.capture_tasks.iter().find(|t| t.id == id))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "another task".to_string())
            };
            emit_error(
                app,
                "recording",
                &format!(
                    "already recording '{}' — press its hotkey again to stop first",
                    active_name
                ),
            );
        }
        return Ok(());
    }

    if matches!(current, RecordingState::Processing) {
        // mid-save from a previous run; skip.
        return Ok(());
    }

    // gate is held only during selection so a screenshot hotkey pressed while
    // the region selector is visible doesn't open a second overlay
    use std::sync::atomic::Ordering as OrdGif;
    if state.capture_in_progress
        .compare_exchange(false, true, OrdGif::SeqCst, OrdGif::SeqCst)
        .is_err()
    {
        tracing::info!("capture already in progress; dropping gif trigger");
        return Ok(());
    }
    let selection = UnifiedSelector::select(None);
    state.capture_in_progress.store(false, OrdGif::SeqCst);

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
    // stop the capture thread first, then clear task_id so the monitor thread
    // doesn't call finalize before the stop signal has been sent to the encoder
    {
        let mut guard = state.gif_recorder.lock().unwrap();
        if let Some(rec) = guard.as_mut() {
            rec.stop();
        }
    }
    *state.recording_task_id.lock().unwrap() = None;
}

fn finalize_gif_recording(task: &CaptureTask, app: &AppHandle) {
    RecordingOverlay::stop();

    let state = app.state::<AppState>();
    *state.recording_state.lock().unwrap() = RecordingState::Processing;

    let cfg = state.config.lock().unwrap().clone();
    let mut recorder = state.gif_recorder.lock().unwrap().take();

    if let Some(ref mut rec) = recorder {
        rec.stop();
        // wait for the capture thread to finish rather than sleeping a fixed
        // duration — the thread sets state to Processing after its last frame
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(rec.state(), crate::recording::RecordingState::Processing) {
            if std::time::Instant::now() >= deadline {
                tracing::warn!("gif capture thread did not finish within 5s; saving partial frames");
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let mut path = cfg.output_path();
        path.set_extension("gif");
        let path = get_unique_filepath(&path);
        if let Err(e) = std::fs::create_dir_all(&cfg.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }

        match rec.save(&path) {
            Ok(()) => {
                *state.last_save.lock().unwrap() = Some(path.clone());
                Sound::Screenshot.play_if_enabled(cfg.post_capture.play_sound);
                if cfg.ui.show_notifications {
                    let _ = show_notification("GIF saved", &path.to_string_lossy());
                }
                notify_capture_saved(app, &path);
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
            // clipboard support for animated GIF varies wildly across OSes/apps.
            // for now: copy the file path text so the user can paste into anything path-aware.
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
            let target_override = task.target_destination;
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
                let service = build_upload_service_for_target(&cfg, target_override);
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("capture.gif");
                match uploader.upload_raw(&bytes, "image/gif", file_name, &service) {
                    Ok(result) => {
                        let st = app2.state::<AppState>();
                        st.record_upload(UploadRecord {
                            url: result.url.clone(),
                            delete_url: result.delete_url.clone(),
                        });
                        crate::rebuild_tray_menu(&app2);
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
        TaskPostAction::SaveFile | TaskPostAction::Prompt | TaskPostAction::DoNothing => {
            // already saved to disk; nothing further.
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
            TaskPostAction::DoNothing => PostActionArg::DoNothing,
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
pub fn notify_capture_saved(app: &AppHandle, path: &std::path::Path) {
    let _ = app.emit(
        "capscr://capture-saved",
        path.to_string_lossy().to_string(),
    );
    let state = app.state::<AppState>();
    let pm = state.plugin_manager.read().unwrap();
    let _ = pm.dispatch(&PluginEvent::PostSave {
        path: path.to_path_buf(),
    });
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
    let state = app.state::<AppState>();
    let pm = state.plugin_manager.read().unwrap();
    let _ = pm.dispatch(&PluginEvent::PostUpload {
        url: result.url.clone(),
    });
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
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
}

// legacy flat plugin.toml (the metadata-only era: top-level name/version/...).
// modern plugins use the sectioned schema parsed by crate::plugin::PluginManifest.
#[derive(Debug, Deserialize)]
struct LegacyFlatManifest {
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

/// extract (name, version, description, enabled) for the plugins list from a
/// plugin.toml body. tries the sectioned runtime schema first — what real WASM
/// plugins use — then falls back to the legacy flat schema. None if neither
/// parses. without the sectioned attempt, installed WASM plugins (whose name
/// lives under `[plugin]`) silently fail to parse and never appear in the list.
fn read_plugin_listing(body: &str) -> Option<(String, String, String, bool)> {
    if let Ok(m) = toml::from_str::<crate::plugin::PluginManifest>(body) {
        return Some((
            m.plugin.name,
            m.plugin.version,
            m.plugin.description.unwrap_or_default(),
            m.enabled,
        ));
    }
    if let Ok(m) = toml::from_str::<LegacyFlatManifest>(body) {
        return Some((m.name, m.version, m.description, m.enabled));
    }
    None
}

#[cfg(test)]
mod plugin_listing_tests {
    use super::read_plugin_listing;

    #[test]
    fn reads_sectioned_wasm_manifest() {
        let body = "enabled = false\n\
            [plugin]\nid = \"grayscale\"\nname = \"Grayscale\"\nversion = \"0.1.0\"\ndescription = \"gray\"\n\
            [runtime]\ntype = \"wasm\"\nfile = \"plugin.wasm\"\n\
            [hooks]\non_capture = \"capscr_on_capture\"\n";
        let (name, version, desc, enabled) = read_plugin_listing(body).expect("sectioned parses");
        assert_eq!(name, "Grayscale");
        assert_eq!(version, "0.1.0");
        assert_eq!(desc, "gray");
        assert!(!enabled);
    }

    #[test]
    fn reads_legacy_flat_manifest() {
        let body = "name = \"Sounds\"\nversion = \"0.1.0\"\ndescription = \"sfx\"\nenabled = true\n";
        let (name, version, _desc, enabled) = read_plugin_listing(body).expect("flat parses");
        assert_eq!(name, "Sounds");
        assert_eq!(version, "0.1.0");
        assert!(enabled);
    }

    #[test]
    fn rejects_garbage() {
        assert!(read_plugin_listing("not [ valid toml").is_none());
    }
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
        let (name, version, description, enabled) = match read_plugin_listing(&body) {
            Some(t) => t,
            None => {
                tracing::warn!("plugin {:?}: unparseable manifest", path.file_name());
                continue;
            }
        };
        out.push(InstalledPlugin {
            id: entry.file_name().to_string_lossy().to_string(),
            name,
            version,
            description,
            enabled,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// plugin load failures captured at startup, surfaced to the plugins tab. the
// runtime loads plugins once at launch, so this reflects that pass — a restart
// re-evaluates after the user fixes a plugin
#[tauri::command]
pub fn plugin_load_errors(state: State<AppState>) -> Vec<String> {
    state.plugin_load_errors.lock().unwrap().clone()
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

#[tauri::command]
pub fn toggle_plugin_enabled(id: String, enabled: bool) -> Result<(), String> {
    crate::marketplace::validate_id(&id).map_err(|e| e.to_string())?;
    let plugins = plugins_dir()?;
    let plugin_dir = plugins.join(&id);
    if !plugin_dir.is_dir() {
        return Err(format!("plugin '{}' not found", id));
    }
    let canonical_plugin = std::fs::canonicalize(&plugin_dir).map_err(|e| e.to_string())?;
    let canonical_plugins = std::fs::canonicalize(&plugins).map_err(|e| e.to_string())?;
    if !canonical_plugin.starts_with(&canonical_plugins) {
        return Err("plugin path escapes plugins dir".to_string());
    }
    let manifest_path = canonical_plugin.join("plugin.toml");
    let body = std::fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?;
    let mut table: toml::Table = toml::from_str(&body).map_err(|e| e.to_string())?;
    table.insert("enabled".to_string(), toml::Value::Boolean(enabled));
    let new_body = toml::to_string(&table).map_err(|e| e.to_string())?;
    std::fs::write(&manifest_path, new_body).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyStatusEntry {
    pub task_id: String,
    pub status: String, // "live" or "failed"
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyDiagnostics {
    pub disabled_globally: bool,
    pub statuses: Vec<HotkeyStatusEntry>,
    #[cfg(windows)]
    pub hook: HookTelemetrySnapshot,
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize)]
pub struct HookTelemetrySnapshot {
    pub installed: bool,
    pub enabled: bool,
    pub registered_count: usize,
    pub registered: Vec<HookBindingSnapshot>,
    pub calls_total: u64,
    pub keydown_calls: u64,
    pub matched_calls: u64,
    pub dispatch_sent: u64,
    pub dispatch_dropped: u64,
    pub last_vk: u32,
    pub last_mods: u8,
}

#[cfg(windows)]
#[derive(Debug, Clone, Serialize)]
pub struct HookBindingSnapshot {
    pub task_id: String,
    pub vk: u32,
    pub mods: u8,
}

// called from the hotkey thread after every register/reload to push per-task
// status into AppState. the hub Tasks view + Settings panel surface this so
// silent registration failures (risky-bare, parse fail) don't hide.
pub fn record_hotkey_status(
    app: &AppHandle,
    live_ids: &[String],
    errors: &[crate::hotkeys::HotkeyRegistrationError],
) {
    let state = app.state::<AppState>();
    let mut status = state.hotkey_status.lock().unwrap();
    status.clear();
    for id in live_ids {
        status.insert(id.clone(), HotkeyStatus::Live);
    }
    for err in errors {
        status.insert(
            err.task_id.clone(),
            HotkeyStatus::Failed {
                reason: err.reason.clone(),
            },
        );
    }
    drop(status);
    let _ = app.emit("capscr://hotkey-status", ());
}

#[tauri::command]
pub fn hotkey_diagnostics(state: State<AppState>) -> HotkeyDiagnostics {
    use std::sync::atomic::Ordering;
    let disabled = state.hotkeys_disabled.load(Ordering::SeqCst);
    let status = state.hotkey_status.lock().unwrap();
    let statuses = status
        .iter()
        .map(|(task_id, st)| match st {
            HotkeyStatus::Live => HotkeyStatusEntry {
                task_id: task_id.clone(),
                status: "live".to_string(),
                reason: None,
            },
            HotkeyStatus::Failed { reason } => HotkeyStatusEntry {
                task_id: task_id.clone(),
                status: "failed".to_string(),
                reason: Some(reason.clone()),
            },
        })
        .collect();
    HotkeyDiagnostics {
        disabled_globally: disabled,
        statuses,
        #[cfg(windows)]
        hook: {
            let t = crate::hotkeys::ll_hook::snapshot_telemetry();
            HookTelemetrySnapshot {
                installed: t.installed,
                enabled: t.enabled,
                registered_count: t.registered_count,
                registered: t
                    .registered
                    .into_iter()
                    .map(|(b, id)| HookBindingSnapshot {
                        task_id: id,
                        vk: b.vk,
                        mods: b.mods,
                    })
                    .collect(),
                calls_total: t.calls_total,
                keydown_calls: t.keydown_calls,
                matched_calls: t.matched_calls,
                dispatch_sent: t.dispatch_sent,
                dispatch_dropped: t.dispatch_dropped,
                last_vk: t.last_vk,
                last_mods: t.last_mods,
            }
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SftpKnownHost {
    pub host_port: String,
    pub fingerprint: String,
    pub first_seen_unix: u64,
}

#[tauri::command]
pub fn sftp_known_hosts() -> Result<Vec<SftpKnownHost>, String> {
    let path = crate::upload::known_hosts::KnownHosts::default_path()
        .ok_or_else(|| "config dir unresolvable".to_string())?;
    let kh = crate::upload::known_hosts::KnownHosts::load(&path);
    let mut out: Vec<SftpKnownHost> = kh
        .hosts
        .into_iter()
        .map(|(host_port, entry)| SftpKnownHost {
            host_port,
            fingerprint: entry.fingerprint,
            first_seen_unix: entry.first_seen_unix,
        })
        .collect();
    out.sort_by(|a, b| a.host_port.cmp(&b.host_port));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTestReport {
    pub destination: String,
    pub overall_ok: bool,
    pub steps: Vec<crate::upload::TestStep>,
}

// invoke wrapper around trigger_task so the hub UI can dry-run a task
// without the user needing to press its hotkey. hides the hub window first
// because a region/window capture overlay launched from a focused hub paints
// over its own selector and looks broken. tray-driven captures already get
// this for free via the existing hub.hide path; this matches the behaviour.
#[tauri::command]
pub fn fire_task(task_id: String, app: AppHandle) -> Result<(), String> {
    if let Some(hub) = app.get_webview_window(HUB_LABEL) {
        let _ = hub.hide();
    }
    trigger_task(&app, &task_id);
    Ok(())
}

#[tauri::command]
pub fn test_upload_connection(
    destination: String,
    state: State<AppState>,
) -> Result<ConnectionTestReport, String> {
    let cfg = state.config.lock().unwrap().clone();
    let steps = match destination.as_str() {
        "Ftp" | "ftp" => {
            let target = crate::upload::FtpTarget {
                host: cfg.upload.ftp.host.clone(),
                port: cfg.upload.ftp.port,
                username: cfg.upload.ftp.username.clone(),
                password: cfg.upload.ftp.password_plaintext(),
                remote_dir: cfg.upload.ftp.remote_dir.clone(),
                use_tls: cfg.upload.ftp.use_tls,
                public_url_template: cfg.upload.ftp.public_url_template.clone(),
            };
            crate::upload::test_connection_ftp(&target).map_err(|e| e.to_string())?
        }
        "Sftp" | "sftp" => {
            let target = crate::upload::SftpTarget {
                host: cfg.upload.sftp.host.clone(),
                port: cfg.upload.sftp.port,
                username: cfg.upload.sftp.username.clone(),
                password: cfg.upload.sftp.password_plaintext(),
                remote_dir: cfg.upload.sftp.remote_dir.clone(),
                public_url_template: cfg.upload.sftp.public_url_template.clone(),
                private_key_path: cfg.upload.sftp.private_key_path.clone(),
                private_key_passphrase: cfg.upload.sftp.private_key_passphrase_plaintext(),
            };
            crate::upload::test_connection_sftp(&target).map_err(|e| e.to_string())?
        }
        "Imgur" | "imgur" => {
            crate::upload::test_connection_imgur(&cfg.upload.imgur_client_id)
                .map_err(|e| e.to_string())?
        }
        "Custom" | "custom" => {
            let uploader = crate::upload::CustomUploader {
                name: "Custom".to_string(),
                request_url: cfg.upload.custom_url.clone(),
                file_form_name: cfg.upload.custom_form_name.clone(),
                response_url_path: cfg.upload.custom_response_path.clone(),
            };
            crate::upload::test_connection_custom(&uploader).map_err(|e| e.to_string())?
        }
        other => return Err(format!("'{other}' has no test-connection probe")),
    };
    let overall_ok = !steps.is_empty() && steps.iter().all(|s| s.ok);
    Ok(ConnectionTestReport {
        destination,
        overall_ok,
        steps,
    })
}

#[tauri::command]
pub fn sftp_forget_host(host_port: String) -> Result<bool, String> {
    let path = crate::upload::known_hosts::KnownHosts::default_path()
        .ok_or_else(|| "config dir unresolvable".to_string())?;
    let mut kh = crate::upload::known_hosts::KnownHosts::load(&path);
    let removed = kh.forget(&host_port);
    if removed {
        kh.save(&path).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

/// Arm the LL hook to capture the next non-modifier keydown as a hotkey.
/// On press, the backend emits `capscr://hotkey-captured` with the vk +
/// mods + canonical hotkey string and clears the arm. UI cancels via
/// `cancel_hotkey_capture` if the user backs out.
#[tauri::command]
#[cfg(windows)]
pub fn start_hotkey_capture() -> Result<(), String> {
    crate::hotkeys::ll_hook::begin_capture();
    Ok(())
}

#[tauri::command]
#[cfg(not(windows))]
pub fn start_hotkey_capture() -> Result<(), String> {
    Err("hotkey capture is windows-only".into())
}

#[tauri::command]
#[cfg(windows)]
pub fn cancel_hotkey_capture() -> Result<(), String> {
    crate::hotkeys::ll_hook::cancel_capture();
    Ok(())
}

#[tauri::command]
#[cfg(not(windows))]
pub fn cancel_hotkey_capture() -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub fn set_hotkeys_disabled(
    disabled: bool,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let was = state.hotkeys_disabled.swap(disabled, Ordering::SeqCst);
    if was == disabled {
        return Ok(());
    }
    // persist into config so the toggle survives restart
    {
        let mut cfg = state.config.lock().unwrap();
        cfg.hotkeys.disabled_globally = disabled;
        if let Err(e) = cfg.save() {
            tracing::warn!("hotkeys_disabled persist failed: {e}");
        }
    }
    #[cfg(windows)]
    crate::hotkeys::ll_hook::set_enabled(!disabled);
    // re-emit reload so the manager status reflects the new state. when
    // disabled we send an empty Vec; when re-enabled we send the live tasks.
    let tasks = if disabled {
        Vec::new()
    } else {
        state.config.lock().unwrap().capture_tasks.clone()
    };
    state.send_hotkey_reload(tasks);
    crate::rebuild_tray_menu(&app);
    let _ = app.emit("capscr://hotkey-status", ());
    Ok(())
}


