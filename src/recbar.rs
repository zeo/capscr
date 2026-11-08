// one recording session: the armed frame (windows), the running clock with
// its pauses, and the "recbar" control window that mirrors both

use crate::capture::Rectangle;
use crate::config::CaptureTask;
use crate::recording::PauseClock;
use crate::state::AppState;
use serde::Serialize;
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

#[cfg(windows)]
use crate::overlay::recording::{windows_impl as frame, FrameEvent, FrameMode};

pub const LABEL: &str = "recbar";

struct Session {
    task: CaptureTask,
    region: Rectangle,
    // none while the frame is armed and nothing records yet
    clock: Option<(Instant, PauseClock)>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

#[derive(Clone, Serialize)]
pub struct BarState {
    mode: &'static str,
    active_ms: u64,
    width: u32,
    height: u32,
}

fn snapshot() -> BarState {
    let session = SESSION.lock().unwrap();
    let Some(s) = session.as_ref() else {
        return BarState {
            mode: "idle",
            active_ms: 0,
            width: 0,
            height: 0,
        };
    };
    let (mode, active_ms) = match &s.clock {
        None => ("armed", 0),
        Some((start, clock)) => (
            if clock.is_paused() {
                "paused"
            } else {
                "recording"
            },
            clock.active(*start, Instant::now()).as_millis() as u64,
        ),
    };
    BarState {
        mode,
        active_ms,
        width: s.region.width,
        height: s.region.height,
    }
}

fn publish(app: &AppHandle) {
    let _ = app.emit_to(LABEL, "recbar://state", snapshot());
}

pub fn armed_task_id() -> Option<String> {
    SESSION
        .lock()
        .unwrap()
        .as_ref()
        .filter(|s| s.clock.is_none())
        .map(|s| s.task.id.clone())
}

#[cfg(windows)]
fn moved(app: &AppHandle, region: Rectangle) {
    if let Some(s) = SESSION.lock().unwrap().as_mut() {
        s.region = region;
    }
    if let Some(recorder) = app
        .state::<AppState>()
        .gif_recorder
        .lock()
        .unwrap()
        .as_ref()
    {
        recorder.set_region(region);
    }
    place_bar(region);
    publish(app);
}

// shows the selected region as an adjustable frame; REC, Enter or the task
// hotkey starts the recording, Esc or the bar's close button drops it
#[cfg(windows)]
pub fn arm(app: &AppHandle, task: &CaptureTask, region: Rectangle) {
    *SESSION.lock().unwrap() = Some(Session {
        task: task.clone(),
        region,
        clock: None,
    });
    let handle = app.clone();
    frame::arm(
        region,
        Box::new(move |event| match event {
            FrameEvent::Moved(region) => moved(&handle, region),
            FrameEvent::Record => {
                let handle = handle.clone();
                std::thread::spawn(move || record(&handle));
            }
            FrameEvent::Cancel => cancel(&handle),
        }),
    );
    place_bar(region);
    publish(app);
}

pub fn record(app: &AppHandle) {
    let Some(session) = SESSION.lock().unwrap().take_if(|s| s.clock.is_none()) else {
        return;
    };
    #[cfg(windows)]
    let region = frame::region();
    #[cfg(not(windows))]
    let region = session.region;
    if let Err(e) = crate::commands::start_gif_recording(&session.task, app, region) {
        tracing::warn!("recording '{}' failed to start: {e:#}", session.task.id);
        crate::commands::emit_error(app, "recording", &format!("could not start recording: {e}"));
        finish(app);
    }
}

// called once the recorder runs, in place of the old overlay start
pub fn begin(app: &AppHandle, task: &CaptureTask, region: Rectangle) {
    *SESSION.lock().unwrap() = Some(Session {
        task: task.clone(),
        region,
        clock: Some((Instant::now(), PauseClock::default())),
    });
    #[cfg(windows)]
    {
        frame::set_mode(FrameMode::Recording);
        // the frame may have moved between REC and the recorder starting
        moved(app, frame::region());
    }
    #[cfg(not(windows))]
    {
        crate::overlay::RecordingOverlay::start(region);
        publish(app);
    }
}

pub fn set_paused(app: &AppHandle, paused: bool) {
    {
        let mut session = SESSION.lock().unwrap();
        let Some((_, clock)) = session.as_mut().and_then(|s| s.clock.as_mut()) else {
            return;
        };
        clock.update(paused, Instant::now());
    }
    if let Some(recorder) = app
        .state::<AppState>()
        .gif_recorder
        .lock()
        .unwrap()
        .as_ref()
    {
        recorder.set_paused(paused);
    }
    #[cfg(windows)]
    frame::set_mode(if paused {
        FrameMode::Paused
    } else {
        FrameMode::Recording
    });
    publish(app);
}

pub fn toggle_pause(app: &AppHandle) {
    let paused = SESSION
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|s| s.clock.as_ref())
        .map(|(_, clock)| clock.is_paused());
    if let Some(paused) = paused {
        set_paused(app, !paused);
    }
}

pub fn cancel(app: &AppHandle) {
    if armed_task_id().is_some() {
        finish(app);
    }
}

pub fn finish(app: &AppHandle) {
    *SESSION.lock().unwrap() = None;
    crate::overlay::RecordingOverlay::stop();
    #[cfg(windows)]
    hide_bar();
    publish(app);
}

#[tauri::command]
pub fn recbar_status() -> BarState {
    snapshot()
}

#[tauri::command]
pub fn recbar_record(app: AppHandle) {
    // starting the recorder blocks; commands without async run on the main thread
    std::thread::spawn(move || record(&app));
}

#[tauri::command]
pub fn recbar_pause(app: AppHandle, paused: bool) {
    set_paused(&app, paused);
}

#[tauri::command]
pub fn recbar_cancel(app: AppHandle) {
    cancel(&app);
}

// resizes the frame around its centre; 0x0 fills the monitor it sits on
#[tauri::command]
pub fn recbar_preset(width: u32, height: u32) {
    #[cfg(windows)]
    {
        let current = frame::region();
        let (cx, cy) = (
            current.x + current.width as i32 / 2,
            current.y + current.height as i32 / 2,
        );
        let monitors = crate::capture::list_monitors().unwrap_or_default();
        let Some(m) = monitors
            .iter()
            .find(|m| {
                cx >= m.x && cy >= m.y && cx < m.x + m.width as i32 && cy < m.y + m.height as i32
            })
            .or(monitors.first())
        else {
            return;
        };
        let bounds = Rectangle::new(m.x, m.y, m.width, m.height);
        let next = if width == 0 || height == 0 {
            bounds
        } else {
            let (w, h) = (width.min(m.width), height.min(m.height));
            crate::overlay::recording::clamp_into(
                Rectangle::new(cx - w as i32 / 2, cy - h as i32 / 2, w, h),
                bounds,
                0,
            )
        };
        frame::set_region(next);
    }
    #[cfg(not(windows))]
    let _ = (width, height);
}

#[cfg(windows)]
mod bar {
    use super::*;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SetWindowDisplayAffinity, SetWindowPos, ShowWindowAsync, HWND_TOPMOST,
        SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE,
        WDA_EXCLUDEFROMCAPTURE,
    };

    // logical size; keep in sync with .recbar-wide in styles.css
    const BAR_W: f64 = 300.0;
    const BAR_H: f64 = 36.0;

    // read once on the main thread; tauri's hwnd() getter blocks on it
    static BAR_HWND: AtomicIsize = AtomicIsize::new(0);

    // built hidden at startup so the bar is up the moment a frame arms
    pub fn prewarm(app: &tauri::App) -> tauri::Result<()> {
        let window = tauri::WebviewWindowBuilder::new(
            app,
            LABEL,
            tauri::WebviewUrl::App("recbar.html".into()),
        )
        .title("capscr recording")
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .shadow(false)
        .visible(false)
        .inner_size(BAR_W, BAR_H)
        .build()?;
        let hwnd = HWND(window.hwnd()?.0 as *mut _);
        unsafe {
            if let Err(error) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
                tracing::warn!("recbar capture exclusion unavailable: {error}");
            }
        }
        BAR_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        Ok(())
    }

    fn hwnd() -> Option<HWND> {
        let raw = BAR_HWND.load(Ordering::SeqCst);
        (raw != 0).then_some(HWND(raw as *mut _))
    }

    // async so a drag on the frame thread never waits on the webview's thread
    pub fn place_bar(region: Rectangle) {
        let Some(hwnd) = hwnd() else { return };
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(hwnd, &mut rect);
        }
        let b = frame::band();
        let outer = Rectangle::new(
            region.x - b,
            region.y - b,
            region.width + b as u32 * 2,
            region.height + b as u32 * 2,
        );
        let monitors = crate::capture::list_monitors().unwrap_or_default();
        let size = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y) = crate::overlay::recording::bar_position(outer, size, &monitors, true);
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    pub fn hide_bar() {
        if let Some(hwnd) = hwnd() {
            unsafe {
                let _ = ShowWindowAsync(hwnd, SW_HIDE);
            }
        }
    }
}

#[cfg(windows)]
pub use bar::prewarm;
#[cfg(windows)]
use bar::{hide_bar, place_bar};
