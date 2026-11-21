import { onCleanup, onMount, createSignal, Show, type JSX } from "solid-js";
import { appWindow as win } from "../host";
import { Minus, Square, Copy as Restore, X } from "lucide-solid";

interface Props {
  context?: string;
  // working controls rendered after the mark (hub quick-capture keys)
  quick?: JSX.Element;
  onClose?: () => void;
}

export function Titlebar(props: Props) {
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
    const target = e.target;
    if (
      target instanceof Element &&
      target.closest(
        "button, a, input, select, textarea, summary, label, [role='button'], [contenteditable]:not([contenteditable='false']), [tabindex]:not([tabindex='-1'])",
      )
    ) {
      return;
    }
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
    <header class="titlebar">
      <div class="titlebar-drag" onDblClick={onDoubleClick}>
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
  );
}
