import { createResource, createSignal, For, Match, onCleanup, onMount, Show, Switch } from "solid-js";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { Titlebar } from "./components/Titlebar";
import { api } from "./api";
import { Settings } from "./views/Settings";
import { History } from "./views/History";
import { Destinations } from "./views/Destinations";
import { Marketplace } from "./views/Marketplace";
import { Tasks } from "./views/Tasks";

interface Toast {
  id: number;
  kind: string;
  msg: string;
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
  const [tab, setTab] = createSignal<Tab>(TABS[0]);
  const [captures] = createResource(api.listCaptures);
  const [toasts, setToasts] = createSignal<Toast[]>([]);

  const active = () => tab().id;

  let nextId = 1;
  let unlisten: UnlistenFn | null = null;

  const pushToast = (kind: string, msg: string) => {
    const id = nextId++;
    setToasts((cur) => [...cur, { id, kind, msg }]);
    setTimeout(() => {
      setToasts((cur) => cur.filter((t) => t.id !== id));
    }, 6000);
  };

  onMount(async () => {
    unlisten = await listen<{ kind: string; msg: string }>(
      "capscr://error",
      (e) => pushToast(e.payload.kind, e.payload.msg),
    );
  });

  onCleanup(() => unlisten?.());

  return (
    <div class="app">
      <Titlebar context={tab().context} />

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
          <span>v0.3.3 / master</span>
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
        <span class="seg is-ok">ready</span>
        <span class="seg">
          {captures.loading
            ? "..."
            : `${captures()?.length ?? 0} captures on disk`}
        </span>
        <span class="grow" />
        <span class="tail">capscr v0.3.3</span>
      </footer>

      <Show when={toasts().length > 0}>
        <div class="toasts">
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
