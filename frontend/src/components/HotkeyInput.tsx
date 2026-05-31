import { createSignal, For, onCleanup, Show } from "solid-js";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { X } from "lucide-solid";
import {
  splitHotkey,
  eventToHotkey,
} from "../keys";
import { api } from "../api";

interface Props {
  value: string;
  onChange: (next: string) => void;
}

interface CapturedPayload {
  vk: number;
  mods: number;
  hotkey: string;
}

const RISKY_BARE_OK = new Set([
  "F1","F2","F3","F4","F5","F6","F7","F8","F9","F10","F11","F12",
  "F13","F14","F15","F16","F17","F18","F19","F20","F21","F22","F23","F24",
  "Mouse4","Mouse5",
  "Pause","PrintScreen","ScrollLock",
  "Numpad0","Numpad1","Numpad2","Numpad3","Numpad4",
  "Numpad5","Numpad6","Numpad7","Numpad8","Numpad9",
  "NumpadAdd","NumpadSubtract","NumpadMultiply","NumpadDivide","NumpadDecimal","NumpadEnter",
]);

/**
 * Click → arms the backend LL hook to capture the next non-modifier
 * keydown → records the exact vk Windows delivers (so NumLock state,
 * FN-modified laptop keys, and unusual layouts all bind correctly).
 * Esc cancels. Backspace clears.
 */
export function HotkeyInput(props: Props) {
  const [capturing, setCapturing] = createSignal(false);
  const [warning, setWarning] = createSignal<string | null>(null);

  let unlisten: UnlistenFn | null = null;
  let escHandler: ((e: KeyboardEvent) => void) | null = null;
  let mousedownHandler: ((e: MouseEvent) => void) | null = null;

  const stop = () => {
    if (unlisten) { unlisten(); unlisten = null; }
    if (escHandler) {
      window.removeEventListener("keydown", escHandler, true);
      escHandler = null;
    }
    if (mousedownHandler) {
      window.removeEventListener("mousedown", mousedownHandler, true);
      mousedownHandler = null;
    }
    void api.cancelHotkeyCapture().catch(() => {});
    setCapturing(false);
  };

  const accept = (payload: CapturedPayload) => {
    const stripped = payload.hotkey.split("+").pop() ?? payload.hotkey;
    const hasModifier = payload.mods !== 0;
    if (!hasModifier && !RISKY_BARE_OK.has(stripped)) {
      setWarning(
        `${payload.hotkey} would steal that key from every app — add a modifier (Ctrl / Alt / Shift / Win).`,
      );
      // re-arm for another attempt
      void api.startHotkeyCapture().catch((e) => {
        setWarning(String(e));
        stop();
      });
      return;
    }
    props.onChange(payload.hotkey);
    stop();
  };

  const begin = async () => {
    if (capturing()) return;
    setWarning(null);

    try {
      unlisten = await listen<CapturedPayload>(
        "capscr://hotkey-captured",
        (e) => accept(e.payload),
      );
      await api.startHotkeyCapture();
      setCapturing(true);
    } catch (e) {
      setWarning(String(e));
      stop();
      return;
    }

    // browser-side key capturing and Esc/Backspace handling — this acts as
    // a robust fallback when the application has focus, because Windows
    // conflicts and prioritizes the webview's raw input over global low-level
    // hooks when the window is in the foreground.
    escHandler = (e: KeyboardEvent) => {
      if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
        e.preventDefault();
        e.stopPropagation();
        stop();
      } else if (e.code === "Backspace" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
        e.preventDefault();
        e.stopPropagation();
        props.onChange("");
        stop();
      } else {
        const parsed = eventToHotkey(e);
        if (parsed) {
          e.preventDefault();
          e.stopPropagation();
          accept({
            vk: 0,
            mods: parsed.modifiers.length,
            hotkey: parsed.combined,
          });
        }
      }
    };
    window.addEventListener("keydown", escHandler, true);

    mousedownHandler = (e: MouseEvent) => {
      // 3 is back (Mouse4), 4 is forward (Mouse5)
      if (e.button === 3 || e.button === 4) {
        e.preventDefault();
        e.stopPropagation();

        const modifiers: string[] = [];
        if (e.ctrlKey) modifiers.push("Ctrl");
        if (e.altKey) modifiers.push("Alt");
        if (e.shiftKey) modifiers.push("Shift");
        if (e.metaKey) modifiers.push("Win");

        const btnName = e.button === 3 ? "Mouse4" : "Mouse5";
        const combined = modifiers.length > 0 ? `${modifiers.join("+")}+${btnName}` : btnName;

        accept({
          vk: e.button === 3 ? 0x05 : 0x06,
          mods: modifiers.length,
          hotkey: combined,
        });
      }
    };
    window.addEventListener("mousedown", mousedownHandler, true);
  };

  onCleanup(() => stop());

  const clear = (e: MouseEvent) => {
    e.stopPropagation();
    props.onChange("");
    setWarning(null);
    if (capturing()) stop();
  };

  return (
    <div>
      <div
        class="hk"
        classList={{ "is-capturing": capturing() }}
        onClick={begin}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (!capturing() && (e.key === "Enter" || e.key === " ")) {
            e.preventDefault();
            begin();
          }
        }}
      >
        <div class="hk-display">
          <Show
            when={capturing()}
            fallback={
              <Show
                when={props.value}
                fallback={<span class="ph">click to bind…</span>}
              >
                <For each={splitHotkey(props.value)}>
                  {(p, i) => (
                    <>
                      {i() > 0 && <span class="hk-sep">+</span>}
                      <span class="hk-chip">{p}</span>
                    </>
                  )}
                </For>
              </Show>
            }
          >
            <span>press a key… (esc to cancel)</span>
          </Show>
        </div>
        <Show when={props.value && !capturing()}>
          <button
            type="button"
            class="hk-clear"
            aria-label="Clear hotkey"
            onClick={clear}
          >
            <X size={12} stroke-width={1.5} />
          </button>
        </Show>
      </div>
      <Show when={warning()}>
        <div class="hk-warning">{warning()}</div>
      </Show>
    </div>
  );
}
