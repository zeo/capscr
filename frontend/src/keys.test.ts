/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
import fixtureText from "../../src/hotkeys/fixtures/hotkeys.txt?raw";
import { eventToHotkey, isRiskyHotkey, META_LABEL, splitHotkey } from "./keys";

type Mods = Partial<Pick<KeyboardEvent, "ctrlKey" | "altKey" | "shiftKey" | "metaKey">>;

const press = (code: string, mods: Mods = {}) =>
  eventToHotkey({ code, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...mods } as KeyboardEvent);

const fixture = fixtureText.split(/\r?\n/).filter(Boolean);

const NAMED_CODES = [
  "Backspace", "Tab", "Enter", "Escape", "Space", "PageUp", "PageDown", "End", "Home",
  "ArrowLeft", "ArrowUp", "ArrowRight", "ArrowDown", "Insert", "Delete", "PrintScreen", "Pause",
  "ScrollLock", "NumpadAdd", "NumpadSubtract", "NumpadMultiply", "NumpadDivide", "NumpadDecimal",
  "NumpadEnter",
];
const range = (n: number, from = 0) => Array.from({ length: n }, (_, i) => i + from);
const EVERY_CODE = [
  ...NAMED_CODES,
  ...range(26).map((i) => `Key${String.fromCharCode(65 + i)}`),
  ...range(10).map((d) => `Digit${d}`),
  ...range(10).map((d) => `Numpad${d}`),
  ...range(24, 1).map((n) => `F${n}`),
];

describe("eventToHotkey", () => {
  it("joins modifiers in the backend's canonical order", () => {
    expect(press("KeyS", { ctrlKey: true, shiftKey: true })?.combined).toBe("Ctrl+Shift+S");
    expect(press("KeyK", { metaKey: true, shiftKey: true, altKey: true, ctrlKey: true })?.combined).toBe(
      "Ctrl+Alt+Shift+Win+K",
    );
  });

  it("names keys the way the backend parses them", () => {
    expect(press("Escape")?.key).toBe("Esc");
    expect(press("ArrowLeft")?.key).toBe("Left");
    expect(press("Digit7")?.key).toBe("7");
    expect(press("Numpad5")?.key).toBe("Numpad5");
    expect(press("PrintScreen")?.key).toBe("PrintScreen");
    expect(press("F23")?.key).toBe("F23");
  });

  it("keeps capturing on a bare modifier", () => {
    for (const code of ["ShiftLeft", "ControlRight", "AltLeft", "MetaLeft", "CapsLock"]) {
      expect(press(code, { shiftKey: true })).toBeNull();
    }
  });

  it("rejects keys the backend can't represent", () => {
    expect(press("F25")).toBeNull();
    expect(press("IntlBackslash")).toBeNull();
    expect(press("Numpad10")).toBeNull();
  });

  it("emits exactly the strings in the shared hotkey fixture", () => {
    const emitted = [
      press("KeyS", { ctrlKey: true, shiftKey: true }),
      press("KeyK", { ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }),
      press("Space", { metaKey: true }),
      press("F4", { altKey: true }),
      ...EVERY_CODE.map((code) => press(code)),
    ].map((parts) => parts?.combined);
    expect(emitted).toEqual(fixture);
  });
});

describe("hotkey helpers", () => {
  it("flags bare letters as risky but not function keys or chords", () => {
    expect(isRiskyHotkey(press("KeyA")!)).toBe(true);
    expect(isRiskyHotkey(press("Digit1")!)).toBe(true);
    expect(isRiskyHotkey(press("F5")!)).toBe(false);
    expect(isRiskyHotkey(press("Numpad5")!)).toBe(false);
    expect(isRiskyHotkey(press("KeyA", { ctrlKey: true })!)).toBe(false);
  });

  it("splits stored strings into chips with the platform's meta label", () => {
    expect(splitHotkey("Ctrl + Win+S")).toEqual(["Ctrl", META_LABEL, "S"]);
    expect(splitHotkey("")).toEqual([]);
  });
});
