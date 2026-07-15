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

use crate::capture::Rectangle;

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
    native_backdrop: Option<super::wayland_backdrop::NativeBackdrop>,
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
            let window_right = window.x.saturating_add_unsigned(window.width);
            let window_bottom = window.y.saturating_add_unsigned(window.height);
            let covers_output = window.x <= surface_rect.0
                && window.y <= surface_rect.1
                && window_right >= right
                && window_bottom >= bottom;
            (!covers_output
                && window.x < right
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
    let ActiveSelection {
        surfaces,
        tx,
        native_backdrop,
        ..
    } = active;
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
    drop(native_backdrop);
    let _ = tx.send(result);
}

// a wayland session also exposes DISPLAY via xwayland, so DISPLAY being set does
// not mean x11. the gtk/webview toolkit runs as a wayland client whenever the
// session is wayland and the backend isn't pinned to x11 — match that, otherwise
// the overlay takes the x11 absolute-positioning path and lands misplaced,
// showing both monitors crammed into one window
fn is_wayland_session() -> bool {
    if std::env::var("GDK_BACKEND")
        .map(|b| b.eq_ignore_ascii_case("x11"))
        .unwrap_or(false)
    {
        return false;
    }
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|t| t.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

pub fn select(frozen_frame: Option<Arc<RgbaImage>>) -> SelectionResult {
    let Some(app) = APP.get() else {
        tracing::warn!("selector invoked before app handle registration");
        return SelectionResult::Cancelled;
    };
    if active_selector_active() {
        return SelectionResult::Cancelled;
    }

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
                    scope.spawn(|| crate::capture::capture_wayland_screen(&monitor.name))
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
            surfaces.push(SelectorSurface {
                label: format!("{SELECTOR_LABEL_PREFIX}{index}"),
                output_name: Some(monitor.name.clone()),
                frame,
                origin: (monitor.x, monitor.y),
                windows: normalize_wayland_windows(all_windows.clone(), rect, &monitor.name),
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

    let focus_label = active_output
        .and_then(|name| {
            monitors
                .iter()
                .position(|monitor| monitor.name == name)
                .map(|index| format!("{SELECTOR_LABEL_PREFIX}{index}"))
        })
        .unwrap_or_else(|| surfaces[0].label.clone());

    let native_backdrop = pure_wayland
        .then(|| {
            let frames = surfaces
                .iter()
                .filter_map(|surface| {
                    Some(super::wayland_backdrop::BackdropFrame {
                        output_name: surface.output_name.clone()?,
                        image: surface.frame.clone(),
                    })
                })
                .collect();
            super::wayland_backdrop::NativeBackdrop::show(frames)
        })
        .transpose()
        .unwrap_or_else(|error| {
            tracing::debug!("using webview selector backdrop: {error:#}");
            None
        });
    let native_backdrop_active = native_backdrop.is_some();

    let (tx, rx): (Sender<SelectionResult>, Receiver<SelectionResult>) = channel();
    *ACTIVE.lock().unwrap() = Some(ActiveSelection {
        surfaces,
        ready: HashSet::new(),
        focus_label,
        tx,
        native_backdrop,
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
            if let Err(error) =
                build_selector_window(&app_for_build, &label, rect, native_backdrop_active)
            {
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
    transparent: bool,
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
        .transparent(transparent)
        .visible(false)
        .position(x, y)
        .inner_size(w, h)
        .build()?;
    use gtk::prelude::{Cast, GtkWindowExt, WidgetExt};
    if let Ok(gtk_window) = window.gtk_window() {
        gtk_window.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
        if is_wayland_session() {
            if let Some(screen) = WidgetExt::screen(&gtk_window) {
                let display = gtk_window.display();
                let center_x = (x + w / 2.0).round() as i32;
                let center_y = (y + h / 2.0).round() as i32;
                let target = display.monitor_at_point(center_x, center_y);
                let monitor_index = (0..display.n_monitors())
                    .find(|index| display.monitor(*index) == target)
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
                tracing::info!("selector {label} using layer shell: {layered}");
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
            let on_blank = url.as_ref().map(|u| u.scheme() == "about").unwrap_or(false);
            tracing::info!("selector webview url after {wait_ms}ms: {url:?}");
            if !on_blank {
                return;
            }
            tracing::warn!("selector webview stuck on about:blank; navigating explicitly");
            let target = app.get_webview_window("hub").and_then(|hub| hub.url().ok());
            if let Some(url) = target {
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
    pub native_backdrop: bool,
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
        native_backdrop: active.native_backdrop.is_some(),
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
                SelectionResult::Region(Rectangle::new(x, y, width, height))
            }
        }
        SelectorOutcome::Window { id, handle, x, y } => match handle {
            Some(handle) => SelectionResult::WaylandWindow { handle, x, y },
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
