import { createResource, createSignal, For, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Copy, ExternalLink, Trash2, X } from "lucide-solid";
import { Titlebar } from "./components/Titlebar";
import { api } from "./api";
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
  num: string;
  label: string;
  context: string;
};

const TABS: Tab[] = [
  { id: "settings", num: "i", label: "settings", context: "settings" },
  { id: "tasks", num: "ii", label: "tasks", context: "tasks" },
  { id: "history", num: "iii", label: "history", context: "history" },
  { id: "destinations", num: "iv", label: "destinations", context: "upload" },
  { id: "marketplace", num: "v", label: "plugins", context: "plugins" },
];

export function App() {
  const label = getCurrentWindow().label;
  if (label === "editor") {
    return <Editor />;
  }
  return <Hub />;
}

function Hub() {
  const [tab, setTab] = createSignal<Tab>(TABS[0]);
  const [captures] = createResource(api.listCaptures);
  const [toasts, setToasts] = createSignal<Toast[]>([]);
  const [uploads, setUploads] = createSignal<UploadCard[]>([]);
  const [recording, setRecording] = createSignal(false);
  const [config] = createResource(api.getConfig);

  const win = getCurrentWindow();
  const active = () => tab().id;

  let nextId = 1;
  const unlisteners: UnlistenFn[] = [];

  const pushToast = (kind: string, msg: string) => {
    const id = nextId++;
    setToasts((cur) => [...cur, { id, kind, msg }]);
    setTimeout(() => {
      setToasts((cur) => cur.filter((t) => t.id !== id));
    }, 6000);
  };

  const pushUpload = (url: string, deleteUrl: string | null) => {
    const id = nextId++;
    setUploads((cur) => [...cur, { id, url, deleteUrl }]);
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
      await listen("capscr://recording-started", () => setRecording(true)),
      await listen("capscr://recording-stopped", () => setRecording(false)),
    );
  });

  onCleanup(() => unlisteners.forEach((u) => u()));

  const onClose = () => {
    const c = config();
    if (c && c.ui.minimize_to_tray) {
      void win.hide();
    } else {
      void win.close();
    }
  };

  return (
    <div class="app">
      <Titlebar context={tab().context} onClose={onClose} />

      <aside class="sidebar">
        <div class="sidebar-label">nav</div>
        <nav class="sidebar-nav">
          <For each={TABS}>
            {(t) => (
              <button
                type="button"
                class="nav-item"
                classList={{ "is-active": active() === t.id }}
                onClick={() => setTab(t)}
              >
                <span class="nav-item-num">{t.num}</span>
                <span>{t.label}</span>
              </button>
            )}
          </For>
        </nav>
        <div class="sidebar-foot">
          <span class="path">~/.capscr</span>
          <span>v0.3.6 / master</span>
        </div>
      </aside>

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
          {recording() ? "● recording" : "ready"}
        </span>
        <span class="seg">
          {captures.loading
            ? "..."
            : `${captures()?.length ?? 0} captures on disk`}
        </span>
        <span class="grow" />
        <span class="tail">capscr v0.3.6</span>
      </footer>

      <Show when={toasts().length > 0 || uploads().length > 0}>
        <div class="toasts">
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
              <div class="toast" data-kind={t.kind}>
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
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
