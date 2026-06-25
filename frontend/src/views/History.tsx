import {
  createMemo,
  createResource,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from "solid-js";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  Copy,
  RefreshCw,
  ExternalLink,
  Trash2,
  Edit3,
  UploadCloud,
  Search,
  Scissors,
  X,
  Type,
  Pin,
} from "lucide-solid";
import { api } from "../api";
import { TrimModal } from "../components/TrimModal";

type FilterKind = "all" | "images" | "gifs" | "videos" | "hdr";

function formatBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(2)} MB`;
}

function formatDate(unix: number): string {
  return new Date(unix).toLocaleString(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function History() {
  const [entries, { refetch }] = createResource(api.listCaptures);
  const [outputDir, setOutputDir] = createSignal<string>("");
  const [screenshotHotkey, setScreenshotHotkey] = createSignal<string>("PrintScreen");
  const [recordGifHotkey, setRecordGifHotkey] = createSignal<string>("Ctrl+Shift+G");
  onMount(() => {
    api.getConfig().then((c) => {
      setOutputDir(c.output.directory);
      if (c.hotkeys) {
        if (c.hotkeys.screenshot) setScreenshotHotkey(c.hotkeys.screenshot);
        if (c.hotkeys.record_gif) setRecordGifHotkey(c.hotkeys.record_gif);
      }
    }).catch(() => {});
  });
  // track which path is in the "confirm delete" state. Second click on the
  // trash icon within 4s commits; otherwise the prompt resets.
  const [confirmDelete, setConfirmDelete] = createSignal<string | null>(null);
  const [search, setSearch] = createSignal("");
  const [filter, setFilter] = createSignal<FilterKind>("all");
  // path of the mp4 currently open in the trim modal, or null
  const [trimPath, setTrimPath] = createSignal<string | null>(null);

  // live-refresh the grid when a new capture lands so the user doesn't
  // have to click "reload" after every screenshot. Coalesce rapid bursts
  // (e.g. a GIF + sidecar landing back-to-back) into one refetch.
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let unlisten: UnlistenFn | null = null;
  onMount(async () => {
    unlisten = await listen("capscr://capture-saved", () => {
      if (refreshTimer) clearTimeout(refreshTimer);
      refreshTimer = setTimeout(() => {
        refetch();
        refreshTimer = null;
      }, 250);
    });
  });
  onCleanup(() => {
    if (refreshTimer) clearTimeout(refreshTimer);
    unlisten?.();
  });

  const filtered = createMemo(() => {
    const list = entries() ?? [];
    const needle = search().trim().toLowerCase();
    const kind = filter();
    return list.filter((e) => {
      if (kind === "gifs" && !e.is_gif) return false;
      if (kind === "videos" && !e.is_mp4) return false;
      if (kind === "images" && (e.is_gif || e.is_mp4)) return false;
      if (kind === "hdr" && !e.has_hdr) return false;
      if (needle && !e.filename.toLowerCase().includes(needle)) return false;
      return true;
    });
  });

  let armTimer: ReturnType<typeof setTimeout> | undefined;
  let errorTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => { clearTimeout(armTimer); clearTimeout(errorTimer); });

  const armDelete = (path: string) => {
    clearTimeout(armTimer);
    setConfirmDelete(path);
    armTimer = setTimeout(() => {
      if (confirmDelete() === path) setConfirmDelete(null);
    }, 4000);
  };

  // single transient banner for tile actions: copy / reveal / re-upload all
  // succeed without changing the grid, so without this they look dead even
  // when they worked. errors surface here too instead of silently no-op'ing
  const [flash, setFlash] = createSignal<{ tone: "ok" | "err"; msg: string } | null>(null);
  let flashTimer: ReturnType<typeof setTimeout>;
  onCleanup(() => clearTimeout(flashTimer));
  const showFlash = (tone: "ok" | "err", msg: string) => {
    setFlash({ tone, msg });
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => setFlash(null), tone === "err" ? 6000 : 2500);
  };
  const doReupload = (path: string) => {
    showFlash("ok", "uploading...");
    api.reuploadCapture(path)
      .then(() => showFlash("ok", "re-uploaded"))
      .catch((e: unknown) => showFlash("err", `upload failed: ${e}`));
  };
  const doCopy = (path: string) => {
    api.copyCaptureToClipboard(path)
      .then(() => showFlash("ok", "copied to clipboard"))
      .catch((e: unknown) => showFlash("err", `copy failed: ${e}`));
  };
  const doReveal = (path: string) => {
    api.openInExplorer(path)
      .then(() => showFlash("ok", "revealed in file explorer"))
      .catch((e: unknown) => showFlash("err", `open failed: ${e}`));
  };
  const doEdit = (path: string) => {
    api.openEditor(path).catch((e: unknown) => showFlash("err", `editor failed: ${e}`));
  };
  const doOcr = (path: string) => {
    showFlash("ok", "extracting text...");
    api.runOcr(path)
      .then((text: string) => {
        if (!text || text.trim() === "") {
          showFlash("err", "no text found in image");
        } else {
          navigator.clipboard.writeText(text)
            .then(() => showFlash("ok", `OCR: text copied (${text.length} chars)`))
            .catch(() => showFlash("err", "failed to copy to clipboard"));
        }
      })
      .catch((e: unknown) => showFlash("err", `OCR failed: ${e}`));
  };
  const doPin = (path: string) => {
    api.pinImage(path)
      .catch((e: unknown) => showFlash("err", `pin failed: ${e}`));
  };
  const doDelete = (path: string) => {
    api.deleteCapture(path).then(() => {
      setConfirmDelete(null);
      refetch();
      showFlash("ok", "deleted");
    }).catch((e: unknown) => {
      setConfirmDelete(null);
      showFlash("err", `delete failed: ${e}`);
    });
  };

  return (
    <>
      <div class="view-head">
        <h1>history</h1>
        <span class="lede">
          {entries.loading
            ? "reading dir..."
            : (() => {
                const total = entries()?.length ?? 0;
                const shown = filtered().length;
                return shown === total
                  ? `${total} files in output dir`
                  : `${shown} of ${total} files match`;
              })()}
        </span>
      </div>

      <div class="row between" style="margin-bottom: 18px; gap: 10px;">
        <div class="history-controls">
          <label class="history-search">
            <Search size={11} stroke-width={1.5} />
            <input
              type="text"
              placeholder="filter by filename..."
              value={search()}
              onInput={(e) => setSearch(e.currentTarget.value)}
            />
            <Show when={search()}>
              <button
                type="button"
                class="search-clear"
                title="clear"
                onClick={() => setSearch("")}
              >
                <X size={10} stroke-width={1.5} />
              </button>
            </Show>
          </label>
          <div class="history-filters">
            <For each={["all", "images", "gifs", "videos", "hdr"] as const}>
              {(k) => (
                <button
                  type="button"
                  class="filter-pill"
                  classList={{ "is-active": filter() === k }}
                  onClick={() => setFilter(k)}
                >
                  {k}
                </button>
              )}
            </For>
          </div>
        </div>
        <button class="btn" data-variant="ghost" onClick={() => refetch()}>
          <RefreshCw size={12} stroke-width={1.5} />
          reload
        </button>
      </div>

      <Show when={flash()}>
        <div class="flash" data-tone={flash()!.tone} style="margin-bottom: 12px;">
          {flash()!.msg}
        </div>
      </Show>

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
                press <kbd>{screenshotHotkey()}</kbd> to drag a region capture to clipboard,
                or <kbd>{recordGifHotkey()}</kbd> to record a GIF.
              </p>
              <p class="muted" style="margin-top: 8px; font-size: 11px;">
                rebind these in <strong>tasks</strong> · destinations live in <strong>destinations</strong>.
              </p>
              <Show when={outputDir()}>
                <p class="muted" style="margin-top: 12px; font-size: 10px;">
                  reading from <code>{outputDir()}</code>
                </p>
              </Show>
            </div>
          </Show>
        }
      >
        <Show
          when={filtered().length > 0}
          fallback={
            <div class="empty">
              <span class="stick" />
              no matches
              <p>nothing in the output dir matches your filter.</p>
            </div>
          }
        >
        <div class="tiles">
          <For each={filtered()}>
            {(e) => (
              <div
                class="tile"
                onClick={(ev) => {
                  // don't open the editor when the click landed on an
                  // overlay button.
                  if ((ev.target as HTMLElement).closest(".tile-actions")) return;
                  // recordings can't be edited — clicking them reveals the
                  // file instead of opening the editor
                  if (e.is_gif || e.is_mp4) {
                    void api.openInExplorer(e.path);
                    return;
                  }
                  void api.openEditor(e.path);
                }}
              >
                <Show
                  when={e.is_mp4}
                  fallback={
                    <img
                      class="tile-img"
                      src={convertFileSrc(e.path)}
                      alt={e.filename}
                      loading="lazy"
                      decoding="async"
                      onError={(ev) => {
                        (ev.currentTarget as HTMLImageElement).style.opacity =
                          "0.3";
                      }}
                    />
                  }
                >
                  <video
                    class="tile-img"
                    src={convertFileSrc(e.path)}
                    autoplay
                    muted
                    loop
                    playsinline
                    style={{ "object-fit": "cover" }}
                  />
                </Show>
                <div class="tile-actions">
                  <Show when={e.is_mp4}>
                    <button
                      class="icon-btn"
                      title="trim"
                      onClick={() => setTrimPath(e.path)}
                    >
                      <Scissors size={12} stroke-width={1.5} />
                    </button>
                  </Show>
                   <Show when={!e.is_gif && !e.is_mp4}>
                    <button
                      class="icon-btn"
                      title="edit"
                      onClick={() => doEdit(e.path)}
                    >
                      <Edit3 size={12} stroke-width={1.5} />
                    </button>
                    <button
                      class="icon-btn"
                      title="extract text (OCR)"
                      onClick={() => doOcr(e.path)}
                    >
                      <Type size={12} stroke-width={1.5} />
                    </button>
                    <button
                      class="icon-btn"
                      title="pin to screen"
                      onClick={() => doPin(e.path)}
                    >
                      <Pin size={12} stroke-width={1.5} />
                    </button>
                  </Show>
                  <button
                    class="icon-btn"
                    title="re-upload"
                    onClick={() => doReupload(e.path)}
                  >
                    <UploadCloud size={12} stroke-width={1.5} />
                  </button>
                  <button
                    class="icon-btn"
                    title="open in os viewer"
                    onClick={() => doReveal(e.path)}
                  >
                    <ExternalLink size={12} stroke-width={1.5} />
                  </button>
                  <button
                    class="icon-btn"
                    title="copy to clipboard"
                    onClick={() => doCopy(e.path)}
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
      </Show>

      <Show when={trimPath()}>
        <TrimModal
          path={trimPath()!}
          onClose={() => setTrimPath(null)}
          onDone={(msg) => {
            setTrimPath(null);
            showFlash("ok", msg);
            refetch();
          }}
        />
      </Show>
    </>
  );
}
