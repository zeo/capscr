import { createResource, For, Show } from "solid-js";
import { api } from "../api";

function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(2)} MB`;
}

function formatDate(unix: number): string {
  return new Date(unix * 1000).toLocaleString();
}

export function History() {
  const [entries, { refetch }] = createResource(api.listCaptures);

  return (
    <>
      <h1>History</h1>
      <div class="row" style="margin-bottom: 12px;">
        <button class="ghost" onClick={() => refetch()}>
          Refresh
        </button>
      </div>
      <Show
        when={entries() && entries()!.length > 0}
        fallback={<p style="color: var(--fg-dim)">No captures yet.</p>}
      >
        <div class="grid">
          <For each={entries()}>
            {(e) => (
              <div class="card">
                <img
                  src={`asset://localhost/${encodeURIComponent(e.path)}`}
                  alt={e.filename}
                  onError={(ev) => {
                    (ev.currentTarget as HTMLImageElement).style.opacity =
                      "0.3";
                  }}
                />
                <div class="meta">
                  <div title={e.path}>{e.filename}</div>
                  <div>
                    {formatBytes(e.size_bytes)} ·{" "}
                    {formatDate(e.modified_unix)}
                  </div>
                  <div class="row" style="margin-top: 6px; gap: 4px;">
                    <button
                      class="ghost"
                      onClick={() => api.openInExplorer(e.path)}
                    >
                      Open
                    </button>
                    <button
                      class="ghost"
                      onClick={() => api.copyCaptureToClipboard(e.path)}
                    >
                      Copy
                    </button>
                    <button
                      class="ghost"
                      onClick={() => api.deleteCapture(e.path).then(refetch)}
                    >
                      Delete
                    </button>
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
