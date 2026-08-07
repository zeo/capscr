import { onCleanup, onMount, createSignal, Show, type JSX } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, Copy as Restore, X } from "lucide-solid";

interface Props {
  context?: string;
  // working controls rendered after the mark (hub quick-capture keys)
  quick?: JSX.Element;
  onClose?: () => void;
}

const resizeHandles = [
  ["North", "n"],
  ["NorthEast", "ne"],
  ["East", "e"],
  ["SouthEast", "se"],
  ["South", "s"],
  ["SouthWest", "sw"],
  ["West", "w"],
  ["NorthWest", "nw"],
] as const;

export function Titlebar(props: Props) {
  const win = getCurrentWindow();
  const [maximized, setMaximized] = createSignal(false);

  let unlisten: (() => void) | null = null;

  onMount(async () => {
    setMaximized(await win.isMaximized());
    unlisten = await win.onResized(async () => {
      setMaximized(await win.isMaximized());
    });
  });

  onCleanup(() => unlisten?.());

  const onDoubleClick = async (e: MouseEvent) => {
    if ((e.target as HTMLElement).closest(".titlebar-btn")) return;
    await win.toggleMaximize();
    setMaximized(await win.isMaximized());
  };

  const onClose = () => {
    if (props.onClose) {
      props.onClose();
    } else {
      void win.close();
    }
  };

  return (
    <>
      <header class="titlebar">
        <div
          class="titlebar-drag"
          data-tauri-drag-region
          onDblClick={onDoubleClick}
        >
          <span class="titlebar-mark">capscr</span>
          <Show when={props.context}>
            <span class="titlebar-context">{props.context}</span>
          </Show>
          {props.quick}
        </div>
        <div class="titlebar-buttons">
          <button
            type="button"
            class="titlebar-btn"
            data-action="minimize"
            aria-label="minimize"
            onClick={() => win.minimize()}
          >
            <Minus size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="titlebar-btn"
            data-action="maximize"
            aria-label={maximized() ? "restore" : "maximize"}
            onClick={async () => {
              await win.toggleMaximize();
              setMaximized(await win.isMaximized());
            }}
          >
            {maximized() ? (
              <Restore size={12} stroke-width={1.5} />
            ) : (
              <Square size={12} stroke-width={1.5} />
            )}
          </button>
          <button
            type="button"
            class="titlebar-btn"
            data-action="close"
            aria-label="close"
            onClick={onClose}
          >
            <X size={14} stroke-width={1.5} />
          </button>
        </div>
      </header>
      {resizeHandles.map(([direction, edge]) => (
        <div
          class={`resize-handle resize-handle-${edge}`}
          classList={{ "is-disabled": maximized() }}
          onMouseDown={(event) => {
            if (event.button !== 0 || maximized()) return;
            event.preventDefault();
            void win.startResizeDragging(direction);
          }}
        />
      ))}
    </>
  );
}
