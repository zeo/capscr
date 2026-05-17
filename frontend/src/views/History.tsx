import { createResource, For, Show } from "solid-js";
import { Copy, RefreshCw, ExternalLink, Trash2 } from "lucide-solid";
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

  return (
    <>
      <div class="view-head">
        <span class="num">iii</span>
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
          <div class="empty">
            <span class="stick" />
            no captures
            <p>take a screenshot or fire a task.</p>
          </div>
        }
      >
        <div class="tiles">
          <For each={entries()}>
            {(e) => (
              <div class="tile">
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
                    title="delete"
                    onClick={() => api.deleteCapture(e.path).then(refetch)}
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
