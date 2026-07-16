// linux selector overlay: one undecorated always-on-top webview per output,
// showing frozen frames with the same interaction
// model as the win32 GDI selector (drag region, click window, alt+click color
// pick, shift aspect-snap, ctrl fine-tune, esc/right-click cancel). the UI
// lives in frontend/src/views/Selector.tsx and talks back over the commands
// at the bottom of this file.
//
// select() blocks the calling capture thread on a channel until the UI
// commits a result; window creation happens on the main thread via
// run_on_main_thread. on wayland each webview is fullscreened on its output.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use image::RgbaImage;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::capture::{gui_is_wayland as is_wayland_session, Rectangle};

use super::SelectionResult;

const SELECTOR_LABEL_PREFIX: &str = "selector-";
// generous upper bound so a wedged webview can't hold the capture gate
// (capture_in_progress) forever
const SELECT_TIMEOUT: Duration = Duration::from_secs(600);

static APP: OnceLock<AppHandle> = OnceLock::new();

struct ActiveSelection {
    surfaces: Vec<SelectorSurface>,
    ready: HashSet<String>,
    focus_label: String,
    tx: Sender<SelectionResult>,
}

struct SelectorSurface {
    label: String,
    output_name: Option<String>,
    frame: Arc<RgbaImage>,
    origin: (i32, i32),
    windows: Vec<WindowRect>,
    rect: (i32, i32, u32, u32),
}

static ACTIVE: Mutex<Option<ActiveSelection>> = Mutex::new(None);
// prewarmed window list, filled on a background thread ahead of select()
static PREWARMED: Mutex<Option<Receiver<Vec<WindowRect>>>> = Mutex::new(None);

struct LayerShellApi {
    _library: usize,
    is_supported: unsafe extern "C" fn() -> c_int,
    init_for_window: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow),
    set_layer: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int),
    set_anchor: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int, c_int),
    set_monitor: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *mut gtk::gdk::ffi::GdkMonitor),
    set_keyboard_mode: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int),
    set_namespace: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *const c_char),
}

unsafe impl Send for LayerShellApi {}
unsafe impl Sync for LayerShellApi {}

static LAYER_SHELL: OnceLock<Option<LayerShellApi>> = OnceLock::new();

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

fn layer_shell_api() -> Option<&'static LayerShellApi> {
    LAYER_SHELL
        .get_or_init(|| unsafe {
            let library_name = CString::new("libgtk-layer-shell.so.0").unwrap();
            let library = dlopen(library_name.as_ptr(), 2);
            if library.is_null() {
                return None;
            }
            macro_rules! symbol {
                ($name:literal, $kind:ty) => {{
                    let name = CString::new($name).unwrap();
                    let symbol = dlsym(library, name.as_ptr());
                    if symbol.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $kind>(symbol)
                }};
            }
            Some(LayerShellApi {
                _library: library as usize,
                is_supported: symbol!("gtk_layer_is_supported", unsafe extern "C" fn() -> c_int),
                init_for_window: symbol!(
                    "gtk_layer_init_for_window",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow)
                ),
                set_layer: symbol!(
                    "gtk_layer_set_layer",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int)
                ),
                set_anchor: symbol!(
                    "gtk_layer_set_anchor",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int, c_int)
                ),
                set_monitor: symbol!(
                    "gtk_layer_set_monitor",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *mut gtk::gdk::ffi::GdkMonitor)
                ),
                set_keyboard_mode: symbol!(
                    "gtk_layer_set_keyboard_mode",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, c_int)
                ),
                set_namespace: symbol!(
                    "gtk_layer_set_namespace",
                    unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *const c_char)
                ),
            })
        })
        .as_ref()
}

fn configure_layer_shell(window: &gtk::Window, monitor: &gtk::gdk::Monitor) -> bool {
    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::WidgetExt;

    let Some(api) = layer_shell_api() else {
        return false;
    };
    if unsafe { (api.is_supported)() } == 0 {
        return false;
    }
    if window.is_realized() {
        window.unrealize();
    }
    let namespace = CString::new("capscr-selector").unwrap();
    unsafe {
        let window_ptr = window.to_glib_none().0;
        (api.init_for_window)(window_ptr);
        (api.set_namespace)(window_ptr, namespace.as_ptr());
        (api.set_layer)(window_ptr, 3);
        for edge in 0..4 {
            (api.set_anchor)(window_ptr, edge, 1);
        }
        (api.set_monitor)(window_ptr, monitor.to_glib_none().0);
        (api.set_keyboard_mode)(window_ptr, 2);
    }
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct WindowRect {
    pub id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

pub(crate) fn app_handle() -> Option<&'static AppHandle> {
    APP.get()
}

fn enumerate_windows() -> Vec<WindowRect> {
    if is_wayland_session() {
        match crate::capture::list_wayland_windows() {
            Ok(windows) => {
                return windows
                    .into_iter()
                    .enumerate()
                    .map(|(index, window)| WindowRect {
                        id: index as u32,
                        handle: Some(window.handle),
                        x: window.x,
                        y: window.y,
                        width: window.width,
                        height: window.height,
                    })
                    .collect();
            }
            Err(error) => tracing::debug!("KWin window list unavailable: {error:#}"),
        }
    }
    let own_pid = std::process::id();
    let Ok(windows) = xcap::Window::all() else {
        return Vec::new();
    };
    let mut rects: Vec<(i32, WindowRect)> = windows
        .iter()
        .filter_map(|w| {
            if w.pid().ok()? == own_pid || w.is_minimized().ok()? {
                return None;
            }
            let rect = WindowRect {
                id: w.id().ok()?,
                handle: None,
                x: w.x().ok()?,
                y: w.y().ok()?,
                width: w.width().ok()?,
                height: w.height().ok()?,
            };
            if rect.width <= 5 || rect.height <= 5 {
                return None;
            }
            Some((w.z().unwrap_or(0), rect))
        })
        .collect();
    // top-most first, so the UI can hit-test by taking the first match
    rects.sort_by_key(|r| std::cmp::Reverse(r.0));
    rects.into_iter().map(|(_, r)| r).collect()
}

fn normalize_wayland_windows(
    windows: Vec<WindowRect>,
    surface_rect: (i32, i32, u32, u32),
    output_name: &str,
) -> Vec<WindowRect> {
    let monitor_origin = xcap::Monitor::all().ok().and_then(|monitors| {
        monitors
            .into_iter()
            .find(|monitor| monitor.name().ok().as_deref() == Some(output_name))
            .and_then(|monitor| Some((monitor.x().ok()?, monitor.y().ok()?)))
    });

    let right = surface_rect.0.saturating_add_unsigned(surface_rect.2);
    let bottom = surface_rect.1.saturating_add_unsigned(surface_rect.3);
    windows
        .into_iter()
        .filter_map(|mut window| {
            if window.handle.is_none() {
                let (monitor_x, monitor_y) = monitor_origin?;
                window.x = surface_rect.0 + window.x - monitor_x;
                window.y = surface_rect.1 + window.y - monitor_y;
            }
            // fullscreen windows cover the whole output and still count: the
            // list is ranked by real stacking order, so they only win the
            // hover hit-test when they are actually on top (desktop and
            // panel surfaces never reach this list — kwin marks them
            // skip-taskbar and list_windows drops those)
            let window_right = window.x.saturating_add_unsigned(window.width);
            let window_bottom = window.y.saturating_add_unsigned(window.height);
            (window.x < right
                && window_right > surface_rect.0
                && window.y < bottom
                && window_bottom > surface_rect.1)
                .then_some(window)
        })
        .collect()
}

pub fn prewarm_window_list() {
    let (tx, rx) = channel();
    *PREWARMED.lock().unwrap() = Some(rx);
    std::thread::spawn(move || {
        let _ = tx.send(enumerate_windows());
    });
}

pub fn active_selector_active() -> bool {
    ACTIVE.lock().unwrap().is_some()
}

pub fn cancel_active_selection() {
    finish(SelectionResult::Cancelled);
}

// resolve the pending selection, notify the blocked capture thread, and tear
// down the overlay window. safe to call from any thread and idempotent — the
// second caller finds ACTIVE already empty.
fn finish(result: SelectionResult) {
    let active = ACTIVE.lock().unwrap().take();
    let Some(active) = active else { return };
    let ActiveSelection { surfaces, tx, .. } = active;
    let labels: Vec<_> = surfaces.into_iter().map(|surface| surface.label).collect();
    if let Some(app) = APP.get() {
        for label in &labels {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.hide();
            }
        }
        let app = app.clone();
        let labels = labels.clone();
        let _ = app.clone().run_on_main_thread(move || {
            for label in labels {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.destroy();
                }
            }
        });
    }
    let _ = tx.send(result);
}

fn frozen_region(rect: Rectangle) -> Option<Arc<RgbaImage>> {
    let active = ACTIVE.lock().unwrap();
    let active = active.as_ref()?;
    compose_frozen_region(rect, &active.surfaces).map(Arc::new)
}

fn compose_frozen_region(rect: Rectangle, surfaces: &[SelectorSurface]) -> Option<RgbaImage> {
    let right = rect.x.saturating_add_unsigned(rect.width);
    let bottom = rect.y.saturating_add_unsigned(rect.height);
    let mut pieces = Vec::new();

    for surface in surfaces {
        let surface_right = surface.rect.0.saturating_add_unsigned(surface.rect.2);
        let surface_bottom = surface.rect.1.saturating_add_unsigned(surface.rect.3);
        let x0 = rect.x.max(surface.rect.0);
        let y0 = rect.y.max(surface.rect.1);
        let x1 = right.min(surface_right);
        let y1 = bottom.min(surface_bottom);
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let scale_x = surface.frame.width() as f64 / surface.rect.2 as f64;
        let scale_y = surface.frame.height() as f64 / surface.rect.3 as f64;
        pieces.push((surface, (x0, y0, x1, y1), scale_x, scale_y));
    }
    if pieces.is_empty() {
        return None;
    }

    let target_scale_x = pieces.iter().map(|piece| piece.2).fold(1.0f64, f64::max);
    let target_scale_y = pieces.iter().map(|piece| piece.3).fold(1.0f64, f64::max);
    let mut captured = RgbaImage::new(
        (rect.width as f64 * target_scale_x).round().max(1.0) as u32,
        (rect.height as f64 * target_scale_y).round().max(1.0) as u32,
    );

    for (surface, (x0, y0, x1, y1), scale_x, scale_y) in pieces {
        let source_x = ((x0 - surface.rect.0) as f64 * scale_x).round() as u32;
        let source_y = ((y0 - surface.rect.1) as f64 * scale_y).round() as u32;
        let source_right = ((x1 - surface.rect.0) as f64 * scale_x).round() as u32;
        let source_bottom = ((y1 - surface.rect.1) as f64 * scale_y).round() as u32;
        let piece = image::imageops::crop_imm(
            &*surface.frame,
            source_x,
            source_y,
            source_right.saturating_sub(source_x),
            source_bottom.saturating_sub(source_y),
        )
        .to_image();
        let destination_width = ((x1 - x0) as f64 * target_scale_x).round().max(1.0) as u32;
        let destination_height = ((y1 - y0) as f64 * target_scale_y).round().max(1.0) as u32;
        let piece = if piece.width() == destination_width && piece.height() == destination_height {
            piece
        } else {
            image::imageops::resize(
                &piece,
                destination_width,
                destination_height,
                image::imageops::FilterType::CatmullRom,
            )
        };
        image::imageops::overlay(
            &mut captured,
            &piece,
            ((x0 - rect.x) as f64 * target_scale_x).round() as i64,
            ((y0 - rect.y) as f64 * target_scale_y).round() as i64,
        );
    }
    Some(captured)
}

pub fn select(frozen_frame: Option<Arc<RgbaImage>>) -> SelectionResult {
    let Some(app) = APP.get() else {
        tracing::warn!("selector invoked before app handle registration");
        return SelectionResult::Cancelled;
    };
    if active_selector_active() {
        return SelectionResult::Cancelled;
    }

    let selector_started = std::time::Instant::now();
    let monitors = crate::capture::list_monitors().unwrap_or_default();
    if monitors.is_empty() {
        return SelectionResult::Cancelled;
    }
    let pure_wayland = is_wayland_session();
    let active_output = pure_wayland
        .then(crate::capture::active_wayland_monitor)
        .and_then(Result::ok)
        .map(|monitor| monitor.name);
    let all_windows = PREWARMED
        .lock()
        .unwrap()
        .take()
        .and_then(|receiver| receiver.recv().ok())
        .unwrap_or_else(enumerate_windows);
    let surfaces = if pure_wayland {
        let captures = std::thread::scope(|scope| {
            monitors
                .iter()
                .map(|monitor| {
                    scope.spawn(|| crate::capture::wayland_freeze_output(&monitor.name))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|job| job.join())
                .collect::<Vec<_>>()
        });
        let mut surfaces = Vec::with_capacity(monitors.len());
        for (index, (monitor, capture)) in monitors.iter().zip(captures).enumerate() {
            let rect = (monitor.x, monitor.y, monitor.width, monitor.height);
            let frame = match capture {
                Ok(Ok(frame)) => Arc::new(frame),
                Ok(Err(error)) => {
                    tracing::error!("failed to freeze output {}: {error:#}", monitor.name);
                    return SelectionResult::Cancelled;
                }
                Err(_) => {
                    tracing::error!("output capture worker panicked for {}", monitor.name);
                    return SelectionResult::Cancelled;
                }
            };
            tracing::info!(
                "froze output {} at {}x{}",
                monitor.name,
                frame.width(),
                frame.height()
            );
            let windows = normalize_wayland_windows(all_windows.clone(), rect, &monitor.name);
            tracing::debug!(
                "selector {}: {} of {} windows intersect",
                monitor.name,
                windows.len(),
                all_windows.len(),
            );
            surfaces.push(SelectorSurface {
                label: format!("{SELECTOR_LABEL_PREFIX}{index}"),
                output_name: Some(monitor.name.clone()),
                frame,
                origin: (monitor.x, monitor.y),
                windows,
                rect,
            });
        }
        surfaces
    } else {
        let desktop_origin = (
            monitors.iter().map(|monitor| monitor.x).min().unwrap_or(0),
            monitors.iter().map(|monitor| monitor.y).min().unwrap_or(0),
        );
        let rect = (
            desktop_origin.0,
            desktop_origin.1,
            monitors
                .iter()
                .map(|monitor| monitor.x + monitor.width as i32)
                .max()
                .unwrap_or(0)
                .saturating_sub(desktop_origin.0) as u32,
            monitors
                .iter()
                .map(|monitor| monitor.y + monitor.height as i32)
                .max()
                .unwrap_or(0)
                .saturating_sub(desktop_origin.1) as u32,
        );
        let frame = frozen_frame.or_else(|| {
            crate::capture::ScreenCapture::all_monitors()
                .ok()
                .map(Arc::new)
        });
        let Some(frame) = frame else {
            return SelectionResult::Cancelled;
        };
        vec![SelectorSurface {
            label: format!("{SELECTOR_LABEL_PREFIX}0"),
            output_name: None,
            frame,
            origin: desktop_origin,
            windows: all_windows,
            rect,
        }]
    };

    // a freeze source that ignored the output transform would compose rotated
    // pixels; route those to the webview selector instead of showing garbage
    let frames_oriented = surfaces.iter().all(|surface| {
        let (frame_w, frame_h) = (surface.frame.width(), surface.frame.height());
        frame_w == frame_h
            || surface.rect.2 == surface.rect.3
            || (frame_w > frame_h) == (surface.rect.2 > surface.rect.3)
    });
    if pure_wayland && !frames_oriented {
        tracing::error!("frozen frame orientation mismatches its output; using webview selector");
    }
    let force_webview = std::env::var_os("CAPSCR_FORCE_WEBVIEW_SELECTOR").is_some();
    if pure_wayland && frames_oriented && !force_webview {
        // debugging aid: map only the named output's selector surface
        let only_output = std::env::var("CAPSCR_SELECTOR_OUTPUT").ok();
        let outputs = surfaces
            .iter()
            .filter(|surface| {
                only_output
                    .as_deref()
                    .is_none_or(|name| surface.output_name.as_deref() == Some(name))
            })
            .filter_map(|surface| {
                Some(super::wayland_native_selector::NativeOutput {
                    output_name: surface.output_name.clone()?,
                    image: surface.frame.clone(),
                    rect: Rectangle::new(
                        surface.rect.0,
                        surface.rect.1,
                        surface.rect.2,
                        surface.rect.3,
                    ),
                    windows: surface
                        .windows
                        .iter()
                        .map(|window| super::wayland_native_selector::NativeWindow {
                            id: window.id,
                            handle: window.handle.clone(),
                            rect: Rectangle::new(window.x, window.y, window.width, window.height),
                        })
                        .collect(),
                })
            })
            .collect();
        match super::wayland_native_selector::NativeSelector::show(outputs) {
            Ok(selector) => {
                tracing::info!(
                    "native selector interactive in {}ms",
                    selector_started.elapsed().as_millis()
                );
                let outcome = selector.recv_timeout(SELECT_TIMEOUT);
                return match outcome {
                    Ok(super::wayland_native_selector::NativeOutcome::Region(rect))
                    | Ok(super::wayland_native_selector::NativeOutcome::Monitor(rect, _)) => {
                        compose_frozen_region(rect, &surfaces).map_or(
                            SelectionResult::Region(rect),
                            |image| SelectionResult::FrozenRegion {
                                rect,
                                image: Arc::new(image),
                            },
                        )
                    }
                    Ok(super::wayland_native_selector::NativeOutcome::Window(window)) => {
                        compose_frozen_region(window.rect, &surfaces).map_or_else(
                            || match window.handle {
                                Some(handle) => SelectionResult::WaylandWindow {
                                    handle,
                                    x: window.rect.x,
                                    y: window.rect.y,
                                },
                                None => SelectionResult::Window(window.id),
                            },
                            |image| SelectionResult::FrozenRegion {
                                rect: window.rect,
                                image: Arc::new(image),
                            },
                        )
                    }
                    Ok(super::wayland_native_selector::NativeOutcome::FullScreen) => {
                        SelectionResult::FullScreen
                    }
                    Ok(super::wayland_native_selector::NativeOutcome::Cancelled) => {
                        SelectionResult::Cancelled
                    }
                    Ok(super::wayland_native_selector::NativeOutcome::Color(r, g, b)) => {
                        SelectionResult::PickedColor(r, g, b)
                    }
                    Err(error) => {
                        tracing::warn!("native selector timed out: {error}");
                        SelectionResult::Cancelled
                    }
                };
            }
            Err(error) => tracing::warn!("using webview selector: {error:#}"),
        }
    }

    let focus_label = active_output
        .and_then(|name| {
            monitors
                .iter()
                .position(|monitor| monitor.name == name)
                .map(|index| format!("{SELECTOR_LABEL_PREFIX}{index}"))
        })
        .unwrap_or_else(|| surfaces[0].label.clone());

    let (tx, rx): (Sender<SelectionResult>, Receiver<SelectionResult>) = channel();
    *ACTIVE.lock().unwrap() = Some(ActiveSelection {
        surfaces,
        ready: HashSet::new(),
        focus_label,
        tx,
    });

    let app_for_build = app.clone();
    let windows: Vec<_> = ACTIVE
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .surfaces
        .iter()
        .map(|surface| (surface.label.clone(), surface.rect))
        .collect();
    let built = app.run_on_main_thread(move || {
        for (label, rect) in windows {
            let rect = (rect.0 as f64, rect.1 as f64, rect.2 as f64, rect.3 as f64);
            if let Err(error) = build_selector_window(&app_for_build, &label, rect) {
                tracing::error!("selector window build failed: {error}");
                finish(SelectionResult::Cancelled);
                break;
            }
        }
    });
    if built.is_err() {
        *ACTIVE.lock().unwrap() = None;
        return SelectionResult::Cancelled;
    }

    match rx.recv_timeout(SELECT_TIMEOUT) {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!("selector timed out — treating as cancelled");
            finish(SelectionResult::Cancelled);
            SelectionResult::Cancelled
        }
    }
}

fn build_selector_window(
    app: &AppHandle,
    label: &str,
    (x, y, w, h): (f64, f64, f64, f64),
) -> tauri::Result<()> {
    if let Some(stale) = app.get_webview_window(label) {
        let _ = stale.destroy();
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let window = tauri::WebviewWindowBuilder::new(app, label, url)
        .title("capscr selector")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .position(x, y)
        .inner_size(w, h)
        .build()?;
    use gtk::gdk::prelude::MonitorExt;
    use gtk::prelude::{Cast, GtkWindowExt, WidgetExt};
    if let Ok(gtk_window) = window.gtk_window() {
        gtk_window.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
        if is_wayland_session() {
            if let Some(screen) = WidgetExt::screen(&gtk_window) {
                let display = gtk_window.display();
                // gdk's invented wayland layout can disagree with the
                // compositor's logical coordinates on mixed-dpi setups, so
                // trust exact geometry first and points only as a fallback
                let expected = (
                    x.round() as i32,
                    y.round() as i32,
                    w.round() as i32,
                    h.round() as i32,
                );
                let by_geometry = (0..display.n_monitors()).find(|index| {
                    display.monitor(*index).is_some_and(|monitor| {
                        let geometry = monitor.geometry();
                        (
                            geometry.x(),
                            geometry.y(),
                            geometry.width(),
                            geometry.height(),
                        ) == expected
                    })
                });
                let center_x = (x + w / 2.0).round() as i32;
                let center_y = (y + h / 2.0).round() as i32;
                let monitor_index = by_geometry
                    .or_else(|| {
                        let target = display.monitor_at_point(center_x, center_y);
                        (0..display.n_monitors()).find(|index| display.monitor(*index) == target)
                    })
                    .or_else(|| {
                        let primary = display.primary_monitor();
                        (0..display.n_monitors()).find(|index| display.monitor(*index) == primary)
                    })
                    .unwrap_or(0);
                let layered = display.monitor(monitor_index).is_some_and(|monitor| {
                    configure_layer_shell(gtk_window.upcast_ref(), &monitor)
                });
                if !layered {
                    gtk_window.fullscreen_on_monitor(&screen, monitor_index);
                }
                tracing::info!(
                    "selector {label} on gdk monitor {monitor_index} (geometry match: {}), layer shell: {layered}",
                    by_geometry.is_some(),
                );
            }
        }
    }
    watch_selector_navigation(app, window);
    Ok(())
}

fn set_selector_cursor(window: &tauri::WebviewWindow) {
    use gtk::prelude::{Cast, ContainerExt, GtkWindowExt, WidgetExt};

    fn set_on_widget(widget: &gtk::Widget, cursor: &gtk::gdk::Cursor) {
        if let Some(surface) = widget.window() {
            surface.set_cursor(Some(cursor));
        }
        if let Some(container) = widget.dynamic_cast_ref::<gtk::Container>() {
            for child in container.children() {
                set_on_widget(&child, cursor);
            }
        }
    }

    let Ok(gtk_window) = window.gtk_window() else {
        return;
    };
    let display = gtk_window.display();
    if let Some(cursor) = gtk::gdk::Cursor::from_name(&display, "crosshair") {
        set_on_widget(gtk_window.upcast_ref(), &cursor);
    }
}

// dynamically created webviews sometimes come up on about:blank instead of
// the app url (tauri#13967) — same watchdog the editor window uses, copying
// the canonical url from the always-alive hub webview
fn watch_selector_navigation(app: &AppHandle, window: tauri::WebviewWindow) {
    let app = app.clone();
    std::thread::spawn(move || {
        for wait_ms in [500u64, 1500, 3000] {
            std::thread::sleep(Duration::from_millis(wait_ms));
            let url = window.url();
            tracing::info!("selector webview url after {wait_ms}ms: {url:?}");
            match url {
                Ok(url) if url.scheme() != "about" => {
                    crate::commands::remember_canonical_url(&app, &url);
                    return;
                }
                Err(_) => return,
                _ => {}
            }
            tracing::warn!("selector webview stuck on about:blank; navigating explicitly");
            if let Some(url) = crate::commands::canonical_app_url(&app) {
                if let Err(e) = window.navigate(url) {
                    tracing::warn!("selector explicit navigation failed: {e}");
                }
            }
        }
    });
}

// ---- commands used by frontend/src/views/Selector.tsx ----

#[derive(Serialize)]
pub struct SelectorContext {
    // virtual-screen origin of the frame (selection results are reported in
    // virtual-screen coordinates, so the UI adds this to canvas coordinates)
    pub origin_x: i32,
    pub origin_y: i32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub windows: Vec<WindowRect>,
}

#[tauri::command]
pub fn selector_context(window: tauri::WebviewWindow) -> Result<SelectorContext, String> {
    let guard = ACTIVE.lock().unwrap();
    let active = guard.as_ref().ok_or("no active selection")?;
    let surface = active
        .surfaces
        .iter()
        .find(|surface| surface.label == window.label())
        .ok_or("selector surface is unavailable")?;
    Ok(SelectorContext {
        origin_x: surface.origin.0,
        origin_y: surface.origin.1,
        frame_width: surface.frame.width(),
        frame_height: surface.frame.height(),
        windows: surface.windows.clone(),
    })
}

// raw RGBA bytes of the frozen frame; the UI paints them straight into a
// canvas ImageData without an encode/decode round-trip
#[tauri::command]
pub fn selector_frame(window: tauri::WebviewWindow) -> Result<tauri::ipc::Response, String> {
    let guard = ACTIVE.lock().unwrap();
    let active = guard.as_ref().ok_or("no active selection")?;
    let surface = active
        .surfaces
        .iter()
        .find(|surface| surface.label == window.label())
        .ok_or("selector surface is unavailable")?;
    Ok(tauri::ipc::Response::new(surface.frame.as_raw().clone()))
}

#[tauri::command]
pub fn selector_ready(window: tauri::WebviewWindow) -> Result<(), String> {
    let app = APP.get().ok_or("selector app handle is unavailable")?;
    let (labels, focus_label) = {
        let mut guard = ACTIVE.lock().unwrap();
        let active = guard.as_mut().ok_or("no active selection")?;
        if !active
            .surfaces
            .iter()
            .any(|surface| surface.label == window.label())
        {
            return Err("selector surface is unavailable".into());
        }
        active.ready.insert(window.label().to_string());
        if active.ready.len() != active.surfaces.len() {
            return Ok(());
        }
        (
            active
                .surfaces
                .iter()
                .map(|surface| surface.label.clone())
                .collect::<Vec<_>>(),
            active.focus_label.clone(),
        )
    };
    for label in labels {
        let surface = app
            .get_webview_window(&label)
            .ok_or("selector window is unavailable")?;
        surface.show().map_err(|error| error.to_string())?;
        set_selector_cursor(&surface);
    }
    if let Some(focus) = app.get_webview_window(&focus_label) {
        focus.set_focus().map_err(|error| error.to_string())?;
    }
    tracing::info!("selector surfaces mapped");
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectorOutcome {
    Region {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    Window {
        id: u32,
        handle: Option<String>,
        x: i32,
        y: i32,
    },
    FullScreen,
    Color {
        r: u8,
        g: u8,
        b: u8,
    },
    Cancelled,
}

#[tauri::command]
pub fn selector_finish(window: tauri::WebviewWindow, outcome: SelectorOutcome) {
    let result = match outcome {
        SelectorOutcome::Region {
            x,
            y,
            width,
            height,
        } => {
            if width == 0 || height == 0 {
                SelectionResult::Cancelled
            } else {
                let rect = Rectangle::new(x, y, width, height);
                match frozen_region(rect) {
                    Some(image) => SelectionResult::FrozenRegion { rect, image },
                    None => SelectionResult::Region(rect),
                }
            }
        }
        SelectorOutcome::Window { id, handle, x, y } => match handle {
            Some(handle) => {
                let rect = {
                    let active = ACTIVE.lock().unwrap();
                    active.as_ref().and_then(|active| {
                        active
                            .surfaces
                            .iter()
                            .flat_map(|surface| surface.windows.iter())
                            .find(|window| window.handle.as_deref() == Some(handle.as_str()))
                            .map(|window| {
                                Rectangle::new(window.x, window.y, window.width, window.height)
                            })
                    })
                };
                rect.and_then(|rect| frozen_region(rect).map(|image| (rect, image)))
                    .map_or(
                        SelectionResult::WaylandWindow { handle, x, y },
                        |(rect, image)| SelectionResult::FrozenRegion { rect, image },
                    )
            }
            None => SelectionResult::Window(id),
        },
        SelectorOutcome::FullScreen => {
            let monitor = ACTIVE.lock().unwrap().as_ref().and_then(|active| {
                active
                    .surfaces
                    .iter()
                    .find(|surface| surface.label == window.label())
                    .map(|surface| (surface.rect, surface.output_name.clone()))
            });
            match monitor {
                Some(((x, y, width, height), output_name)) => SelectionResult::Monitor {
                    rect: Rectangle::new(x, y, width, height),
                    output_name,
                },
                None => SelectionResult::Cancelled,
            }
        }
        SelectorOutcome::Color { r, g, b } => SelectionResult::PickedColor(r, g, b),
        SelectorOutcome::Cancelled => SelectionResult::Cancelled,
    };
    finish(result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn frozen_region_uses_native_pixels_across_differently_scaled_outputs() {
        let surfaces = [
            SelectorSurface {
                label: "left".into(),
                output_name: Some("left".into()),
                frame: Arc::new(RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]))),
                origin: (0, 0),
                windows: Vec::new(),
                rect: (0, 0, 2, 2),
            },
            SelectorSurface {
                label: "right".into(),
                output_name: Some("right".into()),
                frame: Arc::new(RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]))),
                origin: (2, 0),
                windows: Vec::new(),
                rect: (2, 0, 2, 2),
            },
        ];

        let image = compose_frozen_region(Rectangle::new(1, 0, 2, 2), &surfaces).unwrap();

        assert_eq!(image.dimensions(), (4, 4));
        assert_eq!(image.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(1, 3), &Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(2, 0), &Rgba([0, 0, 255, 255]));
        assert_eq!(image.get_pixel(3, 3), &Rgba([0, 0, 255, 255]));
    }

    fn patterned(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x + y * 16) as u8, (x * 3) as u8, (y * 7) as u8, 255])
        })
    }

    // the pixel-exactness contract: a region inside one output must come out
    // byte-identical to the plain crop of that output's native frame
    #[test]
    fn frozen_region_inside_a_fractional_scale_output_is_an_exact_crop() {
        let surfaces = [SelectorSurface {
            label: "hdmi".into(),
            output_name: Some("hdmi".into()),
            frame: Arc::new(patterned(12, 6)),
            origin: (10, 20),
            windows: Vec::new(),
            rect: (10, 20, 8, 4),
        }];

        let image = compose_frozen_region(Rectangle::new(12, 21, 4, 2), &surfaces).unwrap();

        let expected =
            image::imageops::crop_imm(&*surfaces[0].frame, 3, 2, image.width(), image.height())
                .to_image();
        assert_eq!(image.as_raw(), expected.as_raw());
    }

    // this machine's topology in miniature: a portrait scale-1 output beside
    // a fractional 1.5x output, with a region spanning both
    #[test]
    fn frozen_region_spanning_portrait_and_scaled_outputs_lands_each_piece() {
        let surfaces = [
            SelectorSurface {
                label: "portrait".into(),
                output_name: Some("portrait".into()),
                frame: Arc::new(RgbaImage::from_pixel(4, 8, Rgba([255, 0, 0, 255]))),
                origin: (0, 0),
                windows: Vec::new(),
                rect: (0, 0, 4, 8),
            },
            SelectorSurface {
                label: "hdmi".into(),
                output_name: Some("hdmi".into()),
                frame: Arc::new(RgbaImage::from_pixel(12, 6, Rgba([0, 0, 255, 255]))),
                origin: (4, 2),
                windows: Vec::new(),
                rect: (4, 2, 8, 4),
            },
        ];

        let image = compose_frozen_region(Rectangle::new(2, 2, 6, 4), &surfaces).unwrap();

        // densest output wins: 6x4 logical at 1.5 = 9x6 pixels
        assert_eq!(image.dimensions(), (9, 6));
        assert_eq!(image.get_pixel(0, 0), &Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(2, 5), &Rgba([255, 0, 0, 255]));
        assert_eq!(image.get_pixel(3, 0), &Rgba([0, 0, 255, 255]));
        assert_eq!(image.get_pixel(8, 5), &Rgba([0, 0, 255, 255]));
    }
}
