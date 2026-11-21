use super::*;

pub(crate) const HUB_LABEL: &str = "hub";
const HUB_MIN_SIZE: (u32, u32) = (720, 480);
const WINDOW_MARGIN: u32 = 16;
// what a window shows until its page paints; the boot splash's colour
pub(crate) const WINDOW_BACKGROUND: [u8; 3] = [0x14, 0x14, 0x14];

pub(crate) fn window_is_open(app: &AppHandle, label: &str) -> bool {
    app.windows()
        .is_some_and(|windows| windows.state(label).is_some())
}

pub(crate) fn window_handle(app: &AppHandle, label: &str) -> Option<ceri::NativeHandle> {
    app.windows()?.state(label)?.handle
}

// where a window's position, size and maximized state are kept between launches
pub(crate) fn window_geometry_path(label: &str) -> Option<PathBuf> {
    let dirs = crate::config::project_dirs()?;
    Some(dirs.data_dir().join(format!("window-{label}")))
}

// a native question owned by a window, its two buttons named
pub(crate) fn confirm(
    app: &AppHandle,
    window: &str,
    title: &str,
    text: String,
    (accept, refuse): (&str, &str),
) -> bool {
    let dialog = ceri::MessageDialog {
        title: title.to_string(),
        text,
        level: ceri::MessageLevel::Warning,
        buttons: ceri::MessageButtons::OkCancel,
        owner: window_handle(app, window),
        ok_label: Some(accept.to_string()),
        cancel_label: Some(refuse.to_string()),
    };
    dialog.show().unwrap_or_else(|error| {
        tracing::warn!("the '{title}' dialog failed: {error}");
        false
    })
}

fn hub_config(visible: bool) -> WindowConfig {
    WindowConfig {
        label: HUB_LABEL.to_string(),
        title: "capscr".to_string(),
        width: 840,
        height: 560,
        minimum_width: HUB_MIN_SIZE.0,
        minimum_height: HUB_MIN_SIZE.1,
        geometry_path: window_geometry_path(HUB_LABEL),
        decorations: false,
        visible,
        background: WINDOW_BACKGROUND,
        ..WindowConfig::default()
    }
}

// called at start so the hub is loaded before the user opens the tray and
// the first tray click shows it at once
pub fn prewarm_hub_window(app: &AppHandle) {
    let free_memory = app_state()
        .config
        .lock()
        .unwrap()
        .ui
        .free_memory_when_hidden;
    if free_memory || window_is_open(app, HUB_LABEL) {
        return;
    }
    if let Some(windows) = app.windows() {
        windows.open(hub_config(false));
    }
}

// runs on the loop's thread once a window is open and its size can be read
pub fn window_opened(app: &AppHandle, label: &str) {
    match label {
        HUB_LABEL => grow_window_once(app, label, (0.70, 0.75), (840, 560), HUB_MIN_SIZE),
        EDITOR_LABEL => {
            grow_window_once(app, label, (0.85, 0.85), (960, 640), EDITOR_MIN_SIZE);
            if let Some(windows) = app.windows() {
                windows.act(label, WindowAction::Focus);
            }
        }
        #[cfg(windows)]
        crate::recbar::LABEL => crate::recbar::opened(app),
        _ => {}
    }
}

// the first open of each window (including the first launch after this
// shipped, over an older saved size) takes a share of its monitor's work
// area; from then on its remembered geometry keeps whatever the user picks
fn grow_window_once(
    app: &AppHandle,
    label: &str,
    share: (f64, f64),
    floor: (u32, u32),
    minimum: (u32, u32),
) {
    let state = app_state();
    let grown = state
        .config
        .lock()
        .unwrap()
        .ui
        .grown_windows
        .iter()
        .any(|grown| grown == label);
    if grown {
        place_window(app, label, minimum, None);
        return;
    }
    place_window(app, label, minimum, Some((share, floor)));
    let Ok(_pending_mutation) = PendingFileMutation::begin() else {
        return;
    };
    let mut stored = state.config.lock().unwrap();
    let mut config = stored.clone();
    config.ui.grown_windows.push(label.to_string());
    match config.save_and_migrate_secrets() {
        Ok(()) => *stored = config,
        Err(error) => tracing::warn!("failed to record grown window: {error}"),
    }
}

fn clamp_window_axis(current: u32, work_area: u32, minimum: u32) -> u32 {
    let maximum = work_area.saturating_sub(WINDOW_MARGIN).max(1);
    current.max(minimum.min(maximum)).min(maximum)
}

pub(crate) fn fit_window_to_work_area(app: &AppHandle, label: &str, minimum: (u32, u32)) {
    place_window(app, label, minimum, None);
}

// keeps a window inside the work area of the monitor its centre is on, after
// growing it to a share of that area when asked. the window and its monitor
// are read in physical pixels and the window is moved in logical ones
fn place_window(
    app: &AppHandle,
    label: &str,
    minimum: (u32, u32),
    growth: Option<((f64, f64), (u32, u32))>,
) {
    let Some(windows) = app.windows() else {
        return;
    };
    let Some(window) = windows.state(label) else {
        return;
    };
    if window.maximized || window.minimized || window.fullscreen {
        return;
    }
    let centre = (
        window.position.0.saturating_add_unsigned(window.size.0 / 2),
        window.position.1.saturating_add_unsigned(window.size.1 / 2),
    );
    let monitors = windows.monitors();
    let Some(monitor) = monitors
        .iter()
        .find(|monitor| {
            let ((left, top), (width, height)) = (monitor.position, monitor.size);
            (left..left.saturating_add_unsigned(width)).contains(&centre.0)
                && (top..top.saturating_add_unsigned(height)).contains(&centre.1)
        })
        .or_else(|| monitors.iter().find(|monitor| monitor.primary))
    else {
        return;
    };
    let (origin, work_area) = monitor.work_area;
    let physical = |logical: u32| (f64::from(logical) * window.scale).round() as u32;
    let logical = |physical: f64| (physical / window.scale).round();

    let grown = growth
        .map(|(share, floor)| {
            (
                ((f64::from(work_area.0) * share.0) as u32).max(physical(floor.0)),
                ((f64::from(work_area.1) * share.1) as u32).max(physical(floor.1)),
            )
        })
        .filter(|(width, height)| window.size.0 < *width || window.size.1 < *height);
    let wanted = grown.map_or(window.size, |(width, height)| {
        (window.size.0.max(width), window.size.1.max(height))
    });
    let width = clamp_window_axis(wanted.0, work_area.0, physical(minimum.0));
    let height = clamp_window_axis(wanted.1, work_area.1, physical(minimum.1));
    if (width, height) != window.size {
        windows.act(
            label,
            WindowAction::Size(
                logical(f64::from(width)) as u32,
                logical(f64::from(height)) as u32,
            ),
        );
    }
    if grown.is_some() {
        windows.act(label, WindowAction::Center);
        return;
    }
    let max_x = origin
        .0
        .saturating_add_unsigned(work_area.0.saturating_sub(width));
    let max_y = origin
        .1
        .saturating_add_unsigned(work_area.1.saturating_sub(height));
    let x = window.position.0.clamp(origin.0, max_x);
    let y = window.position.1.clamp(origin.1, max_y);
    if (x, y) != window.position {
        windows.act(
            label,
            WindowAction::Position(logical(f64::from(x)) as i32, logical(f64::from(y)) as i32),
        );
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HubCloseRequest {
    request_id: String,
    force_exit: bool,
}

// asks the hub page to finish the close; it completes without the page when
// the page does not acknowledge in time
fn emit_hub_close_request(app: &AppHandle, force_exit: bool) {
    let request_id = uuid::Uuid::new_v4().as_simple().to_string();
    app_state()
        .pending_hub_close_requests
        .lock()
        .unwrap()
        .insert(request_id.clone());
    let request = HubCloseRequest {
        request_id: request_id.clone(),
        force_exit,
    };
    app.emit_to(HUB_LABEL, "capscr://close-requested", request);

    let fallback_app = app.clone();
    crate::runtime().spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let pending = app_state()
            .pending_hub_close_requests
            .lock()
            .unwrap()
            .remove(&request_id);
        if pending {
            complete_hub_close_with_report(force_exit, fallback_app);
        }
    });
}

// whether a window its user or the app asked to close may close; runs on the
// loop's thread
pub fn close_requested(app: &AppHandle, label: &str) -> bool {
    let state = app_state();
    match label {
        // the close button keeps the hub loaded for the next tray click, so
        // only the close complete_hub_close asked for goes through
        HUB_LABEL => {
            if state
                .hub_close_approved
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return true;
            }
            emit_hub_close_request(app, false);
            false
        }
        EDITOR_LABEL
            if state
                .editor_close_guarded
                .load(std::sync::atomic::Ordering::SeqCst) =>
        {
            app.emit_to(EDITOR_LABEL, "capscr://editor-close-requested", ());
            false
        }
        _ => true,
    }
}

pub fn window_closed(label: &str) {
    if label == EDITOR_LABEL {
        app_state()
            .editor_close_guarded
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
    #[cfg(target_os = "linux")]
    forget_pin_position(label);
}

// record what the OS actually dropped so upload_file can trust the path the
// page later hands it (drag-drop is the only legitimate caller with an
// arbitrary path)
pub fn files_dropped(label: &str, paths: &[PathBuf]) {
    if label == HUB_LABEL {
        app_state().remember_dropped_paths(paths.iter().cloned());
    }
}

pub fn request_hub_close(app: &AppHandle, force_exit: bool) {
    if window_is_open(app, HUB_LABEL) {
        emit_hub_close_request(app, force_exit);
        return;
    }
    complete_hub_close_with_report(force_exit, app.clone());
}

pub fn acknowledge_hub_close(request_id: String, state: &AppState) -> bool {
    state
        .pending_hub_close_requests
        .lock()
        .unwrap()
        .remove(&request_id)
}

pub fn complete_hub_close(force_exit: bool, app: AppHandle) -> Result<(), String> {
    if force_exit {
        require_editor_closed(&app, "exiting capscr")?;
        exit_app(app);
        return Ok(());
    }
    let state = app_state();
    let ui = state.config.lock().unwrap().ui.clone();
    match ui.close_behavior {
        crate::config::CloseBehavior::MinimizeToTray => {
            let Some(windows) = app.windows().filter(|_| window_is_open(&app, HUB_LABEL)) else {
                return Ok(());
            };
            // on linux the hidden hub is always closed and reopened on demand
            if cfg!(target_os = "linux") || ui.free_memory_when_hidden {
                state
                    .hub_close_approved
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                windows.close(HUB_LABEL);
            } else {
                windows.act(HUB_LABEL, WindowAction::Hide);
            }
        }
        crate::config::CloseBehavior::MinimizeToTaskbar => {
            if let Some(windows) = app.windows() {
                windows.act(HUB_LABEL, WindowAction::Minimize);
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
    if let Some(windows) = app.windows().filter(|_| window_is_open(app, EDITOR_LABEL)) {
        windows.act(EDITOR_LABEL, WindowAction::Focus);
        return Err(format!("close the editor before {action}"));
    }
    Ok(())
}

pub fn open_hub_window(app: &AppHandle) {
    let Some(windows) = app.windows() else {
        return;
    };
    if window_is_open(app, HUB_LABEL) {
        fit_window_to_work_area(app, HUB_LABEL, HUB_MIN_SIZE);
        windows.act(HUB_LABEL, WindowAction::Focus);
        return;
    }
    windows.open(hub_config(true));
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
