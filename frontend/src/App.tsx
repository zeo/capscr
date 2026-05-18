import { createResource, createSignal, For, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Copy, ExternalLink, Trash2, X, Download } from "lucide-solid";
import { Titlebar } from "./components/Titlebar";
import { api, UpdateInfo } from "./api";
import { configDirty, setConfigDirty } from "./dirty";
import { Settings } from "./views/Settings";
import { History } from "./views/History";
import { Destinations } from "./views/Destinations";
import { Marketplace } from "./views/Marketplace";
import { Tasks } from "./views/Tasks";
import { Editor } from "./views/Editor";

interface Toast {
  id: number;
  kind: string;
  msg: string;
}

interface UploadCard {
  id: number;
  url: string;
  deleteUrl: string | null;
}

type Tab = {
  id: "settings" | "tasks" | "history" | "destinations" | "marketplace";
  key: string; // single-char keyboard mnemonic shown in brackets, also Alt-N shortcut
  label: string;
  context: string;
};

const TABS: Tab[] = [
  { id: "settings", key: "s", label: "settings", context: "settings" },
  { id: "tasks", key: "t", label: "tasks", context: "tasks" },
  { id: "history", key: "h", label: "history", context: "history" },
  { id: "destinations", key: "d", label: "destinations", context: "upload" },
  { id: "marketplace", key: "m", label: "plugins", context: "plugins" },
];

export function App() {
  const label = getCurrentWindow().label;
  if (label === "editor") {
    return <Editor />;
  }
  return <Hub />;
}

function Hub() {
  // Open to history by default — that's the ShareX-style "what just happened"
  // view; settings is buried behind a tab click.
  const historyTab = TABS.find((t) => t.id === "history") ?? TABS[0];
  const [tab, setTab] = createSignal<Tab>(historyTab);
  const [captures] = createResource(api.listCaptures);
  const [toasts, setToasts] = createSignal<Toast[]>([]);
  const [uploads, setUploads] = createSignal<UploadCard[]>([]);
  const [recording, setRecording] = createSignal(false);
  const [recordingSince, setRecordingSince] = createSignal<number | null>(null);
  const [recordingElapsed, setRecordingElapsed] = createSignal("00:00");
  const [config] = createResource(api.getConfig);
  const [dragOver, setDragOver] = createSignal(false);
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [updateDismissed, setUpdateDismissed] = createSignal(false);
  const [updating, setUpdating] = createSignal(false);

  const win = getCurrentWindow();
  const active = () => tab().id;

  let nextId = 1;
  const unlisteners: UnlistenFn[] = [];

  // Cap so error storms (network down, upload retry loops, etc.) can't pile
  // up unbounded DOM nodes — anything older than the cap is silently dropped.
  const MAX_TOASTS = 8;
  const MAX_UPLOADS = 6;

  const pushToast = (kind: string, msg: string) => {
    const id = nextId++;
    setToasts((cur) => {
      const next = [...cur, { id, kind, msg }];
      return next.length > MAX_TOASTS ? next.slice(-MAX_TOASTS) : next;
    });
    setTimeout(() => {
      setToasts((cur) => cur.filter((t) => t.id !== id));
    }, 6000);
  };

  const pushUpload = (url: string, deleteUrl: string | null) => {
    const id = nextId++;
    setUploads((cur) => {
      const next = [...cur, { id, url, deleteUrl }];
      return next.length > MAX_UPLOADS ? next.slice(-MAX_UPLOADS) : next;
    });
  };

  onMount(async () => {
    unlisteners.push(
      await listen<{ kind: string; msg: string }>(
        "capscr://error",
        (e) => pushToast(e.payload.kind, e.payload.msg),
      ),
      await listen<{ url: string; delete_url: string | null }>(
        "capscr://upload-success",
        (e) => pushUpload(e.payload.url, e.payload.delete_url),
      ),
      await listen<string>("capscr://recording-started", (e) => {
        setRecording(true);
        setRecordingSince(Date.now());
        setRecordingElapsed("00:00");
        // Tell the user how to stop — re-pressing the task's hotkey toggles
        // the recording off, but that's not obvious. Look up the hotkey from
        // config so the toast is concrete.
        const taskId = e.payload;
        const task = config()?.capture_tasks.find((t) => t.id === taskId);
        const hint = task?.hotkey
          ? `recording — press ${task.hotkey} again to stop`
          : `recording — press the same hotkey again to stop`;
        pushToast("recording", hint);
      }),
      await listen("capscr://recording-stopped", () => {
        setRecording(false);
        setRecordingSince(null);
      }),
    );

    // Background update check — delayed 4s so it doesn't compete with hub
    // first-paint or block the network during the user's first capture.
    // Skip entirely if the user has opted out in Settings.
    setTimeout(() => {
      if (config()?.ui.check_updates_on_launch === false) return;
      void api
        .checkForUpdates()
        .then((info) => {
          if (info) setUpdateInfo(info);
        })
        .catch(() => {
          // updater endpoint unreachable / GitHub release missing — silent.
        });
    }, 4000);

    // While recording, refresh the elapsed counter once per second so the
    // statusbar shows mm:ss live. Cheap: a single setInterval that no-ops
    // when not recording.
    const tickHandle = setInterval(() => {
      const since = recordingSince();
      if (since === null) return;
      const ms = Date.now() - since;
      const totalSec = Math.floor(ms / 1000);
      const mm = String(Math.floor(totalSec / 60)).padStart(2, "0");
      const ss = String(totalSec % 60).padStart(2, "0");
      setRecordingElapsed(`${mm}:${ss}`);
    }, 1000);
    unlisteners.push(() => clearInterval(tickHandle));

    // Alt+S/T/H/D/M for tab switching — sidebar titles advertise these so
    // the keybind has to actually work. We respect the dirty-state guard so
    // Alt-jumping out of unsaved edits still prompts.
    const onKey = (ev: KeyboardEvent) => {
      if (!ev.altKey || ev.ctrlKey || ev.metaKey || ev.shiftKey) return;
      const k = ev.key.toLowerCase();
      const target = TABS.find((t) => t.key === k);
      if (!target) return;
      ev.preventDefault();
      tryChangeTab(target);
    };
    window.addEventListener("keydown", onKey);
    unlisteners.push(() => window.removeEventListener("keydown", onKey));

    const dragUnlisten = await win.onDragDropEvent(async (e) => {
      const payload = e.payload;
      if (payload.type === "enter" || payload.type === "over") {
        setDragOver(true);
      } else if (payload.type === "leave") {
        setDragOver(false);
      } else if (payload.type === "drop") {
        setDragOver(false);
        // Cap concurrent uploads so dragging 50 files onto the window doesn't
        // melt the UI thread. Anything over the cap is rejected with a single
        // explanatory toast — the user can re-drop the remainder.
        const MAX_BATCH = 5;
        const paths = payload.paths;
        const accepted = paths.slice(0, MAX_BATCH);
        const overflow = paths.length - accepted.length;
        if (overflow > 0) {
          pushToast(
            "upload",
            `dropped ${paths.length} files — uploading first ${MAX_BATCH}, drop the rest after these finish`,
          );
        }
        for (const path of accepted) {
          try {
            await api.uploadFile(path);
          } catch (err) {
            pushToast("upload", String(err));
          }
        }
      }
    });
    unlisteners.push(dragUnlisten);
  });

  onCleanup(() => unlisteners.forEach((u) => u()));

  const confirmDiscardEdits = (): boolean =>
    !configDirty() || window.confirm("Discard unsaved settings changes?");

  const tryChangeTab = (next: Tab) => {
    if (tab().id === next.id) return;
    if (!confirmDiscardEdits()) return;
    setConfigDirty(false);
    setTab(next);
  };

  const onClose = () => {
    if (!confirmDiscardEdits()) return;
    setConfigDirty(false);
    const c = config();
    // Default to hide-to-tray when config hasn't loaded yet — destroying the
    // window on early X-clicks just forces a slow re-create on the next tray
    // click and loses webview state.
    if (!c || c.ui.minimize_to_tray) {
      void win.hide();
    } else {
      void win.close();
    }
  };

  const runUpdate = async () => {
    setUpdating(true);
    try {
      await api.installUpdate();
      // installUpdate triggers app.restart() on success, so we shouldn't
      // get here. If we do, treat it as the update having installed without
      // a restart (rare).
    } catch (e) {
      pushToast("update", String(e));
      setUpdating(false);
    }
  };

  return (
    <div class="app">
      <Titlebar context={tab().context} onClose={onClose} />

      <aside class="sidebar">
        <div class="sidebar-label">
          <span class="sidebar-label-mark">▮</span> capscr/console
        </div>
        <nav class="sidebar-nav">
          <For each={TABS}>
            {(t) => (
              <button
                type="button"
                class="nav-item"
                classList={{ "is-active": active() === t.id }}
                onClick={() => tryChangeTab(t)}
                title={`Alt+${t.key.toUpperCase()}`}
              >
                <span class="nav-item-key">
                  <span class="nav-item-bracket">[</span>
                  {t.key}
                  <span class="nav-item-bracket">]</span>
                </span>
                <span class="nav-item-label">{t.label}</span>
              </button>
            )}
          </For>
        </nav>
        <div class="sidebar-foot">
          <span class="path">~/.capscr</span>
          <span class="build">v{__APP_VERSION__}·rel</span>
        </div>
      </aside>

      <Show when={updateInfo() && !updateDismissed()}>
        <div class="update-banner">
          <span class="update-banner-glyph">▮</span>
          <div class="update-banner-text">
            <span class="update-banner-title">
              update available · v{updateInfo()!.version}
            </span>
            <span class="update-banner-meta">
              you're on v{updateInfo()!.current_version}
            </span>
          </div>
          <button
            type="button"
            class="btn"
            data-size="xs"
            onClick={runUpdate}
            disabled={updating()}
          >
            <Download size={11} stroke-width={1.5} />
            {updating() ? "installing..." : "install + restart"}
          </button>
          <button
            type="button"
            class="btn"
            data-variant="ghost"
            data-size="xs"
            onClick={() => setUpdateDismissed(true)}
          >
            later
          </button>
        </div>
      </Show>

      <main class="content">
        <Switch>
          <Match when={active() === "settings"}>
            <Settings />
          </Match>
          <Match when={active() === "tasks"}>
            <Tasks />
          </Match>
          <Match when={active() === "history"}>
            <History />
          </Match>
          <Match when={active() === "destinations"}>
            <Destinations />
          </Match>
          <Match when={active() === "marketplace"}>
            <Marketplace />
          </Match>
        </Switch>
      </main>

      <footer class="statusbar">
        <span class="seg" classList={{ "is-ok": !recording(), "is-rec": recording() }}>
          <span class="seg-k">stat</span>
          <span class="seg-v">
            {recording() ? `rec ${recordingElapsed()}` : "rdy"}
          </span>
        </span>
        <span class="seg-sep">│</span>
        <span class="seg">
          <span class="seg-k">cap</span>
          <span class="seg-v">{captures.loading ? "·" : (captures()?.length ?? 0).toString().padStart(3, "0")}</span>
        </span>
        <span class="seg-sep">│</span>
        <span class="seg">
          <span class="seg-k">tab</span>
          <span class="seg-v">{tab().id}</span>
        </span>
        <Show when={configDirty()}>
          <span class="seg-sep">│</span>
          <span class="seg is-dirty">
            <span class="seg-k">edit</span>
            <span class="seg-v">unsaved</span>
          </span>
        </Show>
        <span class="grow" />
        <span class="seg tail">
          <span class="seg-k">capscr</span>
          <span class="seg-v">v{__APP_VERSION__}</span>
        </span>
      </footer>

      <Show when={dragOver()}>
        <div class="drop-overlay">
          <div class="drop-overlay-inner">
            <div class="drop-overlay-glyph">+</div>
            <div class="drop-overlay-title">drop to upload</div>
            <div class="drop-overlay-lede">
              destination: {config()?.upload.destination ?? "..."}
            </div>
          </div>
        </div>
      </Show>

      <Show when={toasts().length > 0 || uploads().length > 0}>
        <div class="toasts" role="region" aria-label="notifications">
          <For each={uploads()}>
            {(u) => (
              <div class="toast upload-card">
                <div class="upload-card-head">
                  <span class="toast-kind">uploaded</span>
                  <button
                    type="button"
                    class="toast-close"
                    aria-label="dismiss"
                    onClick={() =>
                      setUploads((cur) => cur.filter((x) => x.id !== u.id))
                    }
                  >
                    <X size={11} stroke-width={1.5} />
                  </button>
                </div>
                <div class="upload-card-url" title={u.url}>{u.url}</div>
                <div class="upload-card-actions">
                  <button
                    class="btn"
                    data-size="xs"
                    onClick={() => void writeText(u.url)}
                  >
                    <Copy size={11} stroke-width={1.5} />
                    copy
                  </button>
                  <button
                    class="btn"
                    data-variant="ghost"
                    data-size="xs"
                    onClick={() => void openUrl(u.url)}
                  >
                    <ExternalLink size={11} stroke-width={1.5} />
                    open
                  </button>
                  <Show when={u.deleteUrl}>
                    <button
                      class="btn"
                      data-variant="ghost"
                      data-size="xs"
                      title={u.deleteUrl!}
                      onClick={() => void writeText(u.deleteUrl!)}
                    >
                      <Trash2 size={11} stroke-width={1.5} />
                      copy del url
                    </button>
                  </Show>
                </div>
              </div>
            )}
          </For>
          <For each={toasts()}>
            {(t) => (
              <output
                class="toast"
                data-kind={t.kind}
                role="status"
                aria-live="polite"
                aria-atomic="true"
              >
                <span class="toast-kind">{t.kind}</span>
                <span class="toast-msg">{t.msg}</span>
                <button
                  type="button"
                  class="toast-close"
                  aria-label="dismiss"
                  onClick={() =>
                    setToasts((cur) => cur.filter((x) => x.id !== t.id))
                  }
                >
                  ×
                </button>
              </output>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
