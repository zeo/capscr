//! wayland-capable global mouse-button hotkeys via raw evdev reads.
//!
//! the global-hotkey crate only grabs on x11, so under a wayland session the
//! side buttons (mouse4/mouse5) never reach us — the compositor owns input and
//! won't hand an unfocused app a global grab. read the button edges straight
//! off /dev/input/event* instead: that sits below the display server and works
//! on both session types. keyboard modifier state is tracked off the same
//! streams so "Ctrl+Mouse5"-style bindings still resolve.

use crate::hotkeys::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Manager};

// linux/input-event-codes.h — event types and the codes we react to
const EV_KEY: u16 = 0x01;
const BTN_SIDE: u16 = 0x113; // mouse4 (back)
const BTN_EXTRA: u16 = 0x114; // mouse5 (forward)
const BTN_FORWARD: u16 = 0x115;
const BTN_BACK: u16 = 0x116;

// modifier keycodes, tracked so bindings can carry ctrl/alt/shift/win
const KEY_LEFTCTRL: u16 = 29;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_RIGHTALT: u16 = 100;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;

// one input_event record on 64-bit linux: timeval(16) + type(2) + code(2) + value(4)
const EVENT_SIZE: usize = 24;
const EV_SYN: u16 = 0x00;
const EV_REL: u16 = 0x02;
const EV_MSC: u16 = 0x04;

const UI_DEV_CREATE: usize = 0x5501;
const UI_DEV_DESTROY: usize = 0x5502;
const UI_DEV_SETUP: usize = 0x405c_5503;
const UI_SET_EVBIT: usize = 0x4004_5564;
const UI_SET_KEYBIT: usize = 0x4004_5565;
const UI_SET_RELBIT: usize = 0x4004_5566;
const UI_SET_MSCBIT: usize = 0x4004_5568;
const EVIOCGRAB: usize = 0x4004_4590;

unsafe extern "C" {
    fn ioctl(fd: i32, request: usize, ...) -> i32;
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UInputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

struct MouseMirror {
    output: File,
    input_fd: i32,
}

impl MouseMirror {
    fn create(path: &Path, input_fd: i32) -> std::io::Result<Option<Self>> {
        let event_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let device_dir = PathBuf::from("/sys/class/input")
            .join(event_name)
            .join("device");
        let is_mouse = std::fs::read_dir(&device_dir)?
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("mouse"));
        if !is_mouse {
            return Ok(None);
        }

        let output = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")?;
        let fd = output.as_raw_fd();
        let set = |request, code| unsafe { ioctl(fd, request, code as usize) } == 0;
        if !set(UI_SET_EVBIT, EV_SYN)
            || !set(UI_SET_EVBIT, EV_KEY)
            || !set(UI_SET_EVBIT, EV_REL)
            || !set(UI_SET_EVBIT, EV_MSC)
        {
            return Err(std::io::Error::last_os_error());
        }
        for code in 0x110..=0x117 {
            if !set(UI_SET_KEYBIT, code) {
                return Err(std::io::Error::last_os_error());
            }
        }
        for code in 0..=12 {
            if !set(UI_SET_RELBIT, code) {
                return Err(std::io::Error::last_os_error());
            }
        }
        for code in 0..=7 {
            if !set(UI_SET_MSCBIT, code) {
                return Err(std::io::Error::last_os_error());
            }
        }

        let mut setup = UInputSetup {
            id: InputId {
                bustype: 0x03,
                vendor: 0x1d6b,
                product: 0x0104,
                version: 1,
            },
            name: [0; 80],
            ff_effects_max: 0,
        };
        let name = b"capscr mouse passthrough";
        setup.name[..name.len()].copy_from_slice(name);
        if unsafe { ioctl(fd, UI_DEV_SETUP, &setup) } != 0
            || unsafe { ioctl(fd, UI_DEV_CREATE) } != 0
        {
            return Err(std::io::Error::last_os_error());
        }

        std::thread::sleep(std::time::Duration::from_millis(250));
        if unsafe { ioctl(input_fd, EVIOCGRAB, 1usize) } != 0 {
            unsafe {
                ioctl(fd, UI_DEV_DESTROY);
            }
            return Err(std::io::Error::last_os_error());
        }
        Ok(Some(Self { output, input_fd }))
    }

    fn forward(&mut self, event: &[u8; EVENT_SIZE]) -> std::io::Result<()> {
        self.output.write_all(event)
    }
}

impl Drop for MouseMirror {
    fn drop(&mut self) {
        unsafe {
            ioctl(self.input_fd, EVIOCGRAB, 0usize);
            ioctl(self.output.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

static MODS: AtomicU8 = AtomicU8::new(0);
// (modifier mask, evdev button code) -> capture task id
static BINDINGS: Mutex<Option<HashMap<(u8, u16), String>>> = Mutex::new(None);

fn button_code(name_upper: &str) -> Option<u16> {
    match name_upper {
        "MOUSE4" | "XBUTTON1" => Some(BTN_SIDE),
        "MOUSE5" | "XBUTTON2" => Some(BTN_EXTRA),
        _ => None,
    }
}

fn normalize_button(code: u16) -> Option<u16> {
    match code {
        BTN_SIDE | BTN_BACK => Some(BTN_SIDE),
        BTN_EXTRA | BTN_FORWARD => Some(BTN_EXTRA),
        _ => None,
    }
}

/// parse a hotkey string into (modifier mask, button code) when it targets a
/// mouse side button; None for anything the x11/global-hotkey path should own.
pub fn parse_mouse_binding(s: &str) -> Option<(u8, u16)> {
    let mut mods = 0u8;
    let mut code = None;
    for part in s.split('+').map(str::trim) {
        let up = part.to_ascii_uppercase();
        match up.as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CTRL,
            "ALT" => mods |= MOD_ALT,
            "SHIFT" => mods |= MOD_SHIFT,
            "SUPER" | "WIN" | "META" | "CMD" => mods |= MOD_WIN,
            other => code = button_code(other),
        }
    }
    code.map(|c| (mods, c))
}

pub fn set_bindings(map: HashMap<(u8, u16), String>) {
    *BINDINGS.lock().unwrap() = Some(map);
}

fn mod_bit(code: u16) -> Option<u8> {
    Some(match code {
        KEY_LEFTCTRL | KEY_RIGHTCTRL => MOD_CTRL,
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => MOD_SHIFT,
        KEY_LEFTALT | KEY_RIGHTALT => MOD_ALT,
        KEY_LEFTMETA | KEY_RIGHTMETA => MOD_WIN,
        _ => return None,
    })
}

/// start one reader thread per readable input device. safe to call once at
/// startup; bindings are read live from BINDINGS so a later config reload just
/// updates the map without restarting the threads.
pub fn start(app: AppHandle) {
    let devices = readable_devices();
    if devices.is_empty() {
        tracing::warn!(
            "evdev: no readable input devices — mouse-button hotkeys need read \
             access to /dev/input. add yourself to the 'input' group \
             (sudo usermod -aG input $USER) and re-login."
        );
        return;
    }
    tracing::info!(
        "evdev: watching {} input device(s) for mouse hotkeys",
        devices.len()
    );
    for path in devices {
        let app = app.clone();
        std::thread::Builder::new()
            .name("capscr-evdev".into())
            .spawn(move || read_device(path, app))
            .ok();
    }
}

fn readable_devices() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("event"))
            .unwrap_or(false);
        if is_event && File::open(&path).is_ok() {
            out.push(path);
        }
    }
    out
}

fn read_device(path: PathBuf, app: AppHandle) {
    let Ok(mut file) = File::open(&path) else {
        return;
    };
    let mut mirror = if std::env::var_os("CAPSCR_EXCLUSIVE_MOUSE").is_some() {
        match MouseMirror::create(&path, file.as_raw_fd()) {
            Ok(mirror) => mirror,
            Err(error) => {
                tracing::debug!("evdev: couldn't mirror {}: {error}", path.display());
                None
            }
        }
    } else {
        None
    };
    let mut buf = [0u8; EVENT_SIZE];
    // per-thread dedupe so a fast double-report of one press fires once
    let mut last_fire: HashMap<String, Instant> = HashMap::new();
    let mut consumed_button = None;
    loop {
        // evdev delivers whole 24-byte records; read_exact stays aligned
        if file.read_exact(&mut buf).is_err() {
            return; // device unplugged or vanished
        }
        let etype = u16::from_ne_bytes([buf[16], buf[17]]);
        if etype != EV_KEY {
            if let Some(mirror) = &mut mirror {
                if mirror.forward(&buf).is_err() {
                    return;
                }
            }
            continue;
        }
        let code = u16::from_ne_bytes([buf[18], buf[19]]);
        let value = i32::from_ne_bytes([buf[20], buf[21], buf[22], buf[23]]);

        if let Some(bit) = mod_bit(code) {
            match value {
                1 => {
                    MODS.fetch_or(bit, Ordering::SeqCst);
                }
                0 => {
                    MODS.fetch_and(!bit, Ordering::SeqCst);
                }
                _ => {} // autorepeat: modifier already held
            }
            if let Some(mirror) = &mut mirror {
                if mirror.forward(&buf).is_err() {
                    return;
                }
            }
            continue;
        }

        let normalized = normalize_button(code);
        let consume = if value == 1 {
            normalized
                .map(|button| {
                    let consumed = dispatch(&app, button, &mut last_fire);
                    if consumed {
                        consumed_button = Some(button);
                    }
                    consumed
                })
                .unwrap_or(false)
        } else if value == 0 {
            let consumed = normalized == consumed_button;
            if consumed {
                consumed_button = None;
            }
            consumed
        } else {
            normalized == consumed_button
        };
        if !consume {
            if let Some(mirror) = &mut mirror {
                if mirror.forward(&buf).is_err() {
                    return;
                }
            }
        }
    }
}

fn dispatch(app: &AppHandle, code: u16, last_fire: &mut HashMap<String, Instant>) -> bool {
    if app
        .state::<crate::state::AppState>()
        .hotkeys_disabled
        .load(Ordering::SeqCst)
    {
        return false;
    }
    let mods = MODS.load(Ordering::SeqCst);
    let task_id = {
        let guard = BINDINGS.lock().unwrap();
        guard.as_ref().and_then(|m| m.get(&(mods, code)).cloned())
    };
    let Some(task_id) = task_id else {
        return false;
    };
    let now = Instant::now();
    let recent = last_fire
        .get(&task_id)
        .map(|t| now.duration_since(*t).as_millis() <= 250)
        .unwrap_or(false);
    if recent {
        return true;
    }
    last_fire.insert(task_id.clone(), now);
    crate::commands::trigger_task(app, &task_id);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_mouse_buttons() {
        assert_eq!(parse_mouse_binding("Mouse4"), Some((0, BTN_SIDE)));
        assert_eq!(parse_mouse_binding("Mouse5"), Some((0, BTN_EXTRA)));
    }

    #[test]
    fn parses_modified_mouse_button() {
        assert_eq!(
            parse_mouse_binding("Ctrl+Mouse5"),
            Some((MOD_CTRL, BTN_EXTRA))
        );
    }

    #[test]
    fn normalizes_back_and_forward_button_codes() {
        assert_eq!(normalize_button(BTN_SIDE), Some(BTN_SIDE));
        assert_eq!(normalize_button(BTN_BACK), Some(BTN_SIDE));
        assert_eq!(normalize_button(BTN_EXTRA), Some(BTN_EXTRA));
        assert_eq!(normalize_button(BTN_FORWARD), Some(BTN_EXTRA));
    }

    #[test]
    fn ignores_keyboard_hotkeys() {
        assert_eq!(parse_mouse_binding("Ctrl+Shift+S"), None);
        assert_eq!(parse_mouse_binding("F12"), None);
    }
}
