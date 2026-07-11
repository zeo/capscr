import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Scissors, X } from "lucide-solid";
import { api } from "../api";

function fmt(s: number): string {
  if (!isFinite(s) || s < 0) s = 0;
  const m = Math.floor(s / 60);
  const sec = (s % 60).toFixed(1).padStart(4, "0");
  return `${m}:${sec}`;
}

function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

export function TrimModal(props: {
  path: string;
  onClose: () => void;
  onDone: (msg: string) => void;
}) {
  let video: HTMLVideoElement | undefined;
  const [dur, setDur] = createSignal(0);
  const [start, setStart] = createSignal(0);
  const [end, setEnd] = createSignal(0);
  const [fast, setFast] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  // Escape closes the modal (unless a trim is in progress), matching the editor
  // and shortcuts overlay
  onMount(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape" && !busy()) {
        ev.preventDefault();
        props.onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const onMeta = () => {
    const d = video?.duration ?? 0;
    if (isFinite(d) && d > 0) {
      setDur(d);
      setEnd(d);
    }
  };

  const setStartClamped = (v: number) =>
    setStart(Math.max(0, Math.min(v, end() - 0.05)));
  const setEndClamped = (v: number) =>
    setEnd(Math.min(dur(), Math.max(v, start() + 0.05)));

  const len = () => Math.max(0, end() - start());

  const exportTrim = async () => {
    if (busy() || len() < 0.05) return;
    setBusy(true);
    setErr(null);
    try {
      const out = await api.trimMp4(props.path, start(), end(), fast());
      props.onDone(`trimmed → ${basename(out)}`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      class="modal-backdrop"
      onClick={(e) => {
        if (e.target === e.currentTarget && !busy()) props.onClose();
      }}
    >
      <div class="modal trim-modal">
        <div class="modal-head">
          <h2>
            <Scissors size={13} stroke-width={1.5} /> trim recording
          </h2>
          <button
            class="icon-btn"
            title="close"
            disabled={busy()}
            onClick={() => props.onClose()}
          >
            <X size={12} stroke-width={1.5} />
          </button>
        </div>

        <video
          ref={video}
          class="trim-video"
          src={convertFileSrc(props.path)}
          controls
          onLoadedMetadata={onMeta}
        />

        <div class="trim-row">
          <span class="trim-label">start <b>{fmt(start())}</b></span>
          <input
            type="range"
            min={0}
            max={dur()}
            step={0.05}
            value={start()}
            disabled={busy()}
            onInput={(e) => setStartClamped(parseFloat(e.currentTarget.value))}
          />
          <button
            class="btn"
            data-variant="ghost"
            disabled={busy()}
            onClick={() => setStartClamped(video?.currentTime ?? 0)}
          >
            playhead
          </button>
        </div>

        <div class="trim-row">
          <span class="trim-label">end <b>{fmt(end())}</b></span>
          <input
            type="range"
            min={0}
            max={dur()}
            step={0.05}
            value={end()}
            disabled={busy()}
            onInput={(e) => setEndClamped(parseFloat(e.currentTarget.value))}
          />
          <button
            class="btn"
            data-variant="ghost"
            disabled={busy()}
            onClick={() => setEndClamped(video?.currentTime ?? 0)}
          >
            playhead
          </button>
        </div>

        <div class="trim-foot">
          <label class="check">
            <input
              type="checkbox"
              checked={fast()}
              disabled={busy()}
              onChange={(e) => setFast(e.currentTarget.checked)}
            />
            <span class="check-label">
              fast (lossless, start snaps to keyframe)
            </span>
          </label>
          <span class="trim-len">length {fmt(len())}</span>
        </div>

        <Show when={err()}>
          <div class="flash" data-tone="err">
            {err()}
          </div>
        </Show>

        <div class="modal-actions">
          <button
            class="btn"
            data-variant="ghost"
            disabled={busy()}
            onClick={() => props.onClose()}
          >
            cancel
          </button>
          <button class="btn" disabled={busy() || len() < 0.05} onClick={exportTrim}>
            {busy() ? "exporting…" : "export trim"}
          </button>
        </div>
      </div>
    </div>
  );
}
