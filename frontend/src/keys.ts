/**
 * Map a browser KeyboardEvent to the hotkey string format the Rust
 * backend (src/hotkeys/mod.rs::format_hotkey) emits and parses.
 *
 * Examples:  "Ctrl+Shift+S", "Numpad5", "Pause", "F12", "Win+Space"
 *
 * Returns null if the event was a pure modifier (Ctrl alone, Shift alone…)
 * or a key that the backend cannot represent — caller should keep capturing.
 */

const CODE_TO_KEY: Record<string, string> = {
  Backspace: "Backspace",
  Tab: "Tab",
  Enter: "Enter",
  ShiftLeft: "",
  ShiftRight: "",
  ControlLeft: "",
  ControlRight: "",
  AltLeft: "",
  AltRight: "",
  MetaLeft: "",
  MetaRight: "",
  CapsLock: "",
  Escape: "Esc",
  Space: "Space",
  PageUp: "PageUp",
  PageDown: "PageDown",
  End: "End",
  Home: "Home",
  ArrowLeft: "Left",
  ArrowUp: "Up",
  ArrowRight: "Right",
  ArrowDown: "Down",
  Insert: "Insert",
  Delete: "Delete",
  PrintScreen: "PrintScreen",
  Pause: "Pause",
  ScrollLock: "ScrollLock",
  NumpadAdd: "NumpadAdd",
  NumpadSubtract: "NumpadSubtract",
  NumpadMultiply: "NumpadMultiply",
  NumpadDivide: "NumpadDivide",
  NumpadDecimal: "NumpadDecimal",
  NumpadEnter: "NumpadEnter",
};

function mapCode(code: string): string | null {
  if (code in CODE_TO_KEY) {
    const v = CODE_TO_KEY[code];
    return v || null;
  }
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^Numpad[0-9]$/.test(code)) return code;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return null;
}

export interface HotkeyParts {
  modifiers: string[]; // ["Ctrl", "Shift", ...] in canonical order
  key: string;
  combined: string; // "Ctrl+Shift+S"
}

export function eventToHotkey(e: KeyboardEvent): HotkeyParts | null {
  const key = mapCode(e.code);
  if (!key) return null;
  const modifiers: string[] = [];
  if (e.ctrlKey) modifiers.push("Ctrl");
  if (e.altKey) modifiers.push("Alt");
  if (e.shiftKey) modifiers.push("Shift");
  if (e.metaKey) modifiers.push("Win");
  return {
    modifiers,
    key,
    combined: [...modifiers, key].join("+"),
  };
}

/** Split an existing hotkey string into parts for chip rendering. */
export function splitHotkey(s: string): string[] {
  return s
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
}

/**
 * A hotkey of just `A`, `B`, `1`, … with no modifier will steal global
 * keystrokes from any focused app. Warn but don't block — power users
 * may legitimately want Numpad5 etc.
 */
export function isRiskyHotkey(parts: HotkeyParts): boolean {
  if (parts.modifiers.length > 0) return false;
  const safeBare = new Set([
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8",
    "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
    "Mouse4", "Mouse5",
    "Pause", "PrintScreen", "ScrollLock",
    "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4",
    "Numpad5", "Numpad6", "Numpad7", "Numpad8", "Numpad9",
    "NumpadAdd", "NumpadSubtract", "NumpadMultiply",
    "NumpadDivide", "NumpadDecimal", "NumpadEnter",
  ]);
  return !safeBare.has(parts.key);
}
