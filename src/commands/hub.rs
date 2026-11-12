use super::*;

pub(crate) const HUB_LABEL: &str = "hub";
const HUB_MIN_WIDTH: f64 = 720.0;
const HUB_MIN_HEIGHT: f64 = 480.0;
const WINDOW_MARGIN: u32 = 16;

// webview2 routes http://tauri.localhost through the system proxy stack, so a
// vpn or global proxy that doesn't exempt loopback renders every window as
// "localhost refused to connect". all webview content here is local, so force
// a direct connection; uploads and update checks run through reqwest on the
// rust side and still honor the system proxy. wry's --disable-features
// defaults are re-passed because additional_browser_args replaces them, and
// every window must get the identical string: webviews share one browser
// process and mismatched args fail creation (tauri-apps/tauri#11144)
#[cfg(windows)]
pub(crate) const WEBVIEW_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --proxy-server=direct://";

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
        *app.state::<AppState>()
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
            let _ = window.eval("if (document.getElementById('boot')) window.location.reload()");
        }
    });
}

// called in setup() so the WebView2 instance is warm before the user opens
// the tray. Without this, the first tray click pays the full WebView2 cold-
// boot cost (multi-second on most machines, >1min on some).
pub fn prewarm_hub_window(app: &tauri::App) -> tauri::Result<()> {
    let free_memory = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .ui
        .free_memory_when_hidden;
    if free_memory || app.get_webview_window(HUB_LABEL).is_some() {
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(840.0, 560.0)
        .min_inner_size(HUB_MIN_WIDTH, HUB_MIN_HEIGHT)
        .resizable(true)
        .decorations(false)
        .visible(false);

    #[cfg(windows)]
    {
        builder = builder.additional_browser_args(WEBVIEW_BROWSER_ARGS);
    }

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon)?;
    }

    let window = builder.build()?;
    restore_hub_state(&window);
    // intercept the close button so the WebView2 process stays alive for the
    // next tray-click. Without this we pay multi-second cold-boot every time
    // the user closes and re-opens the hub, even after the startup prewarm.
    intercept_hub_close(window.clone());
    heal_stuck_boot(window);
    Ok(())
}

fn restore_hub_state(window: &tauri::WebviewWindow) {
    let flags = WindowStateFlags::all() & !WindowStateFlags::VISIBLE;
    let _ = window.restore_state(flags);
    grow_window_once(window, (0.70, 0.75), (840.0, 560.0));
    fit_window_to_work_area(window, HUB_MIN_WIDTH, HUB_MIN_HEIGHT);
}

// the first open of each window (including the first launch after this
// shipped, over an older saved size) takes a share of its monitor's work
// area; from then on the window-state plugin keeps whatever the user picks
pub(crate) fn grow_window_once(
    window: &tauri::WebviewWindow,
    share: (f64, f64),
    floor: (f64, f64),
) {
    let app = window.app_handle();
    let state = app.state::<AppState>();
    let label = window.label().to_string();
    if state.config.lock().unwrap().ui.grown_windows.contains(&label) {
        return;
    }
    let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.inner_size()) else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let floor = tauri::LogicalSize::new(floor.0, floor.1).to_physical::<u32>(scale);
    let work_area = monitor.work_area().size;
    let width = ((work_area.width as f64 * share.0) as u32).max(floor.width);
    let height = ((work_area.height as f64 * share.1) as u32).max(floor.height);
    if size.width < width || size.height < height {
        let _ = window.set_size(tauri::PhysicalSize::new(
            size.width.max(width),
            size.height.max(height),
        ));
        let _ = window.center();
    }
    let Ok(_pending_mutation) = PendingFileMutation::begin(app) else {
        return;
    };
    let mut stored = state.config.lock().unwrap();
    let mut config = stored.clone();
    config.ui.grown_windows.push(label);
    match config.save_and_migrate_secrets() {
        Ok(()) => *stored = config,
        Err(error) => tracing::warn!("failed to record grown window: {error}"),
    }
}

fn clamp_window_axis(current: u32, work_area: u32, minimum: u32) -> u32 {
    let maximum = work_area.saturating_sub(WINDOW_MARGIN).max(1);
    current.max(minimum.min(maximum)).min(maximum)
}

pub(crate) fn fit_window_to_work_area(
    window: &tauri::WebviewWindow,
    min_width: f64,
    min_height: f64,
) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let Ok(size) = window.inner_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let minimum = tauri::LogicalSize::new(min_width, min_height).to_physical::<u32>(scale);
    let work_area = monitor.work_area();
    let width = clamp_window_axis(size.width, work_area.size.width, minimum.width);
    let height = clamp_window_axis(size.height, work_area.size.height, minimum.height);
    if width != size.width || height != size.height {
        let _ = window.set_size(tauri::PhysicalSize::new(width, height));
    }
    let Ok(position) = window.outer_position() else {
        return;
    };
    let max_x = work_area
        .position
        .x
        .saturating_add_unsigned(work_area.size.width.saturating_sub(width));
    let max_y = work_area
        .position
        .y
        .saturating_add_unsigned(work_area.size.height.saturating_sub(height));
    let x = position.x.clamp(work_area.position.x, max_x);
    let y = position.y.clamp(work_area.position.y, max_y);
    if x != position.x || y != position.y {
        let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HubCloseRequest {
    request_id: String,
    force_exit: bool,
}

fn emit_hub_close_request(
    app: &AppHandle,
    window: &tauri::WebviewWindow,
    force_exit: bool,
) -> bool {
    let request_id = uuid::Uuid::new_v4().as_simple().to_string();
    app.state::<AppState>()
        .pending_hub_close_requests
        .lock()
        .unwrap()
        .insert(request_id.clone());
    let request = HubCloseRequest {
        request_id: request_id.clone(),
        force_exit,
    };
    if window.emit("capscr://close-requested", request).is_err() {
        app.state::<AppState>()
            .pending_hub_close_requests
            .lock()
            .unwrap()
            .remove(&request_id);
        return false;
    }

    let fallback_app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let pending = fallback_app
            .state::<AppState>()
            .pending_hub_close_requests
            .lock()
            .unwrap()
            .remove(&request_id);
        if pending {
            let close_app = fallback_app.clone();
            let direct_app = fallback_app.clone();
            if fallback_app
                .run_on_main_thread(move || complete_hub_close_with_report(force_exit, close_app))
                .is_err()
            {
                complete_hub_close_with_report(force_exit, direct_app);
            }
        }
    });
    true
}

fn intercept_hub_close(window: tauri::WebviewWindow) {
    let app = window.app_handle().clone();
    window.clone().on_window_event(move |event| {
        match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                if !emit_hub_close_request(&app, &window, false) {
                    complete_hub_close_with_report(false, app.clone());
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

pub fn request_hub_close(app: &AppHandle, force_exit: bool) {
    if let Some(window) = app.get_webview_window(HUB_LABEL) {
        if emit_hub_close_request(app, &window, force_exit) {
            return;
        }
    }
    complete_hub_close_with_report(force_exit, app.clone());
}

#[tauri::command]
pub fn acknowledge_hub_close(request_id: String, state: State<'_, AppState>) -> bool {
    state
        .pending_hub_close_requests
        .lock()
        .unwrap()
        .remove(&request_id)
}

#[tauri::command]
pub fn complete_hub_close(force_exit: bool, app: AppHandle) -> Result<(), String> {
    if force_exit {
        require_editor_closed(&app, "exiting capscr")?;
        exit_app(app);
        return Ok(());
    }
    let ui = app.state::<AppState>().config.lock().unwrap().ui.clone();
    match ui.close_behavior {
        crate::config::CloseBehavior::MinimizeToTray => {
            let Some(window) = app.get_webview_window(HUB_LABEL) else {
                return Ok(());
            };
            // on linux the hidden hub's webkit processes would keep ~100mb
            // resident, so it is always destroyed and recreated on demand
            if cfg!(target_os = "linux") || ui.free_memory_when_hidden {
                if let Ok(url) = window.url() {
                    remember_canonical_url(&app, &url);
                }
                let _ = window.destroy();
            } else {
                let _ = window.hide();
            }
        }
        crate::config::CloseBehavior::MinimizeToTaskbar => {
            if let Some(window) = app.get_webview_window(HUB_LABEL) {
                let _ = window.minimize();
            }
        }
        crate::config::CloseBehavior::Exit => {
            require_editor_closed(&app, "exiting capscr")?;
            exit_app(app);
        }
    }
    Ok(())
}

fn complete_hub_close_with_report(force_exit: bool, app: AppHandle) {
    if let Err(error) = complete_hub_close(force_exit, app.clone()) {
        emit_error(&app, "exit", &error);
    }
}

pub(crate) fn require_editor_closed(app: &AppHandle, action: &str) -> Result<(), String> {
    if let Some(editor) = app.get_webview_window(EDITOR_LABEL) {
        let _ = editor.show();
        let _ = editor.unminimize();
        let _ = editor.set_focus();
        return Err(format!("close the editor before {action}"));
    }
    Ok(())
}

pub fn open_hub_window(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(HUB_LABEL) {
        fit_window_to_work_area(&window, HUB_MIN_WIDTH, HUB_MIN_HEIGHT);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let mut builder = tauri::WebviewWindowBuilder::new(app, HUB_LABEL, url)
        .title("capscr")
        .inner_size(840.0, 560.0)
        .min_inner_size(HUB_MIN_WIDTH, HUB_MIN_HEIGHT)
        .resizable(true)
        .decorations(false)
        .visible(true);

    #[cfg(windows)]
    {
        builder = builder.additional_browser_args(WEBVIEW_BROWSER_ARGS);
    }

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon)?;
    }

    let window = builder.build()?;
    restore_hub_state(&window);
    intercept_hub_close(window.clone());
    heal_stuck_boot(window);
    Ok(())
}

#[cfg(test)]
mod window_size_tests {
    use super::clamp_window_axis;

    #[test]
    fn expands_stale_compact_state() {
        assert_eq!(clamp_window_axis(446, 1920, 720), 720);
    }

    #[test]
    fn keeps_a_larger_saved_size() {
        assert_eq!(clamp_window_axis(1000, 1920, 720), 1000);
    }

    #[test]
    fn fits_displays_smaller_than_the_preferred_floor() {
        assert_eq!(clamp_window_axis(840, 700, 720), 684);
    }
}
