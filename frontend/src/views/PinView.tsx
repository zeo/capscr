import { createSignal, onMount, Show } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { convertFileSrc } from "@tauri-apps/api/core";
import { X } from "lucide-solid";
import { api } from "../api";

export function PinView(props: { label: string }) {
  const [imagePath, setImagePath] = createSignal<string | null>(null);
  const [opacity, setOpacity] = createSignal<number>(1.0);
  const [hovered, setHovered] = createSignal(false);

  onMount(() => {
    api.getPinnedImagePath(props.label)
      .then((path) => {
        if (path) {
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

            const win = getCurrentWindow();
            win.setSize(new LogicalSize(w, h)).then(() => {
              win.show();
            });
          };
          img.src = convertFileSrc(path);
        }
      })
      .catch((e) => {
        console.error("Failed to load pinned image path:", e);
      });
  });

  const closePin = () => {
    getCurrentWindow().close();
  };

  return (
    <div
      class="pin-container"
      style={{ opacity: opacity() }}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <div data-tauri-drag-region class="pin-drag-region">
        {imagePath() && (
          <img
            src={convertFileSrc(imagePath()!)}
            alt="pinned"
            class="pin-image"
            draggable={false}
          />
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
