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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
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
    Captured { vk: u32, mods: u8 },
}

// when set, the next non-modifier keydown is captured (vk + current mod
// mask) and sent on the channel as HookEvent::Captured. the press is
// swallowed (LRESULT(1)) so the captured key doesn't also fire any other
// handler. used by the hub's HotkeyInput to record the exact vk Windows
// delivers — eliminates browser e.code ↔ VK_* drift across keyboard
// layouts, NumLock state, and FN-combined laptop keys.
pub static CAPTURE_REQUEST: AtomicBool = AtomicBool::new(false);

pub fn begin_capture() {
    CAPTURE_REQUEST.store(true, Ordering::SeqCst);
}

pub fn cancel_capture() {
    CAPTURE_REQUEST.store(false, Ordering::SeqCst);
}

pub fn is_capturing() -> bool {
    CAPTURE_REQUEST.load(Ordering::SeqCst)
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

// telemetry — every counter incremented from the hook callback or from the
// public API. read by hotkey_diagnostics so the user can see exactly where
// the chain is breaking (hook installed? callback firing? key matched?
// dispatcher running?). all atomic so the hook never blocks.
pub static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
pub static HOOK_CALLS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static HOOK_KEYDOWN_CALLS: AtomicU64 = AtomicU64::new(0);
pub static HOOK_MATCHED_CALLS: AtomicU64 = AtomicU64::new(0);
pub static HOOK_DISPATCH_SENT: AtomicU64 = AtomicU64::new(0);
pub static HOOK_DISPATCH_DROPPED: AtomicU64 = AtomicU64::new(0);
pub static HOOK_LAST_VK: AtomicU32 = AtomicU32::new(0);
pub static HOOK_LAST_MODS: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Debug)]
pub struct HookTelemetry {
    pub installed: bool,
    pub enabled: bool,
    pub registered_count: usize,
    pub registered: Vec<(HookBinding, String)>,
    pub calls_total: u64,
    pub keydown_calls: u64,
    pub matched_calls: u64,
    pub dispatch_sent: u64,
    pub dispatch_dropped: u64,
    pub last_vk: u32,
    pub last_mods: u8,
}

pub fn snapshot_telemetry() -> HookTelemetry {
    let reg = registry().lock().unwrap();
    HookTelemetry {
        installed: HOOK_INSTALLED.load(Ordering::SeqCst),
        enabled: reg.enabled,
        registered_count: reg.bindings.len(),
        registered: reg.bindings.iter().map(|(k, v)| (*k, v.clone())).collect(),
        calls_total: HOOK_CALLS_TOTAL.load(Ordering::SeqCst),
        keydown_calls: HOOK_KEYDOWN_CALLS.load(Ordering::SeqCst),
        matched_calls: HOOK_MATCHED_CALLS.load(Ordering::SeqCst),
        dispatch_sent: HOOK_DISPATCH_SENT.load(Ordering::SeqCst),
        dispatch_dropped: HOOK_DISPATCH_DROPPED.load(Ordering::SeqCst),
        last_vk: HOOK_LAST_VK.load(Ordering::SeqCst),
        last_mods: HOOK_LAST_MODS.load(Ordering::SeqCst),
    }
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
    HOOK_CALLS_TOTAL.fetch_add(1, Ordering::SeqCst);
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let msg = wparam.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    if !is_down && !is_up {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    // SAFETY: lparam points at a KBDLLHOOKSTRUCT for the duration of the
    // callback. Microsoft documents this contract; we only read.
    let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let raw_vk = kb.vkCode;
    // LLKHF_EXTENDED bit 0 of flags — set for the "real" cursor / nav cluster
    // (arrow keys, Home/End/PageUp/PageDown/Insert/Delete above the numpad)
    // and clear for the equivalent vks produced by the numpad keys when
    // NumLock is OFF. we use this to normalize numpad bindings: a user who
    // bound "Numpad5" expects it to fire regardless of NumLock state.
    let is_extended = (kb.flags.0 & 0x01) != 0;
    let vk = normalize_numpad_vk(raw_vk, is_extended);

    // modifier tracking has to live inside the hook itself: GetKeyState
    // reflects already-processed input messages, and GetAsyncKeyState is
    // explicitly documented as unreliable for the key being delivered.
    // see LowLevelKeyboardProc remarks on learn.microsoft.com.
    if let Some(bit) = modifier_bit(vk) {
        let cur = MODIFIER_STATE.load(Ordering::SeqCst);
        let next = if is_down { cur | bit } else { cur & !bit };
        MODIFIER_STATE.store(next, Ordering::SeqCst);
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    if !is_down {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }

    HOOK_KEYDOWN_CALLS.fetch_add(1, Ordering::SeqCst);
    let mods = MODIFIER_STATE.load(Ordering::SeqCst);
    HOOK_LAST_VK.store(vk, Ordering::SeqCst);
    HOOK_LAST_MODS.store(mods, Ordering::SeqCst);

    // capture mode: swallow the press, ship (vk, mods) to the dispatcher,
    // and clear the flag so the *next* press goes back to normal matching.
    if CAPTURE_REQUEST.swap(false, Ordering::SeqCst) {
        let tx_opt = {
            let reg = match registry().lock() {
                Ok(g) => g,
                Err(_) => return LRESULT(1),
            };
            reg.tx.clone()
        };
        if let Some(tx) = tx_opt {
            let _ = tx.try_send(HookEvent::Captured { vk, mods });
        }
        return LRESULT(1);
    }

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
        HOOK_MATCHED_CALLS.fetch_add(1, Ordering::SeqCst);
        match tx.try_send(HookEvent::Fire { task_id }) {
            Ok(()) => HOOK_DISPATCH_SENT.fetch_add(1, Ordering::SeqCst),
            Err(_) => HOOK_DISPATCH_DROPPED.fetch_add(1, Ordering::SeqCst),
        };
        return LRESULT(1);
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    HOOK_CALLS_TOTAL.fetch_add(1, Ordering::SeqCst);
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let msg = wparam.0 as u32;
    // WM_XBUTTONDOWN = 0x020B, WM_NCXBUTTONDOWN = 0x00AB
    if msg == 0x020B || msg == 0x00AB {
        let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let xbutton = (ms.mouseData >> 16) as u16;
        let vk = if xbutton == 1 { 0x05 } else { 0x06 };

        HOOK_KEYDOWN_CALLS.fetch_add(1, Ordering::SeqCst);
        let mods = MODIFIER_STATE.load(Ordering::SeqCst);
        HOOK_LAST_VK.store(vk, Ordering::SeqCst);
        HOOK_LAST_MODS.store(mods, Ordering::SeqCst);

        // capture mode
        if CAPTURE_REQUEST.swap(false, Ordering::SeqCst) {
            let tx_opt = {
                let reg = match registry().lock() {
                    Ok(g) => g,
                    Err(_) => return LRESULT(1),
                };
                reg.tx.clone()
            };
            if let Some(tx) = tx_opt {
                let _ = tx.try_send(HookEvent::Captured { vk, mods });
            }
            return LRESULT(1);
        }

        let binding = HookBinding { vk, mods };
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
            HOOK_MATCHED_CALLS.fetch_add(1, Ordering::SeqCst);
            match tx.try_send(HookEvent::Fire { task_id }) {
                Ok(()) => HOOK_DISPATCH_SENT.fetch_add(1, Ordering::SeqCst),
                Err(_) => HOOK_DISPATCH_DROPPED.fetch_add(1, Ordering::SeqCst),
            };
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

// physical state of Ctrl/Alt/Shift/Win, updated from inside the hook on
// every modifier key transition. zeroed at init; tracked across the life
// of the process. see hook_proc for the update path.
static MODIFIER_STATE: AtomicU8 = AtomicU8::new(0);

// when NumLock is OFF the physical numpad keys send the cursor/nav vks
// (VK_CLEAR for Numpad5, VK_HOME for Numpad7, etc) but with the extended
// flag CLEAR, while the dedicated cursor cluster sends the same vks with
// the extended flag SET. normalize the numpad-origin presses back to their
// VK_NUMPAD* equivalents so a binding for Numpad5 fires regardless of the
// NumLock LED state. mirrors the long-standing Windows convention used by
// every input driver that distinguishes numpad-vs-nav.
fn normalize_numpad_vk(vk: u32, is_extended: bool) -> u32 {
    if is_extended {
        return vk;
    }
    match vk {
        0x0C => 0x65, // VK_CLEAR -> VK_NUMPAD5
        0x21 => 0x69, // VK_PRIOR -> VK_NUMPAD9 (Page Up)
        0x22 => 0x63, // VK_NEXT -> VK_NUMPAD3 (Page Down)
        0x23 => 0x61, // VK_END -> VK_NUMPAD1
        0x24 => 0x67, // VK_HOME -> VK_NUMPAD7
        0x25 => 0x64, // VK_LEFT -> VK_NUMPAD4
        0x26 => 0x68, // VK_UP -> VK_NUMPAD8
        0x27 => 0x66, // VK_RIGHT -> VK_NUMPAD6
        0x28 => 0x62, // VK_DOWN -> VK_NUMPAD2
        0x2D => 0x60, // VK_INSERT -> VK_NUMPAD0
        0x2E => 0x6E, // VK_DELETE -> VK_DECIMAL
        _ => vk,
    }
}

fn modifier_bit(vk: u32) -> Option<u8> {
    match vk {
        // VK_SHIFT (both), VK_LSHIFT, VK_RSHIFT
        0x10 | 0xA0 | 0xA1 => Some(MOD_SHIFT),
        // VK_CONTROL (both), VK_LCONTROL, VK_RCONTROL
        0x11 | 0xA2 | 0xA3 => Some(MOD_CTRL),
        // VK_MENU (both), VK_LMENU, VK_RMENU
        0x12 | 0xA4 | 0xA5 => Some(MOD_ALT),
        // VK_LWIN, VK_RWIN
        0x5B | 0x5C => Some(MOD_WIN),
        _ => None,
    }
}

pub fn spawn_hook_thread() -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("capscr-llkeyboard".into())
        .spawn(|| unsafe {
            // prime modifier state from physical key state — guards against
            // capscr starting while the user is already holding a modifier.
            // safe here because we're not inside the hook callback and the
            // event we read isn't the one being delivered.
            use windows::Win32::UI::Input::KeyboardAndMouse::{
                GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
            };
            let mut prime = 0u8;
            let down = |vk: u16| (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0;
            if down(VK_CONTROL.0) { prime |= MOD_CTRL; }
            if down(VK_MENU.0) { prime |= MOD_ALT; }
            if down(VK_SHIFT.0) { prime |= MOD_SHIFT; }
            if down(VK_LWIN.0) || down(VK_RWIN.0) { prime |= MOD_WIN; }
            MODIFIER_STATE.store(prime, Ordering::SeqCst);

            use windows::core::PCWSTR;
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            let hinstance = GetModuleHandleW(PCWSTR::null())
                .map(|h| windows::Win32::Foundation::HINSTANCE(h.0))
                .unwrap_or_default();

            let hook: HHOOK = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hinstance, 0) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}");
                    HOOK_INSTALLED.store(false, Ordering::SeqCst);
                    return;
                }
            };
            
            // install mouse hook for mouse side button keybinds
            let mouse_hook: Option<HHOOK> = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), hinstance, 0).ok();
            if mouse_hook.is_none() {
                tracing::warn!("SetWindowsHookExW(WH_MOUSE_LL) failed - mouse side keybinds will be unavailable in background");
            }

            HOOK_INSTALLED.store(true, Ordering::SeqCst);
            tracing::info!("LL keyboard hook installed");

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            HOOK_INSTALLED.store(false, Ordering::SeqCst);
            if let Some(mh) = mouse_hook {
                let _ = UnhookWindowsHookEx(mh);
            }
            let _ = UnhookWindowsHookEx(hook);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpad_normalization_remaps_only_non_extended() {
        // numpad-origin (extended bit clear) → remapped to VK_NUMPAD*
        assert_eq!(normalize_numpad_vk(0x0C, false), 0x65); // CLEAR -> NUMPAD5
        assert_eq!(normalize_numpad_vk(0x24, false), 0x67); // HOME -> NUMPAD7
        assert_eq!(normalize_numpad_vk(0x26, false), 0x68); // UP -> NUMPAD8
        assert_eq!(normalize_numpad_vk(0x2D, false), 0x60); // INSERT -> NUMPAD0
        assert_eq!(normalize_numpad_vk(0x2E, false), 0x6E); // DELETE -> DECIMAL
        // dedicated cursor cluster (extended bit set) → left alone
        assert_eq!(normalize_numpad_vk(0x24, true), 0x24); // HOME stays HOME
        assert_eq!(normalize_numpad_vk(0x26, true), 0x26); // UP stays UP
        // unrelated vks pass through
        assert_eq!(normalize_numpad_vk(0x41, false), 0x41); // A
        assert_eq!(normalize_numpad_vk(0x2C, false), 0x2C); // PrintScreen
    }

    #[test]
    fn modifier_bit_recognized() {
        assert_eq!(modifier_bit(0x10), Some(MOD_SHIFT));
        assert_eq!(modifier_bit(0xA2), Some(MOD_CTRL));
        assert_eq!(modifier_bit(0x5B), Some(MOD_WIN));
        assert_eq!(modifier_bit(0x12), Some(MOD_ALT));
        assert_eq!(modifier_bit(0x41), None); // A
        assert_eq!(modifier_bit(0x2C), None); // PrintScreen
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
