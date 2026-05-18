import { createResource, createSignal, For, Show } from "solid-js";
import {
  Copy,
  RefreshCw,
  ExternalLink,
  Trash2,
  Edit3,
  UploadCloud,
} from "lucide-solid";
import { api } from "../api";

function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(2)} MB`;
}

function formatDate(unix: number): string {
  return new Date(unix * 1000).toLocaleString(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function History() {
  const [entries, { refetch }] = createResource(api.listCaptures);
  // Track which path is in the "confirm delete" state. Second click on the
  // trash icon within 4s commits; otherwise the prompt resets.
  const [confirmDelete, setConfirmDelete] = createSignal<string | null>(null);

  const armDelete = (path: string) => {
    setConfirmDelete(path);
    setTimeout(() => {
      if (confirmDelete() === path) setConfirmDelete(null);
    }, 4000);
  };

  const doDelete = (path: string) => {
    api.deleteCapture(path).then(() => {
      setConfirmDelete(null);
      refetch();
    });
  };

  return (
    <>
      <div class="view-head">
        <h1>history</h1>
        <span class="lede">
          {entries.loading
            ? "reading dir..."
            : `${entries()?.length ?? 0} files in output dir`}
        </span>
      </div>

      <div class="row between" style="margin-bottom: 18px;">
        <button class="btn" data-variant="ghost" onClick={() => refetch()}>
          <RefreshCw size={12} stroke-width={1.5} />
          reload
        </button>
      </div>

      <Show
        when={entries() && entries()!.length > 0}
        fallback={
          <Show
            when={!entries.loading}
            fallback={
              <div class="empty">
                <span class="stick" />
                reading...
              </div>
            }
          >
            <div class="empty">
              <span class="stick" />
              no captures yet
              <p>
                press <kbd>Numpad 5</kbd> to drag a region capture to clipboard,
                or <kbd>Pause</kbd> to record a GIF.
              </p>
              <p class="muted" style="margin-top: 8px; font-size: 11px;">
                rebind these in <strong>tasks</strong> · destinations live in <strong>destinations</strong>.
              </p>
            </div>
          </Show>
        }
      >
        <div class="tiles">
          <For each={entries()}>
            {(e) => (
              <div
                class="tile"
                onClick={(ev) => {
                  // Don't open the editor when the click landed on an
                  // overlay button.
                  if ((ev.target as HTMLElement).closest(".tile-actions")) return;
                  void api.openEditor(e.path);
                }}
              >
                <img
                  class="tile-img"
                  src={`asset://localhost/${encodeURIComponent(e.path)}`}
                  alt={e.filename}
                  onError={(ev) => {
                    (ev.currentTarget as HTMLImageElement).style.opacity =
                      "0.3";
                  }}
                />
                <div class="tile-actions">
                  <button
                    class="icon-btn"
                    title="edit"
                    onClick={() => api.openEditor(e.path)}
                  >
                    <Edit3 size={12} stroke-width={1.5} />
                  </button>
                  <button
                    class="icon-btn"
                    title="re-upload"
                    onClick={() => api.reuploadCapture(e.path)}
                  >
                    <UploadCloud size={12} stroke-width={1.5} />
                  </button>
                  <button
                    class="icon-btn"
                    title="open in os viewer"
                    onClick={() => api.openInExplorer(e.path)}
                  >
                    <ExternalLink size={12} stroke-width={1.5} />
                  </button>
                  <button
                    class="icon-btn"
                    title="copy to clipboard"
                    onClick={() => api.copyCaptureToClipboard(e.path)}
                  >
                    <Copy size={12} stroke-width={1.5} />
                  </button>
                  <button
                    class="icon-btn"
                    classList={{ "is-arm": confirmDelete() === e.path }}
                    title={
                      confirmDelete() === e.path
                        ? "click again to confirm"
                        : "delete"
                    }
                    onClick={() =>
                      confirmDelete() === e.path
                        ? doDelete(e.path)
                        : armDelete(e.path)
                    }
                  >
                    <Trash2 size={12} stroke-width={1.5} />
                  </button>
                </div>
                <div class="tile-meta">
                  <div class="name" title={e.path}>
                    {e.filename}
                  </div>
                  <div class="stats">
                    <span>{formatBytes(e.size_bytes)}</span>
                    <span>·</span>
                    <span>{formatDate(e.modified_unix)}</span>
                    <Show when={e.has_hdr}>
                      <span>·</span>
                      <span
                        class="tile-tag"
                        title="HDR sidecar present (.hdr.png)"
                      >
                        HDR
                      </span>
                    </Show>
                  </div>
                </div>
              </div>
            )}
          </For>
        </div>
      </Show>
    </>
  );
}
