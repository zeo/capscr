#![allow(dead_code)]

use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::collections::HashMap;

pub struct HotkeyManager {
    manager: GlobalHotKeyManager,
    // map of hotkey_id → (hotkey, task_id) so unregister_all can hand the
    // HotKey values back to the OS — clearing the map alone does not remove
    // the kernel-level registration and causes re-registration to fail.
    registered: HashMap<u32, (HotKey, String)>,
    registration_errors: Vec<HotkeyRegistrationError>,
}

#[derive(Debug, Clone)]
pub struct HotkeyRegistrationError {
    pub task_id: String,
    pub hotkey: String,
    pub reason: String,
}

impl HotkeyManager {
    pub fn new() -> Result<Self> {
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| anyhow!("Failed to create hotkey manager: {}", e))?;
        Ok(Self {
            manager,
            registered: HashMap::new(),
            registration_errors: Vec::new(),
        })
    }

    pub fn register(&mut self, task_id: impl Into<String>, hotkey_str: &str) -> Result<()> {
        if is_risky_bare(hotkey_str) {
            return Err(anyhow!(
                "'{}' has no modifier — it would steal that key from every \
                 app. Add Ctrl / Alt / Shift / Win or use an F-key / Numpad / \
                 PrintScreen.",
                hotkey_str
            ));
        }
        let hotkey = parse_hotkey(hotkey_str)?;
        self.manager
            .register(hotkey)
            .map_err(|e| anyhow!("Failed to register hotkey: {}", e))?;
        self.registered.insert(hotkey.id(), (hotkey, task_id.into()));
        Ok(())
    }

    pub fn try_register(&mut self, task_id: impl Into<String>, hotkey_str: &str) {
        // tasks with no hotkey assigned are valid — silently skip them rather
        // than pushing an error that surfaces as a notification
        if hotkey_str.is_empty() {
            return;
        }
        let task_id = task_id.into();
        match self.register(task_id.clone(), hotkey_str) {
            Ok(()) => {}
            Err(e) => {
                self.registration_errors.push(HotkeyRegistrationError {
                    task_id,
                    hotkey: hotkey_str.to_string(),
                    reason: e.to_string(),
                });
            }
        }
    }

    pub fn take_errors(&mut self) -> Vec<HotkeyRegistrationError> {
        std::mem::take(&mut self.registration_errors)
    }

    pub fn has_errors(&self) -> bool {
        !self.registration_errors.is_empty()
    }

    pub fn poll(&self) -> Option<String> {
        if let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            return self.registered.get(&event.id).map(|(_, id)| id.clone());
        }
        None
    }

    pub fn task_for_event_id(&self, id: u32) -> Option<String> {
        self.registered.get(&id).map(|(_, task_id)| task_id.clone())
    }

    pub fn unregister_all(&mut self) {
        let hotkeys: Vec<HotKey> = self.registered.values().map(|(hk, _)| *hk).collect();
        if !hotkeys.is_empty() {
            let _ = self.manager.unregister_all(&hotkeys);
        }
        self.registered.clear();
    }
}

/// bare letter or digit hotkeys (no modifier) steal that key system-wide
/// and lock the user out of typing it anywhere else. Whitelist the keys
/// that the frontend already marks safe-bare (F-keys, numpad, PrintScreen,
/// Pause, ScrollLock) and reject the rest at registration time. Mirrors
/// `isRiskyHotkey` in `frontend/src/keys.ts`.
pub fn is_risky_bare(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.contains('+') {
        return false;
    }
    let safe = matches!(
        s,
        "F1" | "F2" | "F3" | "F4" | "F5" | "F6" | "F7" | "F8"
            | "F9" | "F10" | "F11" | "F12" | "F13" | "F14" | "F15"
            | "F16" | "F17" | "F18" | "F19" | "F20" | "F21" | "F22"
            | "F23" | "F24"
            | "Pause" | "PrintScreen" | "ScrollLock"
            | "Numpad0" | "Numpad1" | "Numpad2" | "Numpad3" | "Numpad4"
            | "Numpad5" | "Numpad6" | "Numpad7" | "Numpad8" | "Numpad9"
            | "NumpadAdd" | "NumpadSubtract" | "NumpadMultiply"
            | "NumpadDivide" | "NumpadDecimal" | "NumpadEnter"
    );
    !safe
}

pub fn format_hotkey_string(s: &str) -> String {
    if let Ok(hotkey) = parse_hotkey(s) {
        format_hotkey(hotkey.mods, hotkey.key)
    } else {
        s.to_string()
    }
}

pub fn parse_hotkey(s: &str) -> Result<HotKey> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return Err(anyhow!("Empty hotkey string"));
    }
    let mut modifiers = Modifiers::empty();
    let mut key_code: Option<Code> = None;
    for part in parts {
        let lower = part.to_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "super" | "win" | "meta" | "cmd" => modifiers |= Modifiers::SUPER,
            _ => {
                key_code = Some(parse_key_code(part)?);
            }
        }
    }
    let code = key_code.ok_or_else(|| anyhow!("No key specified in hotkey"))?;
    Ok(HotKey::new(Some(modifiers), code))
}

fn parse_key_code(s: &str) -> Result<Code> {
    let upper = s.to_uppercase();
    let code = match upper.as_str() {
        "A" => Code::KeyA,
        "B" => Code::KeyB,
        "C" => Code::KeyC,
        "D" => Code::KeyD,
        "E" => Code::KeyE,
        "F" => Code::KeyF,
        "G" => Code::KeyG,
        "H" => Code::KeyH,
        "I" => Code::KeyI,
        "J" => Code::KeyJ,
        "K" => Code::KeyK,
        "L" => Code::KeyL,
        "M" => Code::KeyM,
        "N" => Code::KeyN,
        "O" => Code::KeyO,
        "P" => Code::KeyP,
        "Q" => Code::KeyQ,
        "R" => Code::KeyR,
        "S" => Code::KeyS,
        "T" => Code::KeyT,
        "U" => Code::KeyU,
        "V" => Code::KeyV,
        "W" => Code::KeyW,
        "X" => Code::KeyX,
        "Y" => Code::KeyY,
        "Z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        "F13" => Code::F13,
        "F14" => Code::F14,
        "F15" => Code::F15,
        "F16" => Code::F16,
        "F17" => Code::F17,
        "F18" => Code::F18,
        "F19" => Code::F19,
        "F20" => Code::F20,
        "F21" => Code::F21,
        "F22" => Code::F22,
        "F23" => Code::F23,
        "F24" => Code::F24,
        "SPACE" => Code::Space,
        "ENTER" | "RETURN" => Code::Enter,
        "TAB" => Code::Tab,
        "ESCAPE" | "ESC" => Code::Escape,
        "BACKSPACE" => Code::Backspace,
        "DELETE" | "DEL" => Code::Delete,
        "INSERT" | "INS" => Code::Insert,
        "HOME" => Code::Home,
        "END" => Code::End,
        "PAGEUP" | "PGUP" => Code::PageUp,
        "PAGEDOWN" | "PGDN" => Code::PageDown,
        "UP" => Code::ArrowUp,
        "DOWN" => Code::ArrowDown,
        "LEFT" => Code::ArrowLeft,
        "RIGHT" => Code::ArrowRight,
        "PRINTSCREEN" | "PRTSC" | "PRINT" => Code::PrintScreen,
        "PAUSE" | "PAUSEBREAK" | "BREAK" => Code::Pause,
        "SCROLLLOCK" | "SCROLL" => Code::ScrollLock,
        "NUMPAD0" | "NUM0" | "KP0" => Code::Numpad0,
        "NUMPAD1" | "NUM1" | "KP1" => Code::Numpad1,
        "NUMPAD2" | "NUM2" | "KP2" => Code::Numpad2,
        "NUMPAD3" | "NUM3" | "KP3" => Code::Numpad3,
        "NUMPAD4" | "NUM4" | "KP4" => Code::Numpad4,
        "NUMPAD5" | "NUM5" | "KP5" => Code::Numpad5,
        "NUMPAD6" | "NUM6" | "KP6" => Code::Numpad6,
        "NUMPAD7" | "NUM7" | "KP7" => Code::Numpad7,
        "NUMPAD8" | "NUM8" | "KP8" => Code::Numpad8,
        "NUMPAD9" | "NUM9" | "KP9" => Code::Numpad9,
        "NUMPADADD" | "NUMADD" | "KPADD" => Code::NumpadAdd,
        "NUMPADSUB" | "NUMSUB" | "KPSUB" | "NUMPADSUBTRACT" => Code::NumpadSubtract,
        "NUMPADMUL" | "NUMMUL" | "KPMUL" | "NUMPADMULTIPLY" => Code::NumpadMultiply,
        "NUMPADDIV" | "NUMDIV" | "KPDIV" | "NUMPADDIVIDE" => Code::NumpadDivide,
        "NUMPADDOT" | "NUMDOT" | "KPDOT" | "NUMPADDECIMAL" => Code::NumpadDecimal,
        "NUMPADENTER" | "KPENTER" => Code::NumpadEnter,
        _ => return Err(anyhow!("Unknown key: {}", s)),
    };
    Ok(code)
}

pub fn format_hotkey(modifiers: Modifiers, code: Code) -> String {
    let mut parts = Vec::new();
    if modifiers.contains(Modifiers::CONTROL) {
        parts.push("Ctrl");
    }
    if modifiers.contains(Modifiers::ALT) {
        parts.push("Alt");
    }
    if modifiers.contains(Modifiers::SHIFT) {
        parts.push("Shift");
    }
    if modifiers.contains(Modifiers::SUPER) {
        parts.push("Win");
    }
    parts.push(format_code(code));
    parts.join("+")
}

pub fn format_code(code: Code) -> &'static str {
    match code {
        Code::KeyA => "A",
        Code::KeyB => "B",
        Code::KeyC => "C",
        Code::KeyD => "D",
        Code::KeyE => "E",
        Code::KeyF => "F",
        Code::KeyG => "G",
        Code::KeyH => "H",
        Code::KeyI => "I",
        Code::KeyJ => "J",
        Code::KeyK => "K",
        Code::KeyL => "L",
        Code::KeyM => "M",
        Code::KeyN => "N",
        Code::KeyO => "O",
        Code::KeyP => "P",
        Code::KeyQ => "Q",
        Code::KeyR => "R",
        Code::KeyS => "S",
        Code::KeyT => "T",
        Code::KeyU => "U",
        Code::KeyV => "V",
        Code::KeyW => "W",
        Code::KeyX => "X",
        Code::KeyY => "Y",
        Code::KeyZ => "Z",
        Code::Digit0 => "0",
        Code::Digit1 => "1",
        Code::Digit2 => "2",
        Code::Digit3 => "3",
        Code::Digit4 => "4",
        Code::Digit5 => "5",
        Code::Digit6 => "6",
        Code::Digit7 => "7",
        Code::Digit8 => "8",
        Code::Digit9 => "9",
        Code::F1 => "F1",
        Code::F2 => "F2",
        Code::F3 => "F3",
        Code::F4 => "F4",
        Code::F5 => "F5",
        Code::F6 => "F6",
        Code::F7 => "F7",
        Code::F8 => "F8",
        Code::F9 => "F9",
        Code::F10 => "F10",
        Code::F11 => "F11",
        Code::F12 => "F12",
        Code::Space => "Space",
        Code::Enter => "Enter",
        Code::Tab => "Tab",
        Code::Escape => "Esc",
        Code::Backspace => "Backspace",
        Code::Delete => "Delete",
        Code::Insert => "Insert",
        Code::Home => "Home",
        Code::End => "End",
        Code::PageUp => "PageUp",
        Code::PageDown => "PageDown",
        Code::ArrowUp => "Up",
        Code::ArrowDown => "Down",
        Code::ArrowLeft => "Left",
        Code::ArrowRight => "Right",
        Code::PrintScreen => "PrintScreen",
        Code::Pause => "Pause",
        Code::ScrollLock => "ScrollLock",
        Code::Numpad0 => "Numpad0",
        Code::Numpad1 => "Numpad1",
        Code::Numpad2 => "Numpad2",
        Code::Numpad3 => "Numpad3",
        Code::Numpad4 => "Numpad4",
        Code::Numpad5 => "Numpad5",
        Code::Numpad6 => "Numpad6",
        Code::Numpad7 => "Numpad7",
        Code::Numpad8 => "Numpad8",
        Code::Numpad9 => "Numpad9",
        Code::NumpadAdd => "NumpadAdd",
        Code::NumpadSubtract => "NumpadSubtract",
        Code::NumpadMultiply => "NumpadMultiply",
        Code::NumpadDivide => "NumpadDivide",
        Code::NumpadDecimal => "NumpadDecimal",
        Code::NumpadEnter => "NumpadEnter",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_hotkey() {
        let result = format_hotkey(Modifiers::CONTROL | Modifiers::SHIFT, Code::KeyS);
        assert!(result.contains("Ctrl"));
        assert!(result.contains("Shift"));
        assert!(result.contains("S"));
    }

    #[test]
    fn test_format_code_letters() {
        assert_eq!(format_code(Code::KeyA), "A");
        assert_eq!(format_code(Code::KeyZ), "Z");
    }

    #[test]
    fn test_format_code_numbers() {
        assert_eq!(format_code(Code::Digit0), "0");
        assert_eq!(format_code(Code::Digit9), "9");
    }

    #[test]
    fn test_format_code_function_keys() {
        assert_eq!(format_code(Code::F1), "F1");
        assert_eq!(format_code(Code::F12), "F12");
    }

    #[test]
    fn test_parse_numpad5() {
        let hk = parse_hotkey("Numpad5").expect("parse");
        assert_eq!(hk.mods, Modifiers::empty());
    }

    #[test]
    fn test_parse_pause() {
        let hk = parse_hotkey("Pause").expect("parse");
        assert_eq!(hk.mods, Modifiers::empty());
    }

    #[test]
    fn test_parse_ctrl_shift_s() {
        let hk = parse_hotkey("Ctrl+Shift+S").expect("parse");
        assert!(hk.mods.contains(Modifiers::CONTROL));
        assert!(hk.mods.contains(Modifiers::SHIFT));
    }

    #[test]
    fn test_parse_printscreen() {
        let hk = parse_hotkey("PrintScreen").expect("parse PrintScreen");
        assert_eq!(hk.key, Code::PrintScreen);
        assert_eq!(hk.mods, Modifiers::empty());
    }

    #[test]
    fn test_parse_empty_is_err() {
        assert!(parse_hotkey("").is_err(), "empty string must fail");
    }

    #[test]
    fn test_empty_hotkey_skipped_silently() {
        // try_register with "" must not push to registration_errors — tasks
        // with no hotkey assigned are valid (user chose not to bind them)
        // and must not trigger the startup-conflict notification.
        let mut hm = HotkeyManager {
            manager: GlobalHotKeyManager::new().expect("manager"),
            registered: HashMap::new(),
            registration_errors: Vec::new(),
        };
        hm.try_register("task-no-key", "");
        assert!(hm.take_errors().is_empty(), "empty hotkey must not produce an error");
    }

    #[test]
    fn test_parse_roundtrip() {
        // verify that the format→parse round-trip is stable for common combos
        let cases = &[
            "PrintScreen",
            "Ctrl+Shift+G",
            "Ctrl+Shift+S",
            "Pause",
            "F12",
            "Numpad5",
        ];
        for &s in cases {
            let hk = parse_hotkey(s).unwrap_or_else(|e| panic!("parse '{s}' failed: {e}"));
            let formatted = format_hotkey(hk.mods, hk.key);
            let hk2 = parse_hotkey(&formatted)
                .unwrap_or_else(|e| panic!("re-parse '{formatted}' failed: {e}"));
            assert_eq!(hk.id(), hk2.id(), "roundtrip mismatch for '{s}'");
        }
    }
}
