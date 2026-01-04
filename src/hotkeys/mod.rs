#![allow(dead_code)]

use anyhow::{anyhow, Result};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager,
};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    Screenshot,
    RecordGif,
}

impl HotkeyAction {
    pub fn all() -> &'static [HotkeyAction] {
        &[
            HotkeyAction::Screenshot,
            HotkeyAction::RecordGif,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            HotkeyAction::Screenshot => "Screenshot",
            HotkeyAction::RecordGif => "Record GIF",
        }
    }
}

pub struct HotkeyManager {
    manager: GlobalHotKeyManager,
    registered: HashMap<u32, HotkeyAction>,
    registration_errors: Vec<HotkeyRegistrationError>,
}

#[derive(Debug, Clone)]
pub struct HotkeyRegistrationError {
    pub action: HotkeyAction,
    pub hotkey: String,
    pub reason: String,
}

impl HotkeyManager {
    pub fn new() -> Result<Self> {
        let manager = GlobalHotKeyManager::new().map_err(|e| anyhow!("Failed to create hotkey manager: {}", e))?;

        Ok(Self {
            manager,
            registered: HashMap::new(),
            registration_errors: Vec::new(),
        })
    }

    pub fn register(&mut self, action: HotkeyAction, hotkey_str: &str) -> Result<()> {
        let hotkey = parse_hotkey(hotkey_str)?;
        self.manager
            .register(hotkey)
            .map_err(|e| anyhow!("Failed to register hotkey: {}", e))?;

        self.registered.insert(hotkey.id(), action);
        Ok(())
    }

    pub fn try_register(&mut self, action: HotkeyAction, hotkey_str: &str) {
        match self.register(action, hotkey_str) {
            Ok(()) => {}
            Err(e) => {
                self.registration_errors.push(HotkeyRegistrationError {
                    action,
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

    pub fn unregister(&mut self, action: HotkeyAction) -> Result<()> {
        let id_to_remove: Option<u32> = self
            .registered
            .iter()
            .find(|(_, &a)| a == action)
            .map(|(&id, _)| id);

        if let Some(id) = id_to_remove {
            self.registered.remove(&id);
        }
        Ok(())
    }

    pub fn poll(&self) -> Option<HotkeyAction> {
        if let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            return self.registered.get(&event.id).copied();
        }
        None
    }

    pub fn unregister_all(&mut self) {
        self.registered.clear();
    }
}

pub fn format_hotkey_string(s: &str) -> String {
    if let Ok(hotkey) = parse_hotkey(s) {
        format_hotkey(hotkey.mods, hotkey.key)
    } else {
        s.to_string()
    }
}

fn parse_hotkey(s: &str) -> Result<HotKey> {
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
        #[cfg(target_os = "macos")]
        parts.push("Cmd");
        #[cfg(not(target_os = "macos"))]
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
    fn test_hotkey_action_display_names() {
        for action in HotkeyAction::all() {
            assert!(!action.display_name().is_empty());
        }
    }
}
