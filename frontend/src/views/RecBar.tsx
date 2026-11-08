import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { IS_LINUX } from "../keys";

type BarState = {
  mode: "idle" | "armed" | "recording" | "paused";
  active_ms: number;
  width: number;
  height: number;
};

const PRESETS: [string, number, number][] = [
  ["640 × 480", 640, 480],
  ["800 × 600", 800, 600],
  ["1280 × 720", 1280, 720],
  ["1920 × 1080", 1920, 1080],
  ["Full screen", 0, 0],
];

const clock = (ms: number) => {
  const total = Math.floor(ms / 1000);
  const mm = String(Math.floor(total / 60)).padStart(2, "0");
  const ss = String(total % 60).padStart(2, "0");
  return `${mm}:${ss}`;
};

// recording control bar. the backend pushes every state change; the clock
// only ticks locally between pushes
export function RecBar() {
  const [bar, setBar] = createSignal<BarState>({
    mode: "idle",
    active_ms: 0,
    width: 0,
    height: 0,
  });
  const [now, setNow] = createSignal(performance.now());
  let receivedAt = performance.now();
  const apply = (state: BarState) => {
    receivedAt = performance.now();
    setNow(receivedAt);
    setBar(state);
  };
  const elapsed = () =>
    bar().active_ms + (bar().mode === "recording" ? now() - receivedAt : 0);

  const tick = setInterval(() => {
    if (bar().mode === "recording") setNow(performance.now());
  }, 250);
  onCleanup(() => clearInterval(tick));

  const record = () => invoke("recbar_record").catch(() => {});
  const cancel = () => invoke("recbar_cancel").catch(() => {});
  const stop = () => invoke("stop_recording").catch(() => {});
  const togglePause = () =>
    invoke("recbar_pause", { paused: bar().mode !== "paused" }).catch(() => {});

  const onKey = (e: KeyboardEvent) => {
    const mode = bar().mode;
    if (mode === "armed" && e.key === "Enter") record();
    else if (mode === "armed" && e.key === "Escape") cancel();
    else if ((mode === "recording" || mode === "paused") && e.key === " ") togglePause();
    else return;
    e.preventDefault();
  };

  onMount(() => {
    // clear every layer of the app background so only the pill shows; the
    // window itself can come up larger than the bar on gtk-wayland
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    const root = document.getElementById("root");
    if (root) {
      root.style.background = "transparent";
    }
    const unlisten = listen<BarState>("recbar://state", (e) => apply(e.payload));
    invoke<BarState>("recbar_status").then(apply).catch(() => {});
    window.addEventListener("keydown", onKey);
    onCleanup(() => {
      unlisten.then((off) => off());
      window.removeEventListener("keydown", onKey);
    });
  });

  return (
    <div class="recbar" classList={{ "recbar-wide": !IS_LINUX }} data-mode={bar().mode}>
      <Show
        when={bar().mode === "armed"}
        fallback={
          <>
            <span class="recbar-dot" />
            <span class="recbar-time">{clock(elapsed())}</span>
            <button type="button" class="recbar-btn" onClick={togglePause}>
              {bar().mode === "paused" ? "resume" : "pause"}
            </button>
            <button type="button" class="recbar-btn recbar-stop" onClick={stop}>
              stop
            </button>
          </>
        }
      >
        <button type="button" class="recbar-btn recbar-rec" onClick={record}>
          rec
        </button>
        <select
          class="recbar-preset"
          aria-label="frame size"
          onChange={(e) => {
            const [width, height] = e.currentTarget.value.split("x").map(Number);
            e.currentTarget.value = "";
            invoke("recbar_preset", { width, height }).catch(() => {});
          }}
        >
          <option value="">size</option>
          <For each={PRESETS}>
            {([label, w, h]) => <option value={`${w}x${h}`}>{label}</option>}
          </For>
        </select>
      </Show>
      <Show when={!IS_LINUX}>
        <span class="recbar-size">
          {bar().width}×{bar().height}
        </span>
      </Show>
      <Show when={bar().mode === "armed"}>
        <button type="button" class="recbar-close" aria-label="cancel" onClick={cancel}>
          ×
        </button>
      </Show>
    </div>
  );
}
