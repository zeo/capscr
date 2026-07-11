import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { convertFileSrc } from "@tauri-apps/api/core";
import { X } from "lucide-solid";
import { api } from "../api";

export function PinView(props: { label: string }) {
  const [imagePath, setImagePath] = createSignal<string | null>(null);
  const [opacity, setOpacity] = createSignal<number>(1.0);
  const [hovered, setHovered] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  // the pin window is created hidden and revealed once we know its size. every
  // path has to reach a reveal, or a failed load leaves an invisible window the
  // user can't find to close.
  const reveal = async (w?: number, h?: number) => {
    const win = getCurrentWindow();
    if (w && h) {
      await win.setSize(new LogicalSize(w, h)).catch(() => {});
    }
    await win.show().catch(() => {});
  };

  onMount(() => {
    api.getPinnedImagePath(props.label)
      .then((path) => {
        if (!path) {
          setError("this pin's image is no longer available");
          reveal(320, 120);
          return;
        }
        setImagePath(path);
        // Load image to read its natural width & height, then resize the window
        const img = new Image();
        img.onload = () => {
          let w = img.naturalWidth;
          let h = img.naturalHeight;

          // Optional: cap size at 85% of screen dimensions to keep it reasonable
          const maxW = window.screen.availWidth * 0.85;
          const maxH = window.screen.availHeight * 0.85;
          if (w > maxW || h > maxH) {
            const ratio = Math.min(maxW / w, maxH / h);
            w = Math.round(w * ratio);
            h = Math.round(h * ratio);
          }
          reveal(w, h);
        };
        img.onerror = () => {
          setImagePath(null);
          setError("couldn't load this pin's image");
          reveal(320, 120);
        };
        img.src = convertFileSrc(path);
      })
      .catch((e) => {
        console.error("Failed to load pinned image path:", e);
        setError("couldn't load this pin");
        reveal(320, 120);
      });
  });

  const closePin = () => {
    getCurrentWindow().close();
  };

  // Escape closes the pin while it's focused, so a pin whose controls are out of
  // reach (mouse away from its corner) still has a keyboard way out
  onMount(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") closePin();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <div
      class="pin-container"
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <div data-tauri-drag-region class="pin-drag-region" style={{ opacity: opacity() }}>
        {imagePath() ? (
          <img
            src={convertFileSrc(imagePath()!)}
            alt="pinned"
            class="pin-image"
            draggable={false}
          />
        ) : (
          <div class="pin-error">{error() ?? "nothing pinned"}</div>
        )}
      </div>
      <Show when={hovered()}>
        <div class="pin-controls">
          <input
            type="range"
            min="0.1"
            max="1.0"
            step="0.05"
            value={opacity()}
            onInput={(e) => setOpacity(parseFloat(e.currentTarget.value))}
            class="pin-opacity-slider"
            title="adjust opacity"
          />
          <button class="pin-close-btn" onClick={closePin} title="close">
            <X size={12} />
          </button>
        </div>
      </Show>
    </div>
  );
}
