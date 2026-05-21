import { createSignal, For, onCleanup, Show } from "solid-js";
import { X } from "lucide-solid";
import {
  eventToHotkey,
  isRiskyHotkey,
  splitHotkey,
} from "../keys";

interface Props {
  value: string;
  onChange: (next: string) => void;
}

/**
 * Click → captures next key combo → emits backend-format string.
 * Esc cancels. Backspace clears. Bare-letter hotkeys warn but accept.
 */
export function HotkeyInput(props: Props) {
  const [capturing, setCapturing] = createSignal(false);
  const [warning, setWarning] = createSignal<string | null>(null);

  let cleanup: (() => void) | null = null;

  const stop = () => {
    cleanup?.();
    cleanup = null;
    setCapturing(false);
  };

  const begin = () => {
    if (capturing()) return;
    setCapturing(true);
    setWarning(null);

    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();

      if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
        stop();
        return;
      }

      if (e.code === "Backspace" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) {
        props.onChange("");
        stop();
        return;
      }

      const parsed = eventToHotkey(e);
      if (!parsed) return; // pure modifier — keep listening

      if (isRiskyHotkey(parsed)) {
        // bare letter/digit hotkeys steal that key system-wide and lock
        // the user out of typing it anywhere else. refuse the bind and
        // keep listening so they can press a combo with a modifier
        setWarning(
          `${parsed.combined} would steal that key from every app — add a modifier (Ctrl / Alt / Shift / Win).`,
        );
        return;
      }
      props.onChange(parsed.combined);
      stop();
    };

    const onBlur = () => stop();

    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("blur", onBlur);

    cleanup = () => {
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("blur", onBlur);
    };
  };

  onCleanup(() => cleanup?.());

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
