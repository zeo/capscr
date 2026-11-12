use super::*;

#[tauri::command]
pub fn take_screenshot(
    mode: CaptureModeArg,
    post: TaskPostAction,
    app: AppHandle,
) -> Result<(), String> {
    let app_handle = app;
    std::thread::spawn(move || {
        let outcome = catch_panic(
            || run_capture_pipeline(mode, post, &app_handle),
            "capture pipeline",
        );
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

fn catch_panic<T>(operation: impl FnOnce() -> anyhow::Result<T>, label: &str) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
        .map_err(|panic_info| anyhow::anyhow!("{label} panicked: {}", panic_message(panic_info)))
        .and_then(|outcome| outcome)
}

fn panic_message(panic_info: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic_info.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = panic_info.downcast_ref::<String>() {
        message.clone()
    } else {
        "internal panic (no message)".to_string()
    }
}

#[cfg(test)]
mod task_failure_tests {
    use super::*;

    #[test]
    fn task_panics_become_errors() {
        let error = catch_panic(
            || -> anyhow::Result<()> { panic!("selector failed") },
            "task",
        )
        .expect_err("the panic should be converted to an error");
        assert_eq!(error.to_string(), "task panicked: selector failed");
    }
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
        if cfg!(windows) {
            "Windows blocked the capture. Run capscr as administrator or check Settings > Privacy > Screen recording.".into()
        } else {
            "The desktop blocked the capture. If a screen-sharing prompt appeared, allow it and try again; `capscr --wayland-diag` shows which capture sources this session grants.".into()
        }
    } else if s.contains("hdr") {
        if cfg!(windows) {
            "HDR capture failed. Turn HDR off in Windows display settings, or disable HDR in capscr Settings > capture > hdr.".into()
        } else {
            "HDR capture failed. Disable HDR in capscr Settings > capture > hdr.".into()
        }
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
    post: TaskPostAction,
    app: &AppHandle,
) -> anyhow::Result<()> {
    run_capture_pipeline_inner(mode, post, app, None, None)
}

pub fn run_capture_pipeline_with_target(
    mode: CaptureModeArg,
    post: TaskPostAction,
    app: &AppHandle,
    upload_target: Option<crate::config::TaskUploadTarget>,
    delay_override: Option<u32>,
) -> anyhow::Result<()> {
    run_capture_pipeline_inner(mode, post, app, upload_target, delay_override)
}

pub(crate) struct CaptureGate<'a>(pub(crate) &'a std::sync::atomic::AtomicBool);

impl Drop for CaptureGate<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

fn run_capture_pipeline_inner(
    mode: CaptureModeArg,
    post: TaskPostAction,
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
                // with the companion extension the capscr selector picks
                // windows on gnome too; fall through to it like on kde/x11
                Err(_)
                    if crate::shell::desktop() == crate::shell::DesktopEnv::Gnome
                        && crate::capture::gnome_shell::available() =>
                {
                    tracing::debug!("using the capscr selector via the companion extension");
                    None
                }
                // gnome without the companion offers no window list to
                // ordinary apps; its own picker through the portal's
                // interactive mode is the only sanctioned window-pick there
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

                if let Some((cropped, at)) = crop_frozen(
                    frozen,
                    (min_x, min_y),
                    (rect.x, rect.y),
                    (rect.width, rect.height),
                ) {
                    (cropped, None, Some(at))
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

                        let size = (
                            rect.right.saturating_sub(rect.left).max(0) as u32,
                            rect.bottom.saturating_sub(rect.top).max(0) as u32,
                        );
                        if let Some((cropped, at)) =
                            crop_frozen(frozen, (min_x, min_y), (rect.left, rect.top), size)
                        {
                            (cropped, None, Some(at))
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
                        crop_frozen(frozen, (min_x, min_y), (w_x, w_y), (w_w, w_h))
                            .map(|(img, at)| (img, None, Some(at)))
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
        // output_name only steers the linux wayland path
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
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

    if matches!(post, TaskPostAction::OpenEditor | TaskPostAction::Prompt) {
        let config = state.config.lock().unwrap().clone();
        if let Err(e) = std::fs::create_dir_all(&config.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }
        let path = get_unique_filepath(&config.output_path());
        let t0 = std::time::Instant::now();
        if let Err(e) =
            crate::clipboard::save_image(&image, &path, config.output.format, config.output.quality)
        {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        tracing::info!(
            "editor save {}ms {:?}",
            t0.elapsed().as_millis(),
            config.output.format
        );
        maybe_write_hdr_sidecar(&path, &hdr_bitmap, &config);
        *state.last_save.lock().unwrap() = Some(path.clone());
        notify_capture_saved(app, sized_save(&path, Some(image.dimensions())));
        open_editor_window(app, &path.to_string_lossy()).map_err(|e| anyhow::anyhow!(e))?;
        Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
        if config.ui.show_notifications {
            let _ = show_notification("Capture opened", &path.to_string_lossy());
        }
        return Ok(());
    }

    if matches!(post, TaskPostAction::Annotate) {
        // annotate-before-save: stage the fresh capture in the app cache
        // (asset-scope reachable, invisible to History) and open it in the
        // editor. saving there mints the real capture through the staging
        // redirect in save_edited_image; closing without saving discards it.
        let config = state.config.lock().unwrap().clone();
        let stage_dir = app
            .path()
            .app_cache_dir()
            .map_err(|e| anyhow::anyhow!(e))?
            .join("annotate-stage");
        std::fs::create_dir_all(&stage_dir).map_err(|e| anyhow::anyhow!(e))?;
        let path = stage_dir.join(format!(
            "annotate-{}.png",
            uuid::Uuid::new_v4().as_simple()
        ));
        crate::clipboard::save_image(
            &image,
            &path,
            crate::config::ImageFormat::Png,
            config.output.quality,
        )?;
        open_editor_window(app, &path.to_string_lossy()).map_err(|e| anyhow::anyhow!(e))?;
        Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
        if config.ui.show_notifications {
            let _ = show_notification("Annotating capture", "save to keep it, close to discard");
        }
        return Ok(());
    }

    if matches!(post, TaskPostAction::CopyText) {
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

    let result = run_post_action(app, &state, image.clone(), hdr_bitmap, post, upload_target);
    result.map(|_| ())
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
        use crate::capture::HdrCapture;
        let monitor = crate::capture::active_wayland_monitor()?;
        let center = (
            monitor.x + monitor.width as i32 / 2,
            monitor.y + monitor.height as i32 / 2,
        );
        // gnome 50 hands out hdr pixels over a screencast stream; everywhere
        // else is_hdr_at_point stays false and the sdr grab below runs
        if HdrCapture::is_hdr_at_point(center.0, center.1) {
            match HdrCapture::new().capture_with_hdr_at(Some(center)) {
                Ok(pair) => return Ok(pair),
                Err(e) => {
                    tracing::warn!("wayland HDR capture failed — SDR fallback: {e:#}")
                }
            }
        }
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

// crops a desktop-space rect out of a frozen frame whose top-left pixel sits
// at `origin`. a rect hanging past the top or left edge shrinks rather than
// shifting, and the crop comes back with its own desktop-space top-left
fn crop_frozen(
    frozen: &RgbaImage,
    origin: (i32, i32),
    (x, y): (i32, i32),
    (w, h): (u32, u32),
) -> Option<(RgbaImage, (i32, i32))> {
    let (left, top) = (x.max(origin.0), y.max(origin.1));
    let img_x = left.abs_diff(origin.0);
    let img_y = top.abs_diff(origin.1);
    let w = w
        .saturating_sub(left.abs_diff(x))
        .min(frozen.width().saturating_sub(img_x));
    let h = h
        .saturating_sub(top.abs_diff(y))
        .min(frozen.height().saturating_sub(img_y));
    (w > 0 && h > 0).then(|| {
        let crop = image::imageops::crop_imm(frozen, img_x, img_y, w, h).to_image();
        (crop, (left, top))
    })
}

#[cfg(test)]
mod crop_frozen_tests {
    use super::*;

    fn desktop() -> RgbaImage {
        RgbaImage::from_fn(4, 2, |x, y| image::Rgba([x as u8, y as u8, 0, 255]))
    }

    fn columns(img: &RgbaImage) -> Vec<(u8, u8)> {
        img.pixels().map(|p| (p[0], p[1])).collect()
    }

    #[test]
    fn crop_frozen_takes_a_rect_inside_the_frame() {
        let (img, at) = crop_frozen(&desktop(), (-10, 5), (-9, 6), (2, 1)).unwrap();
        assert_eq!(at, (-9, 6));
        assert_eq!(columns(&img), [(1, 1), (2, 1)]);
    }

    #[test]
    fn crop_frozen_shrinks_a_rect_left_of_the_origin() {
        let (img, at) = crop_frozen(&desktop(), (-10, 5), (-12, 5), (4, 2)).unwrap();
        assert_eq!(at, (-10, 5));
        assert_eq!(columns(&img), [(0, 0), (1, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn crop_frozen_clamps_a_rect_past_the_right_edge() {
        let (img, at) = crop_frozen(&desktop(), (0, 0), (3, 0), (5, 9)).unwrap();
        assert_eq!(at, (3, 0));
        assert_eq!(columns(&img), [(3, 0), (3, 1)]);
    }

    #[test]
    fn crop_frozen_rejects_a_rect_outside_the_frame() {
        assert!(crop_frozen(&desktop(), (0, 0), (4, 0), (2, 2)).is_none());
        assert!(crop_frozen(&desktop(), (0, 0), (-3, 0), (3, 2)).is_none());
        assert!(crop_frozen(&desktop(), (0, 0), (0, 0), (0, 2)).is_none());
    }
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

pub(crate) struct PendingFileMutation(AppHandle);

impl PendingFileMutation {
    pub(crate) fn begin(app: &AppHandle) -> Result<Self, &'static str> {
        let state = app.state::<AppState>();
        if state
            .shutdown_in_progress
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err("application shutdown is already in progress");
        }
        state
            .pending_file_mutations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if state
            .shutdown_in_progress
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            state
                .pending_file_mutations
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return Err("application shutdown is already in progress");
        }
        Ok(Self(app.clone()))
    }
}

impl Drop for PendingFileMutation {
    fn drop(&mut self) {
        self.0
            .state::<AppState>()
            .pending_file_mutations
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

// returns Some(path) when a fresh file was written by this call
// callers use it to tie the HDR sidecar to the right basename because
// state.last_save may refer to an earlier clipboard-only capture
fn run_post_action(
    app: &AppHandle,
    state: &AppState,
    image: Arc<RgbaImage>,
    hdr_bitmap: Option<crate::capture::HdrBitmap>,
    action: TaskPostAction,
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
        let pending_write = PendingFileMutation::begin(&app_handle).map_err(anyhow::Error::msg)?;
        if let Err(e) = std::fs::create_dir_all(&config.output.directory) {
            tracing::warn!("failed to create output dir: {e}");
        }
        let path = get_unique_filepath(&config.output_path());

        let path_clone = path.clone();
        let format = config.output.format;
        let quality = config.output.quality;
        let config_clone = config.clone();
        std::thread::spawn(move || {
            let _pending_write = pending_write;
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
                    notify_capture_saved(
                        &app_handle,
                        sized_save(&path_clone, Some(img.dimensions())),
                    );
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
        let pending_write = match PendingFileMutation::begin(&app_handle) {
            Ok(pending_write) => pending_write,
            Err(error) => {
                tracing::warn!("history save skipped: {error}");
                return None;
            }
        };
        let history_dir = history_dir()?;
        if let Err(e) = std::fs::create_dir_all(&history_dir) {
            tracing::warn!("failed to create history dir: {e}");
            return None;
        }
        let path = get_unique_filepath(&history_dir.join(config.generate_filename()));

        let path_clone = path.clone();
        let format = config.output.format;
        let quality = config.output.quality;
        let config_clone = config.clone();
        std::thread::spawn(move || {
            let _pending_write = pending_write;
            let t0 = std::time::Instant::now();
            if let Err(e) = save_image(&img, &path_clone, format, quality) {
                tracing::error!("Background save_image to history failed: {e:#}");
                let _ = std::fs::remove_file(&path_clone);
            } else {
                maybe_write_hdr_sidecar(&path_clone, &hdr, &config_clone);
                tracing::info!(
                    "Background save to history completed in {}ms",
                    t0.elapsed().as_millis()
                );
                notify_capture_saved(
                    &app_handle,
                    sized_save(&path_clone, Some(img.dimensions())),
                );
            }
        });
        Some(path)
    };

    let do_clipboard = || -> anyhow::Result<()> {
        let mut cb = ClipboardManager::new()?;
        cb.copy_image(&image)?;
        Ok(())
    };

    let do_upload = || -> anyhow::Result<(crate::upload::UploadResult, &'static str)> {
        let uploader = crate::upload::shared_uploader()?;
        let service = build_upload_service_for_target(&config, upload_target);
        Ok((uploader.upload(&image, &service)?, service.label()))
    };

    match action {
        TaskPostAction::SaveFile => {
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
        TaskPostAction::Clipboard => {
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
        TaskPostAction::SaveAndClipboard => {
            // the encode and the clipboard copy overlap; the toast waits for both
            let (clip_tx, clip_rx) = std::sync::mpsc::channel::<bool>();
            let play = config.post_capture.play_sound;
            let show = config.ui.show_notifications;
            let path = do_save_async(
                image.clone(),
                hdr_bitmap.clone(),
                app.clone(),
                Box::new(move |path| {
                    let clipboard_ok = clip_rx.recv().unwrap_or(false);
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
            );
            let _ = clip_tx.send(do_clipboard().is_ok());
            Ok(Some(path?))
        }
        TaskPostAction::Upload => {
            let history_path =
                do_save_to_history_async(image.clone(), hdr_bitmap.clone(), app.clone());
            let (result, destination) = do_upload()?;
            finish_upload(app, &config, &result, destination, history_path.as_deref());
            Sound::Upload.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Uploaded", &result.url);
            }
            Ok(history_path)
        }
        TaskPostAction::DoNothing => {
            Sound::Screenshot.play_if_enabled(config.post_capture.play_sound);
            if config.ui.show_notifications {
                let _ = show_notification("Capture complete", "Screenshot taken successfully.");
            }
            Ok(None)
        }
        // take_screenshot handles these itself before it gets here
        TaskPostAction::OpenEditor
        | TaskPostAction::Annotate
        | TaskPostAction::Prompt
        | TaskPostAction::CopyText => unreachable!(),
    }
}

pub fn trigger_task(app: &AppHandle, task_id: &str) {
    if task_id == crate::state::PAUSE_BINDING_ID {
        crate::recbar::toggle_pause(app);
        return;
    }
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
        emit_error(
            app,
            "task",
            &format!("no capture task has the id {task_id}"),
        );
        return;
    };
    let app_handle = app.clone();
    let task_label = task.name.clone();
    std::thread::spawn(move || {
        let outcome = catch_panic(|| run_task(&task, &app_handle), "task");
        if let Err(e) = outcome {
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
    run_capture_pipeline_with_target(
        mode,
        task.post_action,
        app,
        task.target_destination,
        task.delay_ms,
    )
}

// whether this platform can deliver hdr pixels at all, gating the hdr
// settings pane and history filter: always on windows, gnome-on-wayland on
// linux (the screencast portal's 10-bit pq path), nowhere else
#[tauri::command]
pub fn hdr_pipeline_supported() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        crate::shell::desktop() == crate::shell::DesktopEnv::Gnome
            && crate::capture::is_wayland_session()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        false
    }
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
