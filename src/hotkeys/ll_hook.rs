// low-level keyboard hook (WH_KEYBOARD_LL) — capscr's authoritative hotkey
// dispatch path on Windows. unlike RegisterHotKey, an LL hook fires before
// WM_HOTKEY routing and before app-level handlers in any other process, so
// capscr wins races against other tools that may have registered the same
// chord. the callback consumes matched key presses (returns 1) so downstream
// handlers don't double-fire.
//
// CRITICAL: the hook callback runs on a kernel-managed thread and is subject
// to LowLevelHooksTimeout (300ms by default on Win10/11). it MUST return
// fast — no logging, no allocation in the hot path, no blocking. all work
// happens on the dispatcher thread that receives HookEvents over a channel.

use crossbeam_channel::Sender;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

pub const MOD_CTRL: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_SHIFT: u8 = 4;
pub const MOD_WIN: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HookBinding {
    pub vk: u32,
    pub mods: u8,
}

#[derive(Clone, Debug)]
pub enum HookEvent {
    Fire { task_id: String },
}

struct HookRegistry {
    bindings: HashMap<HookBinding, String>,
    enabled: bool,
    tx: Option<Sender<HookEvent>>,
}

static REGISTRY: OnceLock<Mutex<HookRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<HookRegistry> {
    REGISTRY.get_or_init(|| {
        Mutex::new(HookRegistry {
            bindings: HashMap::new(),
            enabled: true,
            tx: None,
        })
    })
}

pub fn init(tx: Sender<HookEvent>) {
    registry().lock().unwrap().tx = Some(tx);
}

pub fn set_bindings(bindings: HashMap<HookBinding, String>) {
    registry().lock().unwrap().bindings = bindings;
}

pub fn set_enabled(enabled: bool) {
    registry().lock().unwrap().enabled = enabled;
}

pub fn current_enabled() -> bool {
    registry().lock().unwrap().enabled
}

pub fn registered_bindings() -> Vec<(HookBinding, String)> {
    registry()
        .lock()
        .unwrap()
        .bindings
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect()
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let msg = wparam.0 as u32;
    if msg != WM_KEYDOWN && msg != WM_SYSKEYDOWN {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    // SAFETY: lparam points at a KBDLLHOOKSTRUCT for the duration of the
    // callback. Microsoft documents this contract; we only read.
    let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let vk = kb.vkCode;

    if is_modifier_vk(vk) {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    let mods = current_modifier_mask();
    let binding = HookBinding { vk, mods };

    // tight critical section: pull task_id and channel handle out under lock,
    // do the send afterwards. avoids holding the mutex while crossing thread
    // boundaries inside the LL timeout window.
    let (task_id, tx) = {
        let reg = match registry().lock() {
            Ok(g) => g,
            Err(_) => return unsafe { CallNextHookEx(None, code, wparam, lparam) },
        };
        if !reg.enabled {
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }
        (reg.bindings.get(&binding).cloned(), reg.tx.clone())
    };

    if let (Some(task_id), Some(tx)) = (task_id, tx) {
        let _ = tx.try_send(HookEvent::Fire { task_id });
        return LRESULT(1);
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn is_modifier_vk(vk: u32) -> bool {
    matches!(
        vk,
        0x10  // VK_SHIFT
        | 0x11 // VK_CONTROL
        | 0x12 // VK_MENU (Alt)
        | 0x14 // VK_CAPITAL (CapsLock)
        | 0x5B // VK_LWIN
        | 0x5C // VK_RWIN
        | 0xA0..=0xA5 // left/right variants of shift/ctrl/alt
    )
}

fn current_modifier_mask() -> u8 {
    let mut mods = 0u8;
    unsafe {
        if (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 {
            mods |= MOD_CTRL;
        }
        if (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0 {
            mods |= MOD_ALT;
        }
        if (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 {
            mods |= MOD_SHIFT;
        }
        let lwin = (GetKeyState(VK_LWIN.0 as i32) as u16 & 0x8000) != 0;
        let rwin = (GetKeyState(VK_RWIN.0 as i32) as u16 & 0x8000) != 0;
        if lwin || rwin {
            mods |= MOD_WIN;
        }
    }
    mods
}

pub fn spawn_hook_thread() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("capscr-llkeyboard".into())
        .spawn(|| unsafe {
            let hook: HHOOK = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}");
                    return;
                }
            };

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = UnhookWindowsHookEx(hook);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_vk_recognized() {
        assert!(is_modifier_vk(0x10));
        assert!(is_modifier_vk(0xA2));
        assert!(!is_modifier_vk(0x41)); // A
        assert!(!is_modifier_vk(0x2C)); // PrintScreen
    }

    #[test]
    fn bindings_set_and_read() {
        let mut map = HashMap::new();
        map.insert(
            HookBinding { vk: 0x2C, mods: 0 },
            "task-printscreen".to_string(),
        );
        set_bindings(map);
        let bindings = registered_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].0.vk, 0x2C);
        assert_eq!(bindings[0].1, "task-printscreen");
    }

    #[test]
    fn enabled_toggle_roundtrips() {
        set_enabled(false);
        assert!(!current_enabled());
        set_enabled(true);
        assert!(current_enabled());
    }
}
