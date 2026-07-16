#![allow(dead_code)]

use crate::capture::{Capture, Rectangle, RegionCapture, ScreenCapture, WindowCapture};
use crate::clipboard::{get_unique_filepath, save_image, show_notification, ClipboardManager};
use crate::config::{
    CaptureTask, Config, PostCaptureAction, TaskCaptureMode, TaskPostAction, UploadDestination,
};
use crate::overlay::{RecordingOverlay, SelectionResult, UnifiedSelector};
use crate::plugin::{CaptureType, PluginEvent, PluginResponse};
use crate::recording::{GifRecorder, RecordingSettings, RecordingState, StopReason};
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
    RegionLast,
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
    CopyText,
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
            config.upload.ftp.password_encrypted = stored.upload.ftp.password_encrypted.clone();
        }
        if config.upload.sftp.password.is_empty()
            && config.upload.sftp.password_encrypted.is_empty()
            && !stored.upload.sftp.password_encrypted.is_empty()
        {
            config.upload.sftp.password_encrypted = stored.upload.sftp.password_encrypted.clone();
        }
        if config.upload.sftp.private_key_passphrase.is_empty()
            && config
                .upload
                .sftp
                .private_key_passphrase_encrypted
                .is_empty()
            && !stored
                .upload
                .sftp
                .private_key_passphrase_encrypted
                .is_empty()
        {
            config.upload.sftp.private_key_passphrase_encrypted =
                stored.upload.sftp.private_key_passphrase_encrypted.clone();
        }
        if config.upload.s3.secret_access_key.is_empty()
            && config.upload.s3.secret_access_key_encrypted.is_empty()
            && !stored.upload.s3.secret_access_key_encrypted.is_empty()
        {
            config.upload.s3.secret_access_key_encrypted =
                stored.upload.s3.secret_access_key_encrypted.clone();
        }
    }
    // the global hotkey kill switch lives in the atomic (the tray and Settings
    // toggle it there); make the persisted config agree with it so this save
    // can't quietly clear the switch and re-enable every hotkey on next launch
    config.hotkeys.disabled_globally = state
        .hotkeys_disabled
        .load(std::sync::atomic::Ordering::SeqCst);
    config.validate().map_err(|e| e.to_string())?;
    config.save().map_err(|e| e.to_string())?;
    crate::install_hdr_runtime_from_config(&config);
    // respect the tray's Disable-hotkeys toggle: when off, reload with an
    // empty task list so the new config doesn't silently re-register hotkeys
    use std::sync::atomic::Ordering;
    // apply the advanced-input toggle before the reload flush reads it; an
    // explicit enable also starts the evdev readers on demand
    #[cfg(target_os = "linux")]
    if let Some(advanced) = config.hotkeys.advanced_input {
        crate::hotkeys::set_advanced_input(advanced);
        if advanced {
            crate::hotkeys::evdev_linux::start(app.clone());
        }
    }
    let tasks_to_register = if state.hotkeys_disabled.load(Ordering::SeqCst) {
        Vec::new()
    } else {
        config.capture_tasks.clone()
    };
    state.send_hotkey_reload(tasks_to_register);
    let want_autostart = config.ui.auto_start;
    let output_dir = config.output.directory.clone();
    *state.config.lock().unwrap() = config;
    if let Err(e) = app
        .asset_protocol_scope()
        .allow_directory(&output_dir, true)
    {
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
    crate::rebuild_tray_menu(&app);
    let _ = app.emit("capscr://config-updated", ());
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
    } else if s.contains("no monitor")
        || s.contains("no display")
        || s.contains("monitor not found")
    {
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
    run_capture_pipeline_inner(mode, post, app, None, None)
}

pub fn run_capture_pipeline_with_target(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
    upload_target: Option<crate::config::TaskUploadTarget>,
    delay_override: Option<u32>,
) -> anyhow::Result<()> {
    run_capture_pipeline_inner(mode, post, app, upload_target, delay_override)
}

fn run_capture_pipeline_inner(
    mode: CaptureModeArg,
    post: PostActionArg,
    app: &AppHandle,
    upload_target: Option<crate::config::TaskUploadTarget>,
    delay_override: Option<u32>,
) -> anyhow::Result<()> {
    // cancel selection if already active
    if UnifiedSelector::active_selector_active() {
        UnifiedSelector::cancel_active_selection();
        return Ok(());
    }

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

    // wayland compositors bake the pointer into the grab; windows composites
    // it afterwards, so this hint only steers the linux capture calls
    #[cfg(target_os = "linux")]
    crate::capture::set_include_cursor(gate_state.config.lock().unwrap().capture.show_cursor);

    // keep capscr itself out of the frozen desktop when capture is triggered
    // from an open hub or a second-instance jump command
    let hub_was_visible = app
        .get_webview_window(HUB_LABEL)
        .and_then(|hub| hub.is_visible().ok().map(|visible| (hub, visible)))
        .filter(|(_, visible)| *visible);
    if let Some((hub, _)) = hub_was_visible {
        let _ = hub.hide();
        // let the compositor present one frame without the hub before capture
        std::thread::sleep(Duration::from_millis(34));
    }

    // honour the configured pre-capture delay before capturing the freeze-frame
    // (used to set up menus / hover states before the snapshot is taken).
    {
        // a per-task delay overrides the global one; clamp so a hand-edited
        // config can't stall the capture thread for minutes
        let global_delay = app
            .state::<AppState>()
            .config
            .lock()
            .unwrap()
            .capture
            .delay_ms;
        let delay_ms = delay_override.unwrap_or(global_delay).min(30_000);
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms as u64));
        }
    }

    // "region (last)" re-fires the previous selection rectangle without showing
    // the selector. only the first use (no stored rect yet) falls back to a
    // normal region drag.
    let replay_rect = if matches!(mode, CaptureModeArg::RegionLast) {
        *gate_state.last_region.lock().unwrap()
    } else {
        None
    };
    #[cfg(target_os = "linux")]
    let compositor_window =
        if matches!(mode, CaptureModeArg::Window) && crate::capture::is_wayland_session() {
            let include_cursor = gate_state.config.lock().unwrap().capture.show_cursor;
            match crate::capture::capture_wayland_window(include_cursor) {
                Ok(image) => Some(image),
                Err(error) if format!("{error:#}").contains("Cancelled") => return Ok(()),
                // gnome offers no window list to ordinary apps; its own
                // picker through the portal's interactive mode is the only
                // sanctioned window-pick there
                Err(_) if crate::shell::desktop() == crate::shell::DesktopEnv::Gnome => {
                    match crate::capture::portal_screenshot_interactive() {
                        Ok(image) => Some(image),
                        Err(error) => {
                            tracing::info!("portal interactive pick declined ({error:#})");
                            return Ok(());
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(
                    "compositor window selection unavailable ({error:#}); using capscr selector"
                );
                    None
                }
            }
        } else {
            None
        };
    #[cfg(not(target_os = "linux"))]
    let compositor_window: Option<image::RgbaImage> = None;

    let needs_selector = compositor_window.is_none()
        && (matches!(
            mode,
            CaptureModeArg::Region | CaptureModeArg::Window | CaptureModeArg::Fullscreen
        ) || (matches!(mode, CaptureModeArg::RegionLast) && replay_rect.is_none()));

    // kick window enumeration onto a background thread so it overlaps the
    // freeze-frame capture below instead of running serially on the selector's
    // critical path. only the selector-backed modes consume the result.
    if needs_selector {
        UnifiedSelector::prewarm_window_list();
    }

    let frozen_frame = if needs_selector {
        let t0 = std::time::Instant::now();
        #[cfg(target_os = "linux")]
        let captured = (!crate::capture::is_wayland_session()).then(ScreenCapture::all_monitors);
        #[cfg(not(target_os = "linux"))]
        let captured = Some(ScreenCapture::all_monitors());

        match captured {
            Some(Ok(img)) => {
                tracing::info!(
                    "Captured full screen freeze-frame in {}ms",
                    t0.elapsed().as_millis()
                );
                Some(Arc::new(img))
            }
            Some(Err(e)) => {
                tracing::warn!("Failed to capture full screen freeze-frame: {e:#}");
                None
            }
            None => None,
        }
    } else {
        None
    };

    // snapshot the cursor at the freeze-frame instant for selector-backed modes
    // so the composite step paints it where it was when the screen froze instead
    // of wherever the mouse ended up after the region drag
    let frozen_cursor = if needs_selector {
        crate::capture::capture_cursor_shot()
    } else {
        None
    };

    let mut compositor_window = compositor_window;
    let selection = if compositor_window.is_some() {
        SelectionResult::Cancelled
    } else {
        match mode {
            _ if replay_rect.is_some() => SelectionResult::Region(replay_rect.unwrap()),
            CaptureModeArg::Region
            | CaptureModeArg::RegionLast
            | CaptureModeArg::Window
            | CaptureModeArg::Fullscreen => UnifiedSelector::select(frozen_frame.clone()),
            CaptureModeArg::ActiveMonitor => SelectionResult::FullScreen,
        }
    };

    #[cfg(target_os = "linux")]
    match &selection {
        SelectionResult::FrozenRegion { rect, .. } => {
            tracing::info!("run_capture_pipeline_inner: frozen region = {rect:?}")
        }
        selection => tracing::info!("run_capture_pipeline_inner: selection = {selection:?}"),
    }
    #[cfg(not(target_os = "linux"))]
    tracing::info!("run_capture_pipeline_inner: selection = {selection:?}");
    #[cfg(target_os = "linux")]
    if needs_selector
        && crate::capture::is_wayland_session()
        && !matches!(&selection, SelectionResult::Cancelled)
    {
        std::thread::sleep(Duration::from_millis(20));
    }

    let (mut image, mut hdr_bitmap, screen_origin): (
        image::RgbaImage,
        Option<crate::capture::HdrBitmap>,
        Option<(i32, i32)>,
    ) = match selection {
        SelectionResult::Cancelled => match compositor_window.take() {
            Some(image) => (image, None, None),
            None => return Ok(()),
        },
        SelectionResult::Region(rect) => {
            // remember the rectangle so a "region (last)" task can replay it
            *gate_state.last_region.lock().unwrap() = Some(rect);
            #[cfg(target_os = "linux")]
            let native_wayland = if crate::capture::is_wayland_session() {
                Some(crate::capture::capture_wayland_region(rect)?)
            } else {
                None
            };
            #[cfg(not(target_os = "linux"))]
            let native_wayland: Option<image::RgbaImage> = None;

            if let Some(captured) = native_wayland {
                (captured, None, Some((rect.x, rect.y)))
            } else if let Some(frozen) = &frozen_frame {
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
                    let cropped =
                        image::imageops::crop_imm(&**frozen, img_x, img_y, crop_width, crop_height)
                            .to_image();
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
        #[cfg(target_os = "linux")]
        SelectionResult::FrozenRegion { rect, image } => {
            *gate_state.last_region.lock().unwrap() = Some(rect);
            (Arc::unwrap_or_clone(image), None, Some((rect.x, rect.y)))
        }
        SelectionResult::Window(hwnd) => {
            if let Some(frozen) = &frozen_frame {
                #[cfg(windows)]
                unsafe {
                    use windows::Win32::Foundation::{HWND, RECT};
                    use windows::Win32::Graphics::Dwm::{
                        DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS,
                    };
                    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
                    let mut rect = RECT::default();
                    let hwnd_struct = HWND(hwnd as *mut _);
                    let dwm_ok = DwmGetWindowAttribute(
                        hwnd_struct,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        &mut rect as *mut RECT as *mut _,
                        std::mem::size_of::<RECT>() as u32,
                    )
                    .is_ok();
                    if dwm_ok || GetWindowRect(hwnd_struct, &mut rect).is_ok() {
                        let (min_x, min_y) =
                            if let Ok(monitors) = crate::capture::fast_list_monitors() {
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
                            let cropped = image::imageops::crop_imm(
                                &**frozen,
                                img_x,
                                img_y,
                                crop_width,
                                crop_height,
                            )
                            .to_image();
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
                    // crop the window's rect out of the frozen frame so the
                    // pixels match what the selector showed; a live window
                    // capture is the fallback when bounds are unavailable
                    let cropped = window_bounds(hwnd).and_then(|(w_x, w_y, w_w, w_h)| {
                        let monitors = crate::capture::list_monitors().unwrap_or_default();
                        let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
                        let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
                        let img_x = (w_x - min_x).max(0) as u32;
                        let img_y = (w_y - min_y).max(0) as u32;
                        let crop_width = w_w.min(frozen.width().saturating_sub(img_x));
                        let crop_height = w_h.min(frozen.height().saturating_sub(img_y));
                        if crop_width == 0 || crop_height == 0 {
                            return None;
                        }
                        let img = image::imageops::crop_imm(
                            &**frozen,
                            img_x,
                            img_y,
                            crop_width,
                            crop_height,
                        )
                        .to_image();
                        Some((img, None, Some((w_x, w_y))))
                    });
                    match cropped {
                        Some(result) => result,
                        None => {
                            let cap = WindowCapture::new(hwnd);
                            let img = cap.capture()?;
                            let origin = window_screen_origin(hwnd);
                            (img, None, origin)
                        }
                    }
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
        #[cfg(target_os = "linux")]
        SelectionResult::WaylandWindow { handle, x, y } => {
            let include_cursor = gate_state.config.lock().unwrap().capture.show_cursor;
            let image = crate::capture::capture_wayland_window_handle(&handle, include_cursor)?;
            (image, None, Some((x, y)))
        }
        #[cfg(not(target_os = "linux"))]
        SelectionResult::WaylandWindow { .. } => return Ok(()),
        SelectionResult::Monitor { rect, output_name } => {
            #[cfg(target_os = "linux")]
            let image = if crate::capture::is_wayland_session() {
                match output_name {
                    Some(name) => crate::capture::wayland_freeze_output(&name)?,
                    None => crate::capture::capture_wayland_area(
                        rect.x,
                        rect.y,
                        rect.width,
                        rect.height,
                    )?,
                }
            } else {
                RegionCapture::new(rect).capture()?
            };
            #[cfg(not(target_os = "linux"))]
            let image = RegionCapture::new(rect).capture()?;
            (image, None, Some((rect.x, rect.y)))
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
                match &frozen_cursor {
                    // selector-backed capture: paint the cursor as it was when the
                    // frame froze, so it only appears if it sat inside the selected
                    // area and never lands on the drag-release corner
                    Some(shot) => {
                        crate::capture::composite_cursor_shot(&mut image, shot, origin);
                    }
                    // instant capture has no overlay, so the live cursor is correct
                    None => crate::capture::composite_system_cursor(&mut image, origin),
                }
            }
        }
    }

    let capture_type = match mode {
        CaptureModeArg::Region | CaptureModeArg::RegionLast => CaptureType::Region,
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
        open_editor_window(app, &path.to_string_lossy()).map_err(|e| anyhow::anyhow!(e))?;
        Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
        if config.ui.show_notifications {
            let _ = show_notification("Capture opened", &path.to_string_lossy());
        }
        return Ok(());
    }

    if matches!(post, PostActionArg::CopyText) {
        // run OCR on the fresh capture and put the detected text on the clipboard,
        // without saving a file
        let config = state.config.lock().unwrap().clone();
        let text = ocr_capture(&image)?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            if config.ui.show_notifications {
                let _ = show_notification("No text found", "the capture had no detectable text");
            }
        } else {
            let mut cb = ClipboardManager::new().map_err(|e| anyhow::anyhow!(e))?;
            cb.copy_text(trimmed).map_err(|e| anyhow::anyhow!(e))?;
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let preview: String = trimmed.chars().take(80).collect();
                let _ = show_notification("Text copied", &preview);
            }
        }
        return Ok(());
    }

    let post_action = match post {
        PostActionArg::Clipboard => PostCaptureAction::CopyToClipboard,
        PostActionArg::SaveFile => PostCaptureAction::SaveToFile,
        PostActionArg::Upload => PostCaptureAction::Upload,
        PostActionArg::SaveAndClipboard => PostCaptureAction::SaveAndCopy,
        PostActionArg::OpenEditor | PostActionArg::CopyText => unreachable!(),
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

fn build_s3_service(config: &Config) -> UploadService {
    UploadService::S3(crate::upload::S3Target {
        bucket: config.upload.s3.bucket.clone(),
        region: config.upload.s3.region.clone(),
        endpoint: config.upload.s3.endpoint.clone(),
        access_key_id: config.upload.s3.access_key_id.clone(),
        secret_access_key: config.upload.s3.secret_access_key_plaintext(),
        public_url_template: config.upload.s3.public_url_template.clone(),
    })
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
            UploadDestination::S3 => build_s3_service(config),
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
        Some(TaskUploadTarget::S3) => build_s3_service(config),
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
    let target = cursor_position();

    #[cfg(target_os = "linux")]
    if crate::capture::is_wayland_session() {
        let monitor = crate::capture::active_wayland_monitor()?;
        let image = crate::capture::capture_wayland_area(
            monitor.x,
            monitor.y,
            monitor.width,
            monitor.height,
        )?;
        return Ok((image, None));
    }

    #[cfg(windows)]
    {
        use crate::capture::HdrCapture;
        let wgc_on = crate::capture::wgc_enabled();
        let hdr_avail = HdrCapture::is_hdr_available();
        if hdr_avail {
            if let Some(t) = target {
                if wgc_on {
                    match crate::capture::wgc_capture_at_point(t.0, t.1) {
                        Ok(img) => return Ok((img, None)),
                        Err(e) => {
                            tracing::warn!("active_monitor WGC failed — fallthrough: {e:#}")
                        }
                    }
                } else {
                    let hdr = HdrCapture::new();
                    match hdr.capture_with_hdr_at(Some(t)) {
                        Ok(pair) => return Ok(pair),
                        Err(e) => {
                            tracing::warn!("active_monitor CPU HDR failed — GDI fallback: {e:#}")
                        }
                    }
                }
            }
        }
    }
    let capture = match target {
        Some((x, y)) => ScreenCapture::at_point(x, y)
            .unwrap_or_else(|_| ScreenCapture::primary().unwrap_or_else(|_| ScreenCapture::new())),
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
    crate::capture::pointer_position()
}

fn window_screen_origin(window_id: u32) -> Option<(i32, i32)> {
    let windows = xcap::Window::all().ok()?;
    let w = windows
        .into_iter()
        .find(|w| w.id().map(|i| i == window_id).unwrap_or(false))?;
    Some((w.x().ok()?, w.y().ok()?))
}

// full window rect from the compositor, used to crop a window capture out of
// the frozen frame on platforms without DWM extended-frame bounds
#[cfg(not(windows))]
fn window_bounds(window_id: u32) -> Option<(i32, i32, u32, u32)> {
    let windows = xcap::Window::all().ok()?;
    let w = windows
        .into_iter()
        .find(|w| w.id().map(|i| i == window_id).unwrap_or(false))?;
    Some((w.x().ok()?, w.y().ok()?, w.width().ok()?, w.height().ok()?))
}

fn active_monitor_origin() -> Option<(i32, i32)> {
    let origin_of = |m: xcap::Monitor| Some((m.x().ok()?, m.y().ok()?));
    let (cx, cy) = cursor_position()?;
    xcap::Monitor::from_point(cx, cy)
        .ok()
        .and_then(origin_of)
        .or_else(|| {
            // fallback to primary if the cursor lookup failed.
            xcap::Monitor::all()
                .ok()?
                .into_iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .and_then(origin_of)
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

    // the encode runs on a worker thread; on_saved fires there once the write
    // actually succeeds, so callers announce "saved" only when it's true. a
    // failed write removes the 0-byte placeholder and toasts instead of leaving
    // the earlier "capture saved" claim standing.
    let do_save_async = |img: Arc<RgbaImage>,
                         hdr: Option<crate::capture::HdrBitmap>,
                         app_handle: AppHandle,
                         on_saved: Box<dyn FnOnce(&std::path::Path) + Send>|
     -> anyhow::Result<PathBuf> {
        let base = config.output_path();
        let path = get_unique_filepath(&base);
        if let Err(e) = std::fs::create_dir_all(&config.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }

        let path_clone = path.clone();
        let format = config.output.format;
        let quality = config.output.quality;
        let config_clone = config.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            match save_image(&img, &path_clone, format, quality) {
                Ok(()) => {
                    maybe_write_hdr_sidecar(&path_clone, &hdr, &config_clone);
                    *app_handle.state::<AppState>().last_save.lock().unwrap() =
                        Some(path_clone.clone());
                    tracing::info!(
                        "Background save completed in {}ms",
                        t0.elapsed().as_millis()
                    );
                    notify_capture_saved(&app_handle, &path_clone);
                    on_saved(&path_clone);
                }
                Err(e) => {
                    tracing::error!("Background save_image failed: {e:#}");
                    let _ = std::fs::remove_file(&path_clone);
                    emit_error(&app_handle, "save", "couldn't write the capture to disk");
                    if config_clone.ui.show_notifications {
                        let _ = show_notification(
                            "Capture failed",
                            "couldn't write the capture to disk",
                        );
                    }
                }
            }
        });
        Ok(path)
    };

    let do_save_to_history_async = |img: Arc<RgbaImage>,
                                    hdr: Option<crate::capture::HdrBitmap>,
                                    app_handle: AppHandle|
     -> Option<PathBuf> {
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
                tracing::info!(
                    "Background save to history completed in {}ms",
                    t0.elapsed().as_millis()
                );
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
            let play = config.post_capture.play_sound;
            let show = config.ui.show_notifications;
            let path = do_save_async(
                image.clone(),
                hdr_bitmap.clone(),
                app.clone(),
                Box::new(move |path| {
                    Sound::Screenshot.play_if_enabled(play);
                    if show {
                        let _ = show_notification("Capture saved", &path.to_string_lossy());
                    }
                }),
            )?;
            Ok(Some(path))
        }
        PostCaptureAction::CopyToClipboard => {
            let history_path =
                do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
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
            let clipboard_ok = do_clipboard().is_ok();
            let play = config.post_capture.play_sound;
            let show = config.ui.show_notifications;
            let path = do_save_async(
                image.clone(),
                hdr_bitmap.clone(),
                app.clone(),
                Box::new(move |path| {
                    Sound::Screenshot.play_if_enabled(play);
                    if show {
                        let title = if clipboard_ok {
                            "Capture saved + copied"
                        } else {
                            "Capture saved (clipboard busy)"
                        };
                        let _ = show_notification(title, &path.to_string_lossy());
                    }
                }),
            )?;
            Ok(Some(path))
        }
        PostCaptureAction::Upload => {
            let history_path =
                do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
            let result = do_upload()?;
            Sound::Upload.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Uploaded", &result.url);
            }
            emit_upload_success(app, &result);
            Ok(history_path)
        }
        PostCaptureAction::PromptUser => {
            let clipboard_ok = do_clipboard().is_ok();
            let play = config.post_capture.play_sound;
            let show = config.ui.show_notifications;
            let path = do_save_async(
                image.clone(),
                hdr_bitmap.clone(),
                app.clone(),
                Box::new(move |path| {
                    Sound::Screenshot.play_if_enabled(play);
                    if show {
                        let title = if clipboard_ok {
                            "Capture saved + copied"
                        } else {
                            "Capture saved (clipboard busy)"
                        };
                        let _ = show_notification(title, &path.to_string_lossy());
                    }
                }),
            )?;
            Ok(Some(path))
        }
        PostCaptureAction::DoNothing => {
            let history_path =
                do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
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
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/C", "start", "\"\"", "/B"])
            .arg(path)
            // CREATE_NO_WINDOW: cmd is a console app and the gui-subsystem
            // parent would otherwise flash a console window
            .creation_flags(0x0800_0000)
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
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "mp4"
        ) {
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

        let is_mp4 = ext == "mp4";
        entries.push(HistoryEntry {
            path: path_clean,
            filename,
            size_bytes: metadata.len(),
            modified_unix,
            is_gif: ext == "gif",
            is_mp4,
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
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if ext == "gif" || ext == "mp4" {
        // copy the file itself so pasting inserts the animation instead of a
        // path string; fall back to path text if the file copy fails
        #[cfg(any(windows, target_os = "linux"))]
        if crate::clipboard::copy_file_to_clipboard(&canonical).is_ok() {
            return Ok(());
        }
        let mut cb = ClipboardManager::new().map_err(|e| e.to_string())?;
        return cb
            .copy_text(&canonical.to_string_lossy())
            .map_err(|e| e.to_string());
    }
    let img = image::open(&canonical).map_err(|e| e.to_string())?;
    let rgba = img.into_rgba8();
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
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if ext == "gif" {
        return Err(
            "GIF reupload is not yet supported — open the file manually and upload it from there"
                .into(),
        );
    }
    if ext == "mp4" {
        return Err(
            "MP4 reupload is not yet supported — open the file manually and upload it from there"
                .into(),
        );
    }
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/png",
    };
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    let file_name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("capture")
        .to_string();
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

#[derive(Debug, Clone, Serialize)]
pub struct UploadResponse {
    pub url: String,
    pub delete_url: Option<String>,
}

#[tauri::command]
pub fn open_in_explorer(
    path: String,
    app: AppHandle,
    state: State<AppState>,
) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    // accept the history dir too, not just the output dir — the History tab
    // lists both, so "reveal in folder" must reach either
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed capture directories".into());
    }
    reveal_in_file_manager(&app, &canonical);
    Ok(())
}

#[cfg(windows)]
fn reveal_in_file_manager(_app: &AppHandle, path: &std::path::Path) {
    let _ = std::process::Command::new("explorer")
        .arg("/select,")
        .arg(path)
        .spawn();
}

// the opener plugin talks to the freedesktop FileManager1 dbus interface and
// falls back to plain-opening the parent directory itself
#[cfg(not(windows))]
fn reveal_in_file_manager(app: &AppHandle, path: &std::path::Path) {
    use tauri_plugin_opener::OpenerExt;
    if let Err(e) = app.opener().reveal_item_in_dir(path) {
        tracing::warn!("reveal in file manager failed: {e}");
    }
}

#[tauri::command]
pub fn exit_app(app: AppHandle) {
    // save any active recording before exiting so the user doesn't lose frames
    let state = app.state::<AppState>();
    let cfg = state.config.lock().unwrap().clone();
    let mut recorder = state.gif_recorder.lock().unwrap().take();
    if let Some(ref mut rec) = recorder {
        rec.stop();
        // let the capture thread drain its last frames before we save, the same
        // way finalize_gif_recording does, so we don't clip the tail
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(rec.state(), crate::recording::RecordingState::Processing) {
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let is_mp4 = rec.format() == crate::recording::RecordingFormat::Mp4;
        let mut path = cfg.output_path();
        path.set_extension(if is_mp4 { "mp4" } else { "gif" });
        let path = get_unique_filepath(&path);
        if let Err(e) = std::fs::create_dir_all(&cfg.output.directory) {
            tracing::warn!("failed to create output dir on exit: {e}");
        }
        let save_result = if is_mp4 {
            rec.save_mp4(&path).map(|_| ())
        } else {
            rec.save(&path)
        };
        match save_result {
            Ok(()) => {
                notify_capture_saved(&app, &path);
                if cfg.ui.show_notifications {
                    let title = if is_mp4 { "Video saved" } else { "GIF saved" };
                    let _ = show_notification(title, &path.to_string_lossy());
                }
            }
            Err(e) => {
                tracing::warn!("recording save on exit failed: {e}");
                let err_type = if is_mp4 { "mp4-save" } else { "gif-save" };
                emit_error(&app, err_type, &e.to_string());
            }
        }
    }
    app.exit(0);
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub install_kind: &'static str,
}

// the linux updater can only swap a running AppImage; deb/rpm/plain-binary
// installs must fetch the release themselves
fn update_install_kind() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("APPIMAGE").is_some() {
            "in-place"
        } else {
            "external"
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        "in-place"
    }
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
        install_kind: update_install_kind(),
    }))
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    if update_install_kind() == "external" {
        return Err("this install can't self-update; download the new release instead".into());
    }
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
        while state
            .capture_in_progress
            .load(std::sync::atomic::Ordering::SeqCst)
            && waited < 50
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 1;
        }
    }
    app.restart();
}

const HUB_LABEL: &str = "hub";

// the canonical app url for healing webviews stuck on about:blank: a live
// hub wins, then the last observed good url, then the fixed release origin
// (or the dev server in dev builds)
pub fn canonical_app_url(app: &AppHandle) -> Option<url::Url> {
    if let Some(hub) = app.get_webview_window(HUB_LABEL) {
        if let Ok(url) = hub.url() {
            if url.scheme() != "about" {
                remember_canonical_url(app, &url);
                return Some(url);
            }
        }
    }
    if let Some(url) = app
        .state::<AppState>()
        .canonical_webview_url
        .lock()
        .unwrap()
        .clone()
    {
        return Some(url);
    }
    if tauri::is_dev() {
        return app.config().build.dev_url.clone();
    }
    // custom-protocol release builds serve the bundled assets from a fixed
    // origin (observed via the selector watchdog logs)
    url::Url::parse("tauri://localhost").ok()
}

pub fn remember_canonical_url(app: &AppHandle, url: &url::Url) {
    if url.scheme() != "about" {
        *app
            .state::<AppState>()
            .canonical_webview_url
            .lock()
            .unwrap() = Some(url.clone());
    }
}

// tauri#13967-adjacent: a webview can finish loading with its module script
// never executed, leaving the static boot splash up forever; reload heals it
fn heal_stuck_boot(window: tauri::WebviewWindow) {
    std::thread::spawn(move || {
        for delay_ms in [3000u64, 8000] {
            std::thread::sleep(Duration::from_millis(delay_ms));
            let _ =
                window.eval("if (document.getElementById('boot')) window.location.reload()");
        }
    });
}

// called in setup() so the WebView2 instance is warm before the user opens
// the tray. Without this, the first tray click pays the full WebView2 cold-
// boot cost (multi-second on most machines, >1min on some).
pub fn prewarm_hub_window(app: &tauri::App) -> tauri::Result<()> {
    if app.get_webview_window(HUB_LABEL).is_some() {
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(900.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .decorations(false)
        .visible(false);

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon)?;
    }

    let window = builder.build()?;
    // intercept the close button so the WebView2 process stays alive for the
    // next tray-click. Without this we pay multi-second cold-boot every time
    // the user closes and re-opens the hub, even after the startup prewarm.
    intercept_hub_close(window.clone());
    heal_stuck_boot(window);
    Ok(())
}

fn intercept_hub_close(window: tauri::WebviewWindow) {
    let app = window.app_handle().clone();
    window.clone().on_window_event(move |event| {
        match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let state = app.state::<AppState>();
                let close_behavior = {
                    let cfg = state.config.lock().unwrap();
                    cfg.ui.close_behavior
                };
                match close_behavior {
                    crate::config::CloseBehavior::MinimizeToTray => {
                        // on linux the hidden hub's webkit processes would
                        // keep ~100mb resident; destroy and recreate on
                        // demand (webkitgtk cold boot is fast). windows keeps
                        // the warm WebView2 (see the prewarm rationale).
                        #[cfg(target_os = "linux")]
                        {
                            if let Ok(url) = window.url() {
                                remember_canonical_url(&app, &url);
                            }
                            let _ = window.destroy();
                        }
                        #[cfg(not(target_os = "linux"))]
                        {
                            let _ = window.hide();
                        }
                    }
                    crate::config::CloseBehavior::MinimizeToTaskbar => {
                        let _ = window.minimize();
                    }
                    crate::config::CloseBehavior::Exit => {
                        exit_app(app.clone());
                    }
                }
            }
            // record what the OS actually dropped so upload_file can trust the
            // path the webview later hands it (drag-drop is the only legitimate
            // caller with an arbitrary path)
            tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) => {
                app.state::<AppState>()
                    .remember_dropped_paths(paths.clone());
            }
            _ => {}
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
    let mut builder = tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(900.0, 640.0)
        .min_inner_size(720.0, 480.0)
        .resizable(true)
        .decorations(false)
        .visible(true);

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon)?;
    }

    let window = builder.build()?;
    intercept_hub_close(window.clone());
    heal_stuck_boot(window);
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
        // the reused window may be a leftover that never loaded (about:blank);
        // heal it the same way as a fresh one — once the page boots it pulls
        // the current path via get_editor_image_path
        watch_editor_navigation(app, window);
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(app, EDITOR_LABEL, url)
        .title("capscr — edit")
        .inner_size(1200.0, 800.0)
        .min_inner_size(800.0, 600.0)
        .resizable(true)
        .decorations(false)
        .visible(true);

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon)?;
    }

    let window = builder.build()?;
    watch_editor_navigation(app, window);
    Ok(())
}

// dynamically created webview windows sometimes come up on about:blank instead
// of loading the app url (tauri#13967). The page never renders, and because the
// editor is undecorated there's no native close button either — the user is
// stuck with a dead white window. Probe the webview shortly after creation and
// navigate explicitly when that happens. The hub window is prewarmed at startup
// and never destroyed, so its url is the canonical app url to copy.
fn watch_editor_navigation(app: &AppHandle, window: tauri::WebviewWindow) {
    let app = app.clone();
    std::thread::spawn(move || {
        for wait_ms in [500u64, 1500, 3000] {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            match window.url() {
                Ok(url) if url.scheme() != "about" => {
                    remember_canonical_url(&app, &url);
                    return;
                }
                Err(_) => return,
                _ => {}
            }
            tracing::warn!("editor webview stuck on about:blank; navigating explicitly");
            if let Some(url) = canonical_app_url(&app) {
                if let Err(e) = window.navigate(url) {
                    tracing::warn!("editor explicit navigation failed: {e}");
                }
            } else {
                tracing::warn!("no canonical app url available for the editor");
            }
        }
    });
}

#[tauri::command]
pub fn get_editor_image_path(state: State<AppState>) -> Option<String> {
    // liveness signal: fires only when the editor frontend actually booted
    tracing::debug!("editor webview alive — frontend requested image path");
    state.editor_image_path.lock().unwrap().clone()
}

// async so the webview window is built off the main thread — a sync command
// runs on the main thread and WebviewWindowBuilder::build deadlocks there on
// windows, leaving a white window that can't be closed
#[tauri::command]
pub async fn open_editor(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let buf = PathBuf::from(&path);
    let canonical = std::fs::canonicalize(&buf).map_err(|e| e.to_string())?;
    let cfg = state.config.lock().unwrap().clone();
    // history captures live in the history dir; the editor must reach them too
    if !is_path_allowed(&canonical, &cfg) {
        return Err("Path is outside the allowed capture directories".into());
    }
    let ext = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if ext == "gif" || ext == "mp4" {
        return Err("Recordings can't be edited — the editor would flatten the animation".into());
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
    // allow writing edited images back into the history dir too, matching where
    // open_editor is now allowed to read from
    if !is_path_allowed(&canonical_parent, &config) {
        return Err("Path is outside the allowed capture directories".into());
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
    let stem = buf.file_name().and_then(|s| s.to_str()).unwrap_or("edited");
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
    let rgba = img.into_rgba8();
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
    // only upload a path the user actually dropped, or one of capscr's own
    // captures; refuse an arbitrary path a compromised webview might supply
    let config = state.config.lock().unwrap().clone();
    if !state.was_dropped(&canonical) && !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
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
        "mp4" => "video/mp4",
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
    let rgba = img.into_rgba8();
    let config = state.config.lock().unwrap().clone();
    let uploader = crate::upload::shared_uploader().map_err(|e| e.to_string())?;
    let service = build_upload_service(&config);
    let result = uploader
        .upload(&rgba, &service)
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
            emit_error(&app_handle, "task", &format!("{}: {}", task_label, e));
            let state = app_handle.state::<AppState>();
            let show = state.config.lock().unwrap().ui.show_notifications;
            if show {
                let _ = show_notification(&format!("Task '{}' failed", task_label), &e.to_string());
            }
        }
    });
}

pub fn run_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    if matches!(
        task.capture_mode,
        TaskCaptureMode::RegionGif | TaskCaptureMode::RegionMp4
    ) {
        return run_gif_task(task, app);
    }
    let mode = match task.capture_mode {
        TaskCaptureMode::Region
        | TaskCaptureMode::RegionLast
        | TaskCaptureMode::Window
        | TaskCaptureMode::Fullscreen => CaptureModeArg::from_task_mode(task.capture_mode),
        TaskCaptureMode::ActiveMonitor => CaptureModeArg::ActiveMonitor,
        TaskCaptureMode::RegionGif | TaskCaptureMode::RegionMp4 => unreachable!("handled above"),
    };
    let post = PostActionArg::from_task_action(task.post_action);
    run_capture_pipeline_with_target(mode, post, app, task.target_destination, task.delay_ms)
}

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
static FFMPEG_DOWNLOADING: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn perform_ffmpeg_download() -> anyhow::Result<()> {
    const FFMPEG_BIN_NAME: &str = "ffmpeg.exe";

    let proj_dirs = directories::ProjectDirs::from("com", "capscr", "capscr")
        .ok_or_else(|| anyhow::anyhow!("failed to locate app data directory"))?;
    let data_dir = proj_dirs.data_dir();
    std::fs::create_dir_all(data_dir)?;
    let dest_path = data_dir.join(FFMPEG_BIN_NAME);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("http error: {}", resp.status()));
    }

    let zip_bytes = resp.bytes()?;

    // verify the payload against the vendor's published sha256 sidecar before
    // trusting a binary we're about to write and later execute. this closes the
    // no-integrity-check gap: a truncated, corrupted, or zip-only-tampered
    // download is rejected instead of run. (it can't by itself defend a fully
    // compromised host that serves a matching hash, but it is fetched over the
    // same TLS channel and fails closed.)
    let expected = client
        .get(format!("{url}.sha256"))
        .send()?
        .error_for_status()?
        .text()?
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow::anyhow!(
            "ffmpeg checksum sidecar missing or malformed; refusing to install unverified binary"
        ));
    }
    let got = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(zip_bytes.as_ref()));
    if got != expected {
        return Err(anyhow::anyhow!(
            "ffmpeg download failed its integrity check (sha256 mismatch)"
        ));
    }

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.as_ref()))?;
    let mut found = false;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name();
        if name.ends_with("bin/ffmpeg.exe") || name.ends_with("bin/ffmpeg") {
            let mut out_file = std::fs::File::create(&dest_path)?;
            std::io::copy(&mut file, &mut out_file)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&dest_path)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&dest_path, perms)?;
            }

            found = true;
            break;
        }
    }

    if !found {
        return Err(anyhow::anyhow!("ffmpeg binary not found in zip archive"));
    }

    Ok(())
}

// the auto-download serves a windows build (gyan.dev); on linux ffmpeg is a
// package-manager install away and stays security-updated there
#[cfg(not(windows))]
fn handle_missing_ffmpeg(app: &AppHandle) -> anyhow::Result<()> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .message(
            "Recording MP4 requires FFmpeg, which was not found on your system.\n\n\
             Install it with your package manager, e.g.\n\
             sudo apt install ffmpeg   (Debian/Ubuntu)\n\
             sudo dnf install ffmpeg   (Fedora)",
        )
        .title("FFmpeg Required")
        .kind(tauri_plugin_dialog::MessageDialogKind::Info)
        .blocking_show();
    Ok(())
}

#[cfg(windows)]
fn handle_missing_ffmpeg(app: &AppHandle) -> anyhow::Result<()> {
    if FFMPEG_DOWNLOADING.load(Ordering::SeqCst) {
        let _ = show_notification(
            "FFmpeg Download",
            "FFmpeg is already downloading in the background. Please wait.",
        );
        return Ok(());
    }

    use tauri_plugin_dialog::DialogExt;
    let is_confirmed = app.dialog()
        .message("Recording MP4 requires FFmpeg, which was not found on your system.\n\nWould you like to automatically download and configure it?")
        .title("FFmpeg Required")
        .kind(tauri_plugin_dialog::MessageDialogKind::Info)
        .buttons(tauri_plugin_dialog::MessageDialogButtons::YesNo)
        .blocking_show();

    if is_confirmed {
        FFMPEG_DOWNLOADING.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = show_notification(
                "FFmpeg Download",
                "Starting FFmpeg download (approx. 90MB)...",
            );
            match perform_ffmpeg_download() {
                Ok(()) => {
                    let _ = show_notification(
                        "FFmpeg Configured",
                        "FFmpeg has been downloaded and configured. You can now record MP4 videos.",
                    );
                }
                Err(e) => {
                    tracing::error!("failed to download ffmpeg: {}", e);
                    let _ = show_notification(
                        "FFmpeg Download Failed",
                        &format!("Could not configure FFmpeg: {}", e),
                    );
                }
            }
            FFMPEG_DOWNLOADING.store(false, Ordering::SeqCst);
        });
    }

    Ok(())
}

fn run_gif_task(task: &CaptureTask, app: &AppHandle) -> anyhow::Result<()> {
    // cancel selection if already active
    if UnifiedSelector::active_selector_active() {
        UnifiedSelector::cancel_active_selection();
        return Ok(());
    }

    let state = app.state::<AppState>();
    let current = *state.recording_state.lock().unwrap();
    let active_id = state.recording_task_id.lock().unwrap().clone();

    if matches!(current, RecordingState::Recording) {
        // same task hotkey re-pressed -> user wants to stop
        // different task hotkey while recording -> reject and tell the user
        if active_id.as_deref() == Some(task.id.as_str()) {
            stop_gif_recording(app);
        } else {
            let active_name = {
                let cfg = state.config.lock().unwrap();
                active_id
                    .as_deref()
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
        // mid-save from a previous run; skip
        return Ok(());
    }

    if matches!(task.capture_mode, TaskCaptureMode::RegionMp4)
        && !crate::recording::is_ffmpeg_available()
    {
        handle_missing_ffmpeg(app)?;
        return Ok(());
    }

    // gate is held only during selection so a screenshot hotkey pressed while
    // the region selector is visible doesn't open a second overlay
    use std::sync::atomic::Ordering as OrdGif;
    if state
        .capture_in_progress
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
        // the selector hands back frozen pixels with the rect; recording only
        // needs the rect
        SelectionResult::FrozenRegion { rect, .. } => rect,
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

    let is_mp4 = matches!(task.capture_mode, TaskCaptureMode::RegionMp4);
    let settings = RecordingSettings {
        fps: if is_mp4 {
            cfg.capture.video_fps
        } else {
            cfg.capture.gif_fps
        },
        max_duration: Duration::from_secs(cfg.capture.gif_max_duration_secs as u64),
        quality: cfg.output.quality,
        video_crf: cfg.capture.video_quality.crf(),
        show_cursor: cfg.capture.show_cursor,
        record_audio: cfg.capture.record_audio,
        format: if is_mp4 {
            crate::recording::RecordingFormat::Mp4
        } else {
            crate::recording::RecordingFormat::Gif
        },
    };

    // steer the wayland frame grabs; windows composites the cursor per frame
    #[cfg(target_os = "linux")]
    crate::capture::set_include_cursor(cfg.capture.show_cursor);

    let mut recorder = GifRecorder::new(settings).with_region(region);
    recorder.start()?;

    *state.gif_recorder.lock().unwrap() = Some(recorder);
    *state.recording_state.lock().unwrap() = RecordingState::Recording;
    *state.recording_task_id.lock().unwrap() = Some(task.id.clone());

    // the overlay's stop button ends the recording the same way a re-pressed
    // hotkey does; the callback fires on the overlay thread, which is safe
    // because stop_gif_recording only touches mutex-guarded state
    let app_for_stop = app.clone();
    RecordingOverlay::start(
        region,
        cfg.capture.gif_max_duration_secs as u64,
        Box::new(move || stop_gif_recording(&app_for_stop)),
    );
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
        // duration -- the thread sets state to processing after its last frame
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !matches!(rec.state(), crate::recording::RecordingState::Processing) {
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    "gif capture thread did not finish within 5s; saving partial frames"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let is_mp4 = matches!(task.capture_mode, TaskCaptureMode::RegionMp4);

        let mut path = cfg.output_path();
        if is_mp4 {
            path.set_extension("mp4");
        } else {
            path.set_extension("gif");
        }
        let path = get_unique_filepath(&path);
        if let Err(e) = std::fs::create_dir_all(&cfg.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }

        let save_result = if is_mp4 {
            rec.save_mp4(&path)
        } else {
            rec.save(&path).map(|_| false)
        };

        match save_result {
            Ok(audio_dropped) => {
                *state.last_save.lock().unwrap() = Some(path.clone());
                Sound::Screenshot.play_if_enabled(cfg.post_capture.play_sound);
                if cfg.ui.show_notifications {
                    let title = if is_mp4 { "Video saved" } else { "GIF saved" };
                    let _ = show_notification(title, &path.to_string_lossy());
                }
                // the user asked for system audio but the track was lost
                if audio_dropped {
                    emit_error(
                        app,
                        "recording",
                        "saved without audio — the system-audio track couldn't be captured or muxed",
                    );
                }
                // a stop the user didn't ask for deserves an explanation
                let early_stop_note = match rec.stop_reason() {
                    Some(StopReason::MaxDuration) => Some(format!(
                        "hit the {}s max duration — raise it under settings → capture",
                        cfg.capture.gif_max_duration_secs
                    )),
                    Some(StopReason::FrameCap) => {
                        Some("hit the frame-count safety limit — recording saved".to_string())
                    }
                    Some(StopReason::DiskFull) => {
                        Some("stopped early: the disk is nearly full — recording saved".to_string())
                    }
                    Some(StopReason::EncoderFailed) => Some(
                        "stopped early: couldn't write frames to disk — saved what was captured"
                            .to_string(),
                    ),
                    _ => None,
                };
                if let Some(note) = early_stop_note {
                    emit_error(app, "recording", &note);
                }
                notify_capture_saved(app, &path);
                apply_gif_post_action(task, app, &path, &cfg);
            }
            Err(e) => {
                let err_type = if is_mp4 { "mp4-save" } else { "gif-save" };
                tracing::warn!("{} failed: {e}", err_type);
                emit_error(app, err_type, &e.to_string());
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

// trim an mp4 recording to [start_secs, start_secs + duration). `fast` stream-
// copies (instant, but the start snaps to the nearest keyframe); otherwise the
// clip is re-encoded for a frame-accurate cut. writes a new file next to the
// source and returns its path. recordings are video-only, so audio is dropped.
#[tauri::command]
pub async fn trim_mp4(
    path: String,
    start_secs: f64,
    end_secs: f64,
    fast: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    // operate on the resolved path so a symlink can't be repointed between this
    // allow-list check and ffmpeg opening the file. strip the \\?\ verbatim
    // prefix canonicalize adds on windows, which trim_mp4_blocking's network-path
    // guard would otherwise reject.
    #[cfg(windows)]
    let resolved = {
        let s = canonical.to_string_lossy();
        match s.strip_prefix(r"\\?\") {
            Some(rest) => match rest.strip_prefix(r"UNC\") {
                Some(unc) => format!(r"\\{unc}"),
                None => rest.to_string(),
            },
            None => s.into_owned(),
        }
    };
    #[cfg(not(windows))]
    let resolved = canonical.to_string_lossy().into_owned();
    tauri::async_runtime::spawn_blocking(move || {
        trim_mp4_blocking(&resolved, start_secs, end_secs, fast).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn trim_mp4_blocking(
    path: &str,
    start_secs: f64,
    end_secs: f64,
    fast: bool,
) -> anyhow::Result<String> {
    use std::path::Path;
    if path.contains("..") {
        anyhow::bail!("path contains directory traversal");
    }
    #[cfg(windows)]
    if path.starts_with("\\\\") {
        anyhow::bail!("network paths not allowed");
    }
    let src = Path::new(path);
    if !src.exists() {
        anyhow::bail!("file not found");
    }
    let is_mp4 = src
        .extension()
        .map(|e| e.eq_ignore_ascii_case("mp4"))
        .unwrap_or(false);
    if !is_mp4 {
        anyhow::bail!("not an mp4 file");
    }

    let start = start_secs.max(0.0);
    let duration = end_secs - start;
    if !duration.is_finite() || duration <= 0.05 {
        anyhow::bail!("trim must be at least 0.05s and end after start");
    }

    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let base = parent.join(format!("{stem}_trim.mp4"));
    let out = crate::clipboard::get_unique_filepath(&base);
    let out_str = out.to_string_lossy().to_string();

    let start_s = format!("{start:.3}");
    let dur_s = format!("{duration:.3}");

    let mut cmd = crate::recording::ffmpeg_command();
    if fast {
        // input-side seek + stream copy: instant, keyframe-aligned start
        cmd.args([
            "-ss", &start_s, "-i", path, "-t", &dur_s, "-c", "copy", "-an", "-y", &out_str,
        ]);
    } else {
        // decode then re-encode for a frame-accurate cut
        cmd.args([
            "-i", path, "-ss", &start_s, "-t", &dur_s, "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-crf", "23", "-an", "-y", &out_str,
        ]);
    }

    let output = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| anyhow::anyhow!("failed to launch ffmpeg: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.trim().lines().last().unwrap_or("ffmpeg error");
        anyhow::bail!("ffmpeg failed: {tail}");
    }
    if !out.exists() {
        anyhow::bail!("ffmpeg produced no output file");
    }
    Ok(out_str)
}

fn apply_gif_post_action(
    task: &CaptureTask,
    app: &AppHandle,
    path: &std::path::Path,
    cfg: &Config,
) {
    match task.post_action {
        TaskPostAction::Clipboard | TaskPostAction::SaveAndClipboard => {
            // animated gif/mp4 can't go on the clipboard as pixels without
            // flattening, so copy the saved file itself (CF_HDROP) — pasting
            // into explorer/discord/slack then inserts the actual file
            #[cfg(any(windows, target_os = "linux"))]
            let copied = match crate::clipboard::copy_file_to_clipboard(path) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("file clipboard copy failed: {e}; falling back to path text");
                    false
                }
            };
            #[cfg(not(any(windows, target_os = "linux")))]
            let copied = false;
            if !copied {
                if let Ok(mut cb) = ClipboardManager::new() {
                    let _ = cb.copy_text(&path.to_string_lossy());
                }
            }
        }
        TaskPostAction::OpenEditor => {
            // recordings can't be annotated without flattening their frames —
            // reveal the saved file in the file manager instead of opening the editor
            reveal_in_file_manager(app, path);
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
                let is_mp4 = path.extension().is_some_and(|ext| ext == "mp4");
                let (mime, default_name) = if is_mp4 {
                    ("video/mp4", "capture.mp4")
                } else {
                    ("image/gif", "capture.gif")
                };
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(default_name);
                match uploader.upload_raw(&bytes, mime, file_name, &service) {
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
        // CopyText (OCR) is a still-only action, filtered out of the recording
        // post-action list in the UI; if one reaches here the file is saved and
        // there's nothing sensible to OCR from a recording
        TaskPostAction::SaveFile
        | TaskPostAction::Prompt
        | TaskPostAction::DoNothing
        | TaskPostAction::CopyText => {
            // already saved to disk; nothing further
        }
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
            TaskPostAction::CopyText => PostActionArg::CopyText,
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
    let _ = app.emit("capscr://capture-saved", path.to_string_lossy().to_string());
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
    // human-readable "area:grant" strings (e.g. "image:read", "fetch:api.x.com")
    // so the plugins tab can show what a plugin was granted at install time
    pub capabilities: Vec<String>,
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

struct PluginListing {
    name: String,
    version: String,
    description: String,
    enabled: bool,
    capabilities: Vec<String>,
}

/// flatten a plugin's `[capabilities]` table into sorted "area:grant" labels
/// (e.g. "clipboard:write", "fetch:https://api.x.com/*") for display
fn flatten_capabilities(caps: &std::collections::HashMap<String, Vec<String>>) -> Vec<String> {
    let mut out: Vec<String> = caps
        .iter()
        .flat_map(|(k, vs)| vs.iter().map(move |v| format!("{k}:{v}")))
        .collect();
    out.sort();
    out
}

/// read the plugins-list fields from a plugin.toml body. tries the sectioned
/// runtime schema first — what real WASM plugins use — then falls back to the
/// legacy flat schema. None if neither parses. without the sectioned attempt,
/// installed WASM plugins (whose name lives under `[plugin]`) silently fail to
/// parse and never appear in the list.
fn read_plugin_listing(body: &str) -> Option<PluginListing> {
    if let Ok(m) = toml::from_str::<crate::plugin::PluginManifest>(body) {
        return Some(PluginListing {
            name: m.plugin.name,
            version: m.plugin.version,
            description: m.plugin.description.unwrap_or_default(),
            enabled: m.enabled,
            capabilities: flatten_capabilities(&m.capabilities),
        });
    }
    if let Ok(m) = toml::from_str::<LegacyFlatManifest>(body) {
        return Some(PluginListing {
            name: m.name,
            version: m.version,
            description: m.description,
            enabled: m.enabled,
            capabilities: Vec::new(),
        });
    }
    None
}

#[cfg(target_os = "linux")]
#[cfg(test)]
mod ocr_locale_tests {
    use super::desired_tess_langs;

    #[test]
    fn maps_a_plain_lang_locale() {
        assert_eq!(
            desired_tess_langs(None, None, Some("de_DE.UTF-8")),
            vec!["deu"]
        );
    }

    #[test]
    fn language_priority_list_wins_and_dedupes() {
        assert_eq!(
            desired_tess_langs(Some("de_DE:en_US:de_AT"), Some("fr_FR.UTF-8"), None),
            vec!["deu", "eng"]
        );
    }

    #[test]
    fn c_and_posix_locales_yield_nothing() {
        assert!(desired_tess_langs(None, Some("C"), Some("POSIX")).is_empty());
    }

    #[test]
    fn chinese_splits_by_region() {
        assert_eq!(
            desired_tess_langs(Some("zh_TW:zh_CN"), None, None),
            vec!["chi_tra", "chi_sim"]
        );
    }

    #[test]
    fn unknown_codes_are_skipped() {
        assert_eq!(
            desired_tess_langs(Some("xx_YY:ja_JP"), None, None),
            vec!["jpn"]
        );
    }

    #[test]
    fn modifiers_and_encodings_are_stripped() {
        assert_eq!(
            desired_tess_langs(None, None, Some("sr_RS.UTF-8@latin")),
            Vec::<&str>::new()
        );
        assert_eq!(
            desired_tess_langs(None, None, Some("uk_UA.KOI8-U")),
            vec!["ukr"]
        );
    }
}

#[cfg(test)]
mod plugin_listing_tests {
    use super::read_plugin_listing;

    #[test]
    fn reads_sectioned_wasm_manifest() {
        let body = "enabled = false\n\
            [plugin]\nid = \"grayscale\"\nname = \"Grayscale\"\nversion = \"0.1.0\"\ndescription = \"gray\"\n\
            [runtime]\ntype = \"wasm\"\nfile = \"plugin.wasm\"\n\
            [hooks]\non_capture = \"capscr_on_capture\"\n\
            [capabilities]\nimage = [\"read\"]\nfetch = [\"https://api.example.com/*\"]\n";
        let listing = read_plugin_listing(body).expect("sectioned parses");
        assert_eq!(listing.name, "Grayscale");
        assert_eq!(listing.version, "0.1.0");
        assert_eq!(listing.description, "gray");
        assert!(!listing.enabled);
        assert_eq!(
            listing.capabilities,
            vec!["fetch:https://api.example.com/*", "image:read"]
        );
    }

    #[test]
    fn reads_legacy_flat_manifest() {
        let body =
            "name = \"Sounds\"\nversion = \"0.1.0\"\ndescription = \"sfx\"\nenabled = true\n";
        let listing = read_plugin_listing(body).expect("flat parses");
        assert_eq!(listing.name, "Sounds");
        assert_eq!(listing.version, "0.1.0");
        assert!(listing.enabled);
        assert!(listing.capabilities.is_empty());
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
        let listing = match read_plugin_listing(&body) {
            Some(t) => t,
            None => {
                tracing::warn!("plugin {:?}: unparseable manifest", path.file_name());
                continue;
            }
        };
        out.push(InstalledPlugin {
            id: entry.file_name().to_string_lossy().to_string(),
            name: listing.name,
            version: listing.version,
            description: listing.description,
            enabled: listing.enabled,
            capabilities: listing.capabilities,
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
pub async fn marketplace_browse(
    state: State<'_, AppState>,
) -> Result<Vec<crate::marketplace::RegistryEntry>, String> {
    let url = state
        .config
        .lock()
        .unwrap()
        .marketplace
        .registry_url
        .clone();
    // reqwest::blocking inside an async command — push to a worker thread so
    // we don't park the tokio runtime.
    tokio::task::spawn_blocking(move || crate::marketplace::fetch_registry(&url))
        .await
        .map_err(|e| e.to_string())?
        .map(|reg| reg.plugins)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn marketplace_install(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let url = state
        .config
        .lock()
        .unwrap()
        .marketplace
        .registry_url
        .clone();
    let plugins = plugins_dir()?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
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
    // the desktop's own description of what actually triggers this shortcut
    // (portal backend only) — it may differ from the configured chord when
    // the compositor remapped it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_trigger: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotkeyDiagnostics {
    pub disabled_globally: bool,
    // which mechanism owns keyboard hotkeys right now:
    // "ll_hook" (windows), "x11", "portal", "evdev", or "none"
    pub backend: String,
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

#[derive(Debug, Clone, Serialize)]
pub struct EvdevStatus {
    pub enabled: bool,
    pub readable_device_count: usize,
    pub dev_input_exists: bool,
}

// settings backing for the advanced-input section: whether the opt-in is on
// and whether /dev/input is actually readable (input-group membership)
#[tauri::command]
pub fn evdev_status() -> EvdevStatus {
    #[cfg(target_os = "linux")]
    {
        EvdevStatus {
            enabled: crate::hotkeys::advanced_input_enabled(),
            readable_device_count: crate::hotkeys::evdev_linux::readable_devices().len(),
            dev_input_exists: std::path::Path::new("/dev/input").exists(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        EvdevStatus {
            enabled: false,
            readable_device_count: 0,
            dev_input_exists: false,
        }
    }
}

// re-run the portal bind for the current set even when unchanged, so a user
// who dismissed the desktop's approval dialog can bring it back
#[tauri::command]
pub fn portal_rebind_shortcuts(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        crate::hotkeys::portal_linux::rebind().map_err(|e| format!("{e:#}"))?;
        // re-flush so statuses reflect the fresh outcome
        let tasks = state.config.lock().unwrap().capture_tasks.clone();
        state.send_hotkey_reload(tasks);
        let _ = app.emit("capscr://hotkey-status", ());
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, state);
        Err("the shortcuts portal only exists on Linux".to_string())
    }
}

fn hotkey_backend_name() -> String {
    #[cfg(windows)]
    {
        "ll_hook".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        if !crate::capture::is_wayland_session() {
            "x11".to_string()
        } else if crate::hotkeys::portal_linux::available() {
            "portal".to_string()
        } else if crate::hotkeys::advanced_input_enabled() {
            "evdev".to_string()
        } else {
            "none".to_string()
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        "none".to_string()
    }
}

#[tauri::command]
pub fn hotkey_diagnostics(state: State<AppState>) -> HotkeyDiagnostics {
    use std::sync::atomic::Ordering;
    let disabled = state.hotkeys_disabled.load(Ordering::SeqCst);
    #[cfg(target_os = "linux")]
    let effective = crate::hotkeys::portal_linux::effective_triggers();
    #[cfg(not(target_os = "linux"))]
    let effective: std::collections::HashMap<String, String> = Default::default();
    let status = state.hotkey_status.lock().unwrap();
    let statuses = status
        .iter()
        .map(|(task_id, st)| match st {
            HotkeyStatus::Live => HotkeyStatusEntry {
                task_id: task_id.clone(),
                status: "live".to_string(),
                reason: None,
                effective_trigger: effective.get(task_id).cloned(),
            },
            HotkeyStatus::Failed { reason } => HotkeyStatusEntry {
                task_id: task_id.clone(),
                status: "failed".to_string(),
                reason: Some(reason.clone()),
                effective_trigger: None,
            },
        })
        .collect();
    HotkeyDiagnostics {
        disabled_globally: disabled,
        backend: hotkey_backend_name(),
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
        "Imgur" | "imgur" => crate::upload::test_connection_imgur(&cfg.upload.imgur_client_id)
            .map_err(|e| e.to_string())?,
        "Custom" | "custom" => {
            let uploader = crate::upload::CustomUploader {
                name: "Custom".to_string(),
                request_url: cfg.upload.custom_url.clone(),
                file_form_name: cfg.upload.custom_form_name.clone(),
                response_url_path: cfg.upload.custom_response_path.clone(),
            };
            crate::upload::test_connection_custom(&uploader).map_err(|e| e.to_string())?
        }
        "S3" | "s3" => {
            let target = crate::upload::S3Target {
                bucket: cfg.upload.s3.bucket.clone(),
                region: cfg.upload.s3.region.clone(),
                endpoint: cfg.upload.s3.endpoint.clone(),
                access_key_id: cfg.upload.s3.access_key_id.clone(),
                secret_access_key: cfg.upload.s3.secret_access_key_plaintext(),
                public_url_template: cfg.upload.s3.public_url_template.clone(),
            };
            crate::upload::test_connection_s3(&target).map_err(|e| e.to_string())?
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
    // no global hook to arm here — the hub window has focus while binding, so
    // HotkeyInput's browser-side keydown capture records the chord itself
    Ok(())
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
    // the store mirrors hotkeys.disabled_globally too; nudge it to refetch so a
    // later Settings save doesn't persist a stale (enabled) value over the switch
    let _ = app.emit("capscr://config-updated", ());
    Ok(())
}

#[tauri::command]
pub fn run_ocr(path: String, state: State<AppState>) -> Result<String, String> {
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    ocr_image_bytes(&bytes).map_err(|e| e.to_string())
}

/// run Windows OCR over an encoded image (png/jpeg/...) and return the text
#[cfg(windows)]
fn ocr_image_bytes(image_bytes: &[u8]) -> anyhow::Result<String> {
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| anyhow::anyhow!("Failed to create OCR engine: {:?}", e))?;
    let stream = InMemoryRandomAccessStream::new()
        .map_err(|e| anyhow::anyhow!("Failed to create in-memory stream: {:?}", e))?;
    let writer = DataWriter::CreateDataWriter(&stream)
        .map_err(|e| anyhow::anyhow!("Failed to create data writer: {:?}", e))?;
    writer
        .WriteBytes(image_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to write bytes: {:?}", e))?;
    writer
        .StoreAsync()?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to store data: {:?}", e))?;
    writer
        .FlushAsync()?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to flush stream: {:?}", e))?;
    stream
        .Seek(0)
        .map_err(|e| anyhow::anyhow!("Failed to seek stream: {:?}", e))?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to decode image: {:?}", e))?;
    let software_bitmap = decoder
        .GetSoftwareBitmapAsync()?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to get software bitmap: {:?}", e))?;
    let result = engine
        .RecognizeAsync(&software_bitmap)?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to run OCR: {:?}", e))?;
    Ok(result.Text()?.to_string())
}

// windows OCR picks the user's profile languages; mirror that by mapping the
// session locale onto tesseract's traineddata codes
#[cfg(target_os = "linux")]
fn locale_to_tess(lang: &str, region: Option<&str>) -> Option<&'static str> {
    Some(match lang {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "ko" => "kor",
        "nl" => "nld",
        "pl" => "pol",
        "sv" => "swe",
        "fi" => "fin",
        "da" => "dan",
        "nb" | "no" => "nor",
        "cs" => "ces",
        "hu" => "hun",
        "tr" => "tur",
        "uk" => "ukr",
        "el" => "ell",
        "ro" => "ron",
        "bg" => "bul",
        "ar" => "ara",
        "he" => "heb",
        "hi" => "hin",
        "th" => "tha",
        "vi" => "vie",
        "zh" => match region {
            Some("TW") | Some("HK") | Some("MO") => "chi_tra",
            _ => "chi_sim",
        },
        _ => return None,
    })
}

// LANGUAGE is a colon-separated priority list; LC_ALL then LANG hold single
// locales like de_DE.UTF-8. C/POSIX mean "no preference".
#[cfg(target_os = "linux")]
fn desired_tess_langs(
    language: Option<&str>,
    lc_all: Option<&str>,
    lang: Option<&str>,
) -> Vec<&'static str> {
    let raw: Vec<&str> = match language.filter(|v| !v.is_empty()) {
        Some(list) => list.split(':').collect(),
        None => lc_all
            .filter(|v| !v.is_empty())
            .or(lang.filter(|v| !v.is_empty()))
            .into_iter()
            .collect(),
    };
    let mut mapped = Vec::new();
    for locale in raw {
        let locale = locale.split(['.', '@']).next().unwrap_or(locale);
        if locale.is_empty() || locale == "C" || locale == "POSIX" {
            continue;
        }
        let (lang, region) = match locale.split_once('_') {
            Some((lang, region)) => (lang, Some(region)),
            None => (locale, None),
        };
        if let Some(code) = locale_to_tess(lang, region) {
            if !mapped.contains(&code) {
                mapped.push(code);
            }
        }
    }
    mapped
}

#[cfg(target_os = "linux")]
fn installed_tess_langs() -> &'static std::collections::HashSet<String> {
    static LANGS: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    LANGS.get_or_init(|| {
        std::process::Command::new("tesseract")
            .arg("--list-langs")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .skip(1)
                    .map(|line| line.trim().to_string())
                    .filter(|line| !line.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
}

// `-l deu+eng` style; None falls back to tesseract's default silently
#[cfg(target_os = "linux")]
fn tesseract_lang_args() -> Option<String> {
    let installed = installed_tess_langs();
    let desired: Vec<&str> = desired_tess_langs(
        std::env::var("LANGUAGE").ok().as_deref(),
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LANG").ok().as_deref(),
    )
    .into_iter()
    .filter(|code| installed.contains(*code))
    .collect();
    if desired.is_empty() {
        return None;
    }
    Some(desired.join("+"))
}

/// run tesseract over an encoded image and return the text. tesseract is the
/// packaged OCR engine on every major distro; the error message points at the
/// package when it's missing so the post-action isn't a dead end
#[cfg(target_os = "linux")]
fn ocr_image_bytes(image_bytes: &[u8]) -> anyhow::Result<String> {
    use std::io::Write;
    let mut command = std::process::Command::new("tesseract");
    command.args(["stdin", "stdout"]);
    if let Some(langs) = tesseract_lang_args() {
        tracing::debug!("ocr languages: {langs}");
        command.args(["-l", &langs]);
    }
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| anyhow::anyhow!("OCR needs tesseract — install the tesseract-ocr package"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("tesseract stdin unavailable"))?
        .write_all(image_bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("tesseract failed with status {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn ocr_image_bytes(_image_bytes: &[u8]) -> anyhow::Result<String> {
    anyhow::bail!("OCR is not supported on this platform")
}

/// static grid thumbnail for the history view. full-size animated gifs
/// dropped straight into <img> tags decode to gigabytes across a grid, and
/// files outside the asset-protocol scope render blank; a cached first-frame
/// jpeg in the app cache dir solves both. keyed by path + size + mtime.
#[tauri::command]
pub async fn history_thumbnail(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("thumbs");
    tokio::task::spawn_blocking(move || {
        thumbnail_for(&cache_dir, &canonical).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
    .map(|thumb| thumb.to_string_lossy().to_string())
}

fn thumbnail_for(cache_dir: &std::path::Path, path: &std::path::Path) -> anyhow::Result<PathBuf> {
    use std::hash::{Hash, Hasher};
    const THUMB_WIDTH: u32 = 480;
    let meta = std::fs::metadata(path)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    if let Ok(modified) = meta.modified() {
        if let Ok(age) = modified.duration_since(std::time::UNIX_EPOCH) {
            age.as_secs().hash(&mut hasher);
        }
    }
    std::fs::create_dir_all(cache_dir)?;
    let thumb = cache_dir.join(format!("{:016x}.jpg", hasher.finish()));
    if thumb.exists() {
        return Ok(thumb);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let frame: RgbaImage = if ext == "gif" {
        // first frame only; the grid shows a still, the viewer animates
        use image::AnimationDecoder;
        let file = std::io::BufReader::new(std::fs::File::open(path)?);
        let decoder = image::codecs::gif::GifDecoder::new(file)?;
        decoder
            .into_frames()
            .next()
            .ok_or_else(|| anyhow::anyhow!("gif has no frames"))??
            .into_buffer()
    } else {
        image::open(path)?.to_rgba8()
    };
    let scale = (THUMB_WIDTH as f32 / frame.width() as f32).min(1.0);
    let width = ((frame.width() as f32 * scale) as u32).max(1);
    let height = ((frame.height() as f32 * scale) as u32).max(1);
    let small = image::imageops::thumbnail(&frame, width, height);
    let rgb = image::DynamicImage::ImageRgba8(small).to_rgb8();
    // atomic-ish: encode to a temp name then rename, so a torn write never
    // caches as a valid thumb
    let staging = thumb.with_extension("jpg.tmp");
    rgb.save_with_format(&staging, image::ImageFormat::Jpeg)?;
    std::fs::rename(&staging, &thumb)?;
    Ok(thumb)
}

/// OCR a freshly captured image by encoding it to PNG in memory first
fn ocr_capture(image: &RgbaImage) -> anyhow::Result<String> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| anyhow::anyhow!("failed to encode capture for OCR: {e}"))?;
    ocr_image_bytes(&png)
}

#[tauri::command]
pub async fn pin_image(
    path: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use uuid::Uuid;
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    let path = canonical.to_string_lossy().to_string();
    let label = format!("pin_{}", Uuid::new_v4());

    state
        .pinned_images
        .lock()
        .unwrap()
        .insert(label.clone(), path);

    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(&app, &label, url)
        .title("capscr — pinned")
        .decorations(false)
        .resizable(true)
        .always_on_top(true)
        .transparent(true)
        .visible(false);

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon).map_err(|e| e.to_string())?;
    }

    let window = builder.build().map_err(|e| e.to_string())?;
    watch_pin_navigation(&app, window);

    // always_on_top is a no-op on wayland; flag the pin keep-above through
    // kwin once its window maps (the frontend shows it asynchronously)
    #[cfg(target_os = "linux")]
    if crate::capture::gui_is_wayland() {
        std::thread::spawn(|| {
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(300));
                match crate::capture::keep_own_windows_above("capscr — pinned") {
                    Ok(count) if count > 0 => {
                        tracing::debug!("flagged {count} pinned window(s) keep-above");
                        return;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::debug!("keep-above unavailable: {e:#}");
                        return;
                    }
                }
            }
            tracing::warn!("pin window never appeared in the window list");
        });
    }
    Ok(())
}

fn watch_pin_navigation(app: &AppHandle, window: tauri::WebviewWindow) {
    let app = app.clone();
    std::thread::spawn(move || {
        for wait_ms in [500u64, 1500, 3000] {
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            match window.url() {
                Ok(url) if url.scheme() != "about" => {
                    remember_canonical_url(&app, &url);
                    return;
                }
                Err(_) => return,
                _ => {}
            }
            tracing::warn!("pin webview stuck on about:blank; navigating explicitly");
            if let Some(url) = canonical_app_url(&app) {
                if let Err(e) = window.navigate(url) {
                    tracing::warn!("pin explicit navigation failed: {e}");
                }
            }
        }
    });
}

#[tauri::command]
pub fn get_pinned_image_path(label: String, state: State<'_, AppState>) -> Option<String> {
    state.pinned_images.lock().unwrap().get(&label).cloned()
}
