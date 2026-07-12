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
    let min_x = monitors.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = monitors
        .iter()
        .map(|m| m.x + m.width as i32)
        .max()
        .unwrap_or(0);
    let max_y = monitors
        .iter()
        .map(|m| m.y + m.height as i32)
        .max()
        .unwrap_or(0);

    let windows = PREWARMED
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(enumerate_windows);

    // recording region-picks call select(None). the win32 selector shows the
    // live desktop through a semi-transparent layered window there; X11 can't
    // rely on a compositor for that, so grab our own freeze-frame backdrop
    let frame = frozen_frame.or_else(|| {
        crate::capture::ScreenCapture::all_monitors()
            .ok()
            .map(Arc::new)
    });

    let (tx, rx): (Sender<SelectionResult>, Receiver<SelectionResult>) = channel();
    *ACTIVE.lock().unwrap() = Some(ActiveSelection {
        frame,
        origin: (min_x, min_y),
        windows,
        tx,
    });

    let app_for_build = app.clone();
    let virt = (
        min_x as f64,
        min_y as f64,
        (max_x - min_x) as f64,
        (max_y - min_y) as f64,
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

fn build_selector_window(
    app: &AppHandle,
    (x, y, w, h): (f64, f64, f64, f64),
) -> tauri::Result<()> {
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
    // wayland can't place windows at absolute coordinates; fall back to
    // fullscreen-on-current-monitor so the overlay is still usable there
    if std::env::var("WAYLAND_DISPLAY").is_ok() && std::env::var("DISPLAY").is_err() {
        let _ = window.set_fullscreen(true);
    }
    let _ = window.show();
    let _ = window.set_focus();
    watch_selector_navigation(app, window);
    Ok(())
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
            let target = app
                .get_webview_window("hub")
                .and_then(|hub| hub.url().ok());
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

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectorOutcome {
    Region { x: i32, y: i32, width: u32, height: u32 },
    Window { id: u32 },
    FullScreen,
    Color { r: u8, g: u8, b: u8 },
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

