// linux selector overlay: a single undecorated always-on-top webview spanning
// the virtual desktop, showing the frozen frame with the same interaction
// model as the win32 GDI selector (drag region, click window, alt+click color
// pick, shift aspect-snap, ctrl fine-tune, esc/right-click cancel). the UI
// lives in frontend/src/views/Selector.tsx and talks back over the commands
// at the bottom of this file.
//
// select() blocks the calling capture thread on a channel until the UI
// commits a result; window creation happens on the main thread via
// run_on_main_thread. on wayland global window positioning isn't available,
// so the overlay is fullscreened on the current monitor instead of spanning.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use image::RgbaImage;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::capture::Rectangle;

use super::SelectionResult;

const SELECTOR_LABEL: &str = "selector";
// generous upper bound so a wedged webview can't hold the capture gate
// (capture_in_progress) forever
const SELECT_TIMEOUT: Duration = Duration::from_secs(600);

static APP: OnceLock<AppHandle> = OnceLock::new();

struct ActiveSelection {
    frame: Option<Arc<RgbaImage>>,
    // virtual-screen origin of the frame's top-left pixel
    origin: (i32, i32),
    windows: Vec<WindowRect>,
    tx: Sender<SelectionResult>,
}

static ACTIVE: Mutex<Option<ActiveSelection>> = Mutex::new(None);
// prewarmed window list, filled on a background thread ahead of select()
static PREWARMED: Mutex<Option<Vec<WindowRect>>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
pub struct WindowRect {
    pub id: u32,
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
    let Some(monitor) = xcap::Monitor::all().ok().and_then(|monitors| {
        monitors
            .into_iter()
            .find(|monitor| monitor.name().ok().as_deref() == Some(output_name))
    }) else {
        return windows;
    };
    let Ok(monitor_x) = monitor.x() else {
        return windows;
    };
    let Ok(monitor_y) = monitor.y() else {
        return windows;
    };

    let right = surface_rect.0.saturating_add_unsigned(surface_rect.2);
    let bottom = surface_rect.1.saturating_add_unsigned(surface_rect.3);
    windows
        .into_iter()
        .filter_map(|mut window| {
            window.x = surface_rect.0 + window.x - monitor_x;
            window.y = surface_rect.1 + window.y - monitor_y;
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
    std::thread::spawn(|| {
        let list = enumerate_windows();
        *PREWARMED.lock().unwrap() = Some(list);
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
    let _ = active.tx.send(result);
    if let Some(app) = APP.get() {
        let app = app.clone();
        let _ = app.clone().run_on_main_thread(move || {
            if let Some(w) = app.get_webview_window(SELECTOR_LABEL) {
                let _ = w.destroy();
            }
        });
    }
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
    let desktop_origin = (
        monitors.iter().map(|m| m.x).min().unwrap_or(0),
        monitors.iter().map(|m| m.y).min().unwrap_or(0),
    );
    let pure_wayland = is_wayland_session();
    let active_wayland_monitor = if pure_wayland {
        crate::capture::active_wayland_monitor().ok().or_else(|| {
            monitors
                .iter()
                .find(|monitor| monitor.is_primary)
                .or_else(|| monitors.first())
                .cloned()
        })
    } else {
        None
    };
    let surface_rect = if let Some(monitor) = &active_wayland_monitor {
        (monitor.x, monitor.y, monitor.width, monitor.height)
    } else {
        (
            desktop_origin.0,
            desktop_origin.1,
            monitors
                .iter()
                .map(|m| m.x + m.width as i32)
                .max()
                .unwrap_or(0)
                .saturating_sub(desktop_origin.0) as u32,
            monitors
                .iter()
                .map(|m| m.y + m.height as i32)
                .max()
                .unwrap_or(0)
                .saturating_sub(desktop_origin.1) as u32,
        )
    };

    let mut windows = PREWARMED
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(enumerate_windows);
    if pure_wayland {
        let output_name = active_wayland_monitor
            .as_ref()
            .map(|monitor| monitor.name.as_str())
            .unwrap_or_default();
        windows = normalize_wayland_windows(windows, surface_rect, output_name);
    }

    // recording region-picks call select(None). the win32 selector shows the
    // live desktop through a semi-transparent layered window there; X11 can't
    // rely on a compositor for that, so grab our own freeze-frame backdrop
    let frame = frozen_frame.or_else(|| {
        if pure_wayland {
            crate::capture::capture_wayland_area(
                surface_rect.0,
                surface_rect.1,
                surface_rect.2,
                surface_rect.3,
            )
            .ok()
            .map(Arc::new)
        } else {
            crate::capture::ScreenCapture::all_monitors()
                .ok()
                .map(Arc::new)
        }
    });

    let (tx, rx): (Sender<SelectionResult>, Receiver<SelectionResult>) = channel();
    *ACTIVE.lock().unwrap() = Some(ActiveSelection {
        frame,
        origin: (surface_rect.0, surface_rect.1),
        windows,
        tx,
    });

    let app_for_build = app.clone();
    let virt = (
        surface_rect.0 as f64,
        surface_rect.1 as f64,
        surface_rect.2 as f64,
        surface_rect.3 as f64,
    );
    let built = app.run_on_main_thread(move || {
        if let Err(e) = build_selector_window(&app_for_build, virt) {
            tracing::error!("selector window build failed: {e}");
            finish(SelectionResult::Cancelled);
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

fn build_selector_window(app: &AppHandle, (x, y, w, h): (f64, f64, f64, f64)) -> tauri::Result<()> {
    if let Some(stale) = app.get_webview_window(SELECTOR_LABEL) {
        let _ = stale.destroy();
    }
    let url = tauri::WebviewUrl::App("index.html".into());
    let window = tauri::WebviewWindowBuilder::new(app, SELECTOR_LABEL, url)
        .title("capscr selector")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .position(x, y)
        .inner_size(w, h)
        .build()?;
    use gtk::prelude::{GtkWindowExt, WidgetExt};
    if let Ok(gtk_window) = window.gtk_window() {
        gtk_window.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
        if is_wayland_session() {
            if let Some(screen) = WidgetExt::screen(&gtk_window) {
                let display = gtk_window.display();
                let primary = display.primary_monitor();
                let monitor_index = (0..display.n_monitors())
                    .find(|index| display.monitor(*index) == primary)
                    .unwrap_or(0);
                gtk_window.fullscreen_on_monitor(&screen, monitor_index);
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
}

#[tauri::command]
pub fn selector_context() -> Result<SelectorContext, String> {
    let guard = ACTIVE.lock().unwrap();
    let active = guard.as_ref().ok_or("no active selection")?;
    let (fw, fh) = active
        .frame
        .as_ref()
        .map(|f| (f.width(), f.height()))
        .unwrap_or((0, 0));
    Ok(SelectorContext {
        origin_x: active.origin.0,
        origin_y: active.origin.1,
        frame_width: fw,
        frame_height: fh,
        windows: active.windows.clone(),
    })
}

// raw RGBA bytes of the frozen frame; the UI paints them straight into a
// canvas ImageData without an encode/decode round-trip
#[tauri::command]
pub fn selector_frame() -> Result<tauri::ipc::Response, String> {
    let guard = ACTIVE.lock().unwrap();
    let active = guard.as_ref().ok_or("no active selection")?;
    let frame = active.frame.as_ref().ok_or("no frozen frame")?;
    Ok(tauri::ipc::Response::new(frame.as_raw().clone()))
}

#[tauri::command]
pub fn selector_ready() -> Result<(), String> {
    let app = APP.get().ok_or("selector app handle is unavailable")?;
    let window = app
        .get_webview_window(SELECTOR_LABEL)
        .ok_or("selector window is unavailable")?;
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;
    set_selector_cursor(&window);
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
pub fn selector_finish(outcome: SelectorOutcome) {
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
        SelectorOutcome::Window { id } => SelectionResult::Window(id),
        SelectorOutcome::FullScreen => SelectionResult::FullScreen,
        SelectorOutcome::Color { r, g, b } => SelectionResult::PickedColor(r, g, b),
        SelectorOutcome::Cancelled => SelectionResult::Cancelled,
    };
    finish(result);
}
