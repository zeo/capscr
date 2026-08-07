import { createResource, createSignal, For, lazy, Match, onCleanup, onMount, Show, Suspense, Switch } from "solid-js";
import { listen, TauriEvent, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Copy, ExternalLink, Trash2, X, Download } from "lucide-solid";
import { Titlebar } from "./components/Titlebar";
import { api, type AppConfig, type CaptureMode, HotkeyDiagnostics, UpdateInfo } from "./api";
import { configDirty, setConfigDirty } from "./dirty";
import { Settings } from "./views/Settings";
import { History } from "./views/History";
import { Destinations } from "./views/Destinations";
import { Tasks } from "./views/Tasks";
import {
  captureTaskSaveState,
  config,
  discardFailedCaptureTaskSave,
  mutateConfig,
  saveCaptureTasks,
  waitForCaptureTaskSaves,
} from "./store";
import { HotkeyInput } from "./components/HotkeyInput";
import { PinView } from "./views/PinView";
import { Selector } from "./views/Selector";
import { RecBar } from "./views/RecBar";

// lazy-loaded so the hub bundle doesn't ship the full canvas editor and the
// editor window doesn't ship the hub. Editor is a named export, so adapt it to
// the default export the lazy loader expects.
const Editor = lazy(() => import("./views/Editor").then((m) => ({ default: m.Editor })));

// plugins view is code-split out of the hub's initial bundle — it isn't the
// default tab, so its registry browser + icons load only when the tab is opened
const Marketplace = lazy(() => import("./views/Marketplace").then((m) => ({ default: m.Marketplace })));

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

interface EditorExitDecision {
  requestId: string;
  allowed: boolean;
}

type Tab = {
  id: "settings" | "tasks" | "history" | "destinations" | "marketplace";
  key: string; // single-char keyboard mnemonic shown in brackets, also Alt-N shortcut
  label: string;
  context: string;
};

// release page fallback when the shared updater is unavailable
const RELEASES_URL = "https://github.com/zeo/capscr/releases/latest";

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
    return (
      <Suspense fallback={<div class="editor-loading">loading editor…</div>}>
        <Editor />
      </Suspense>
    );
  }
  if (label.startsWith("pin_")) {
    return <PinView label={label} />;
  }
  if (label.startsWith("selector-")) {
    return <Selector />;
  }
  if (label === "recbar") {
    return <RecBar />;
  }
  return <Hub />;
}

function Hub() {
  const needsOnboarding = () => {
    const c = config();
    if (!c) return false;
    const task = c.capture_tasks.find((t) => t.id === "screenshot-save-clipboard");
    return task && !task.hotkey;
  };

  // open to history by default — "what just happened" is the expected view;
  // settings is buried behind a tab click.
  const historyTab = TABS.find((t) => t.id === "history") ?? TABS[0];
  const [tab, setTab] = createSignal<Tab>(historyTab);
  const [captures, { refetch: refetchCaptures }] = createResource(api.listCaptures);
  const [toasts, setToasts] = createSignal<Toast[]>([]);
  const [uploads, setUploads] = createSignal<UploadCard[]>([]);
  const [recording, setRecording] = createSignal(false);
  const [recordingSince, setRecordingSince] = createSignal<number | null>(null);
  const [recordingElapsed, setRecordingElapsed] = createSignal("00:00");
  const [dragOver, setDragOver] = createSignal(false);
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [updateDismissed, setUpdateDismissed] = createSignal(false);
  const [trayMissing, setTrayMissing] = createSignal(false);
  const [updating, setUpdating] = createSignal(false);
  const [onboardingSaving, setOnboardingSaving] = createSignal(false);
  const [showShortcuts, setShowShortcuts] = createSignal(false);
  const [hotkeyDiag, { refetch: refetchHotkeyDiag }] = createResource<HotkeyDiagnostics>(
    api.hotkeyDiagnostics,
  );

  // the recording elapsed clock. it runs only between recording-started and
  // recording-stopped so the reused (never-closed) hub isn't waking a 1Hz timer
  // for the life of the process while it sits hidden in the tray.
  let tickHandle: ReturnType<typeof setInterval> | null = null;
  const stopTick = () => {
    if (tickHandle !== null) {
      clearInterval(tickHandle);
      tickHandle = null;
    }
  };
  const startTick = () => {
    if (tickHandle !== null) return;
    tickHandle = setInterval(() => {
      const since = recordingSince();
      if (since === null) return;
      const totalSec = Math.floor((Date.now() - since) / 1000);
      const mm = String(Math.floor(totalSec / 60)).padStart(2, "0");
      const ss = String(totalSec % 60).padStart(2, "0");
      setRecordingElapsed(`${mm}:${ss}`);
    }, 1000);
  };
  onCleanup(stopTick);

  const win = getCurrentWindow();
  const active = () => tab().id;

  let nextId = 1;
  const unlisteners: UnlistenFn[] = [];
  const toastTimers: ReturnType<typeof setTimeout>[] = [];

  // cap so error storms (network down, upload retry loops, etc.) can't pile
  // up unbounded DOM nodes — anything older than the cap is silently dropped.
  const MAX_TOASTS = 8;
  const MAX_UPLOADS = 6;

  onCleanup(() => toastTimers.forEach(clearTimeout));

  const pushToast = (kind: string, msg: string) => {
    const id = nextId++;
    setToasts((cur) => {
      const next = [...cur, { id, kind, msg }];
      return next.length > MAX_TOASTS ? next.slice(-MAX_TOASTS) : next;
    });
    toastTimers.push(setTimeout(() => {
      setToasts((cur) => cur.filter((t) => t.id !== id));
    }, 6000));
  };

  const pushUpload = (url: string, deleteUrl: string | null) => {
    const id = nextId++;
    setUploads((cur) => {
      const next = [...cur, { id, url, deleteUrl }];
      return next.length > MAX_UPLOADS ? next.slice(-MAX_UPLOADS) : next;
    });
  };

  const confirmEditorExit = async () => {
    const editor = await Window.getByLabel("editor");
    if (!editor) return true;

    const requestId = crypto.randomUUID();
    let resolveDecision!: (allowed: boolean) => void;
    const decision = new Promise<boolean>((resolve) => {
      resolveDecision = resolve;
    });
    let resolved = false;
    let timedOut = false;
    const finish = (allowed: boolean) => {
      if (resolved) return;
      resolved = true;
      resolveDecision(allowed);
    };

    const stopDecision = await win.listen<EditorExitDecision>(
      "capscr://editor-exit-decision",
      (event) => {
        if (event.payload.requestId === requestId) {
          finish(event.payload.allowed);
        }
      },
    );
    let stopDestroyed: UnlistenFn | undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      stopDestroyed = await editor.once(TauriEvent.WINDOW_DESTROYED, () => finish(true));
      if (!(await Window.getByLabel("editor"))) return true;

      timeout = setTimeout(() => {
        timedOut = true;
        finish(false);
      }, 120_000);
      await win.emitTo(
        { kind: "Window", label: "editor" },
        "capscr://editor-exit-request",
        { requestId },
      );
      const allowed = await decision;
      if (timedOut) throw new Error("editor did not respond; action cancelled");
      return allowed;
    } finally {
      if (timeout) clearTimeout(timeout);
      stopDestroyed?.();
      stopDecision();
    }
  };

  onMount(async () => {
    unlisteners.push(
      await listen<{ requestId: string; forceExit: boolean }>("capscr://close-requested", (e) => {
        void api.acknowledgeHubClose(e.payload.requestId)
          .then((acknowledged) => {
            if (acknowledged) void closeHub(e.payload.forceExit);
          })
          .catch((error) => pushToast("close", String(error)));
      }),
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
        startTick();
        // tell the user how to stop — re-pressing the task's hotkey toggles
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
        stopTick();
      }),
      // fired at startup on desktops with no system-tray host (vanilla
      // gnome); the hub is already open, this just explains why there's no
      // tray icon and how to keep reaching capscr
      await listen("capscr://tray-missing", () => setTrayMissing(true)),
      // the hub window is reused for the whole process, so this resource loads
      // once at first mount; refetch it when a capture lands so the statusbar
      // count actually tracks new screenshots and recordings
      await listen("capscr://capture-saved", () => {
        refetchCaptures();
      }),
      // tray "Open hub → <Tab>" fires this so the hub lands on the chosen tab
      await listen<string>("capscr://goto-tab", (e) => {
        const target = TABS.find((t) => t.id === e.payload);
        if (target) tryChangeTab(target);
      }),
      // tray destination switcher mutates config — refetch so any visible
      // dropdown stays in sync, but never over the top of unsaved edits: a
      // refetch replaces the whole store, so mid-edit it would silently drop
      // the user's pending changes. their next save wins instead.
      await listen("capscr://config-updated", async () => {
        if (
          configDirty() ||
          captureTaskSaveState().kind === "saving" ||
          captureTaskSaveState().kind === "error"
        ) {
          // don't clobber unsaved edits, but still pick up the one field the
          // tray can change out from under the user — the upload destination —
          // so their next save doesn't silently revert the tray switch
          try {
            const fresh = await api.getConfig();
            const cur = config();
            if (cur) {
              mutateConfig({
                ...cur,
                upload: { ...cur.upload, destination: fresh.upload.destination },
                hotkeys: {
                  ...cur.hotkeys,
                  disabled_globally: fresh.hotkeys.disabled_globally,
                },
                ui: {
                  ...cur.ui,
                  tray_hint_dismissed: fresh.ui.tray_hint_dismissed,
                },
              });
            }
          } catch {
            /* best-effort reconcile */
          }
          return;
        }
        try {
          await reloadSavedConfig();
        } catch {
          /* config refetch best-effort */
        }
      }),
      // backend pushes this after every hotkey registration pass — chip + Tasks
      // view both refetch so per-task status is current without polling
      await listen("capscr://hotkey-status", () => {
        refetchHotkeyDiag();
      }),
    );

    // background update check — delayed 4s so it doesn't compete with hub
    // first-paint or block the network during the user's first capture.
    // skip entirely if the user has opted out in Settings.
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

    const onKey = (ev: KeyboardEvent) => {
      // prevent default browser find dialog (ctrl+f)
      if (ev.key.toLowerCase() === "f" && (ev.ctrlKey || ev.metaKey)) {
        ev.preventDefault();
        return;
      }
      // F1 — toggle the shortcuts cheatsheet, no modifiers required.
      if (ev.key === "F1" && !ev.ctrlKey && !ev.altKey && !ev.metaKey) {
        ev.preventDefault();
        setShowShortcuts((v) => !v);
        return;
      }
      // escape — close the cheatsheet if it's open.
      if (ev.key === "Escape" && showShortcuts()) {
        ev.preventDefault();
        setShowShortcuts(false);
        return;
      }
      // alt+S/T/H/D/M for tab switching — sidebar titles advertise these so
      // the keybind has to actually work. We respect the dirty-state guard so
      // alt-jumping out of unsaved edits still prompts.
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
        // cap concurrent uploads so dragging 50 files onto the window doesn't
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

  const hasUnsavedConfig = () =>
    configDirty() || captureTaskSaveState().kind === "error";

  const reloadSavedConfig = async () => {
    mutateConfig(await api.getConfig());
  };

  let changingTab = false;
  const tryChangeTab = async (next: Tab) => {
    if (tab().id === next.id) return;
    if (changingTab) return;
    if (hasUnsavedConfig()) {
      if (!window.confirm("Discard unsaved settings changes?")) return;
      changingTab = true;
      try {
        await reloadSavedConfig();
      } catch (error) {
        pushToast("config", `failed to reload saved settings: ${error}`);
        changingTab = false;
        return;
      }
      discardFailedCaptureTaskSave();
      setConfigDirty(false);
      changingTab = false;
    }
    setTab(next);
  };

  const confirmPendingEdits = async (requireEditor: boolean) => {
    await waitForCaptureTaskSaves();
    const discardConfig = hasUnsavedConfig();
    if (discardConfig && !window.confirm("Discard unsaved settings changes?")) {
      return false;
    }
    let savedConfig: AppConfig | undefined;
    if (discardConfig) {
      try {
        savedConfig = await api.getConfig();
      } catch (error) {
        pushToast("config", `failed to reload saved settings: ${error}`);
        return false;
      }
    }
    const closeBehavior = (savedConfig ?? config())?.ui.close_behavior;
    const includeEditor =
      requireEditor || closeBehavior === undefined || closeBehavior === "exit";
    if (includeEditor && !(await confirmEditorExit())) return false;
    if (savedConfig) {
      mutateConfig(savedConfig);
      discardFailedCaptureTaskSave();
      setConfigDirty(false);
    }
    return true;
  };

  let exitActionPending = false;
  const closeHub = async (forceExit: boolean) => {
    if (exitActionPending) return;
    exitActionPending = true;
    try {
      if (!(await confirmPendingEdits(forceExit))) return;
      await api.completeHubClose(forceExit);
    } catch (error) {
      pushToast("close", String(error));
    } finally {
      exitActionPending = false;
    }
  };

  // titlebar quick captures: same pipeline as a task, using the global
  // post-capture action so the result lands where the user expects
  const quickShot = (mode: CaptureMode) => {
    const post = config()?.post_capture.action ?? "save-and-clipboard";
    void api
      .takeScreenshot(mode, post)
      .catch((e) => pushToast("capture", String(e)));
  };

  const recordTask = () =>
    config()?.capture_tasks.find(
      (t) => t.capture_mode === "region-gif" || t.capture_mode === "region-mp4",
    );

  const quickRecord = () => {
    const task = recordTask();
    if (!task || recording()) return;
    void api.fireTask(task.id).catch((e) => pushToast("recording", String(e)));
  };

  // the output dir can be long; the bar shows the last two path segments
  const shortDir = (dir: string) => {
    const parts = dir.replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean);
    return parts.slice(-2).join("/");
  };

  const runUpdate = async () => {
    if (updating() || recording() || exitActionPending) return;
    exitActionPending = true;
    try {
      if (!(await confirmPendingEdits(true))) return;
      setUpdating(true);
      await api.installUpdate();
    } catch (e) {
      const message = String(e);
      pushToast("update", message);
      if (message.includes("not installed")) {
        void openUrl(RELEASES_URL);
      }
      setUpdating(false);
    } finally {
      exitActionPending = false;
    }
  };

  return (
    <div class="app">
      <Titlebar
        onClose={() => void closeHub(false)}
        quick={
          <div class="titlebar-quick">
            <button
              type="button"
              class="qc"
              onClick={() => quickShot("region")}
              title="capture a region now"
            >
              region
            </button>
            <button
              type="button"
              class="qc"
              onClick={() => quickShot("window")}
              title="capture a window now"
            >
              window
            </button>
            <button
              type="button"
              class="qc"
              onClick={() => quickShot("fullscreen")}
              title="capture the primary screen now"
            >
              full
            </button>
            <button
              type="button"
              class="qc"
              classList={{ "is-recording": recording() }}
              onClick={quickRecord}
              disabled={recording() || !recordTask()}
              title={
                recording()
                  ? "recording: stop with the task hotkey or the recording bar"
                  : recordTask()
                    ? `start a ${recordTask()!.capture_mode === "region-mp4" ? "video" : "gif"} recording`
                    : "add a gif or mp4 task to record from here"
              }
            >
              <span class="qc-dot" aria-hidden="true">●</span>
              {recording() ? recordingElapsed() : "record"}
            </button>
          </div>
        }
      />

      <Show when={needsOnboarding()}>
        <div class="onboarding-overlay">
          <div class="onboarding-card">
            <h2>welcome to capscr</h2>
            <p class="lede">let's set up your screenshot shortcut key first</p>
            <p class="desc">
              not every keyboard has a PrintScreen key. click the box below and press any key combination (e.g. PrintScreen, Ctrl+Alt+S, Alt+Shift+A) to bind it.
            </p>
            <div style="margin: 24px 0 12px 0; width: 280px; align-self: center;">
              <HotkeyInput
                value=""
                disabled={onboardingSaving()}
                onChange={async (nextHotkey) => {
                  if (!nextHotkey || onboardingSaving()) return;
                  const c = config();
                  if (!c) return;
                  const index = c.capture_tasks.findIndex((t) => t.id === "screenshot-save-clipboard");
                  if (index !== -1) {
                    const next = [...c.capture_tasks];
                    next[index] = { ...next[index], hotkey: nextHotkey };
                    setOnboardingSaving(true);
                    try {
                      await saveCaptureTasks(next);
                      pushToast("config", "screenshot hotkey bound successfully");
                    } catch (e) {
                      discardFailedCaptureTaskSave();
                      pushToast("config", `failed to save: ${e}`);
                    } finally {
                      setOnboardingSaving(false);
                    }
                  }
                }}
              />
            </div>
          </div>
        </div>
      </Show>

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
          <span class="build">v{__APP_VERSION__}</span>
        </div>
      </aside>

      <Show when={showShortcuts()}>
        <div
          class="shortcuts-overlay"
          onClick={() => setShowShortcuts(false)}
          role="dialog"
          aria-label="keyboard shortcuts"
        >
          <div class="shortcuts-panel" onClick={(e) => e.stopPropagation()}>
            <div class="shortcuts-head">
              <span class="shortcuts-title">keyboard shortcuts</span>
              <button
                type="button"
                class="icon-btn"
                onClick={() => setShowShortcuts(false)}
                aria-label="close"
              >
                <X size={12} stroke-width={1.5} />
              </button>
            </div>
            <div class="shortcuts-body">
              <div class="shortcuts-group">
                <span class="shortcuts-group-label">hub</span>
                <div class="shortcuts-row"><kbd>F1</kbd><span>toggle this overlay</span></div>
                <div class="shortcuts-row"><kbd>Alt</kbd>+<kbd>S</kbd><span>settings tab</span></div>
                <div class="shortcuts-row"><kbd>Alt</kbd>+<kbd>T</kbd><span>tasks tab</span></div>
                <div class="shortcuts-row"><kbd>Alt</kbd>+<kbd>H</kbd><span>history tab</span></div>
                <div class="shortcuts-row"><kbd>Alt</kbd>+<kbd>D</kbd><span>destinations tab</span></div>
                <div class="shortcuts-row"><kbd>Alt</kbd>+<kbd>M</kbd><span>plugins tab</span></div>
                <div class="shortcuts-row"><kbd>Esc</kbd><span>close overlay / hide hub</span></div>
              </div>
              <div class="shortcuts-group">
                <span class="shortcuts-group-label">editor</span>
                <div class="shortcuts-row"><kbd>1</kbd><span>arrow tool</span></div>
                <div class="shortcuts-row"><kbd>2</kbd><span>rect tool</span></div>
                <div class="shortcuts-row"><kbd>3</kbd><span>text tool</span></div>
                <div class="shortcuts-row"><kbd>4</kbd><span>blur tool</span></div>
                <div class="shortcuts-row"><kbd>Ctrl</kbd>+<kbd>Z</kbd><span>undo</span></div>
                <div class="shortcuts-row"><kbd>Ctrl</kbd>+<kbd>Y</kbd><span>redo</span></div>
                <div class="shortcuts-row"><kbd>Ctrl</kbd>+<kbd>V</kbd><span>paste image from clipboard</span></div>
                <div class="shortcuts-row"><kbd>Ctrl</kbd>+<kbd>=</kbd>/<kbd>-</kbd><span>zoom in / out</span></div>
                <div class="shortcuts-row"><kbd>Ctrl</kbd>+<kbd>0</kbd><span>zoom 100%</span></div>
                <div class="shortcuts-row"><kbd>Ctrl</kbd>+wheel<span>zoom</span></div>
              </div>
              <div class="shortcuts-group">
                <span class="shortcuts-group-label">global (default — rebindable in tasks)</span>
                <div class="shortcuts-row"><kbd>PrintScreen</kbd><span>region screenshot → save + clipboard</span></div>
                <div class="shortcuts-row"><kbd>Ctrl+Shift+G</kbd><span>region GIF → save</span></div>
              </div>
            </div>
            <div class="shortcuts-foot">
              <span class="muted">press <kbd>F1</kbd> or <kbd>Esc</kbd> to close</span>
            </div>
          </div>
        </div>
      </Show>

      <main class="content">
        {/* in normal flow at the top of the content area so it pushes the view
            down instead of overlaying the view title */}
        <Show when={trayMissing()}>
          <div class="update-banner">
            <span class="update-banner-glyph">▮</span>
            <div class="update-banner-text">
              <span class="update-banner-title">no system tray detected</span>
              <span class="update-banner-meta">
                common on vanilla GNOME. capscr keeps running in the
                background: global hotkeys still work, right-click the capscr
                entry in Activities for capture actions, and launching capscr
                again reopens this window. for a tray icon, install the
                AppIndicator extension.
              </span>
            </div>
            <button
              type="button"
              class="btn"
              data-size="xs"
              onClick={() =>
                void openUrl("https://extensions.gnome.org/extension/615/appindicator-support/")
              }
            >
              get extension
            </button>
            <button
              type="button"
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={async () => {
                try {
                  await api.dismissTrayHint();
                  const current = config();
                  if (current) {
                    mutateConfig({
                      ...current,
                      ui: { ...current.ui, tray_hint_dismissed: true },
                    });
                  }
                  setTrayMissing(false);
                } catch (error) {
                  pushToast("tray", `failed to save tray preference: ${error}`);
                }
              }}
            >
              don't show again
            </button>
          </div>
        </Show>

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
              <Show when={updateInfo()!.notes}>
                <details class="update-notes">
                  <summary>what's new</summary>
                  <pre class="update-notes-body">{updateInfo()!.notes}</pre>
                </details>
              </Show>
            </div>
            <Show
              when={updateInfo()!.install_kind === "in-place"}
              fallback={
                <button
                  type="button"
                  class="btn"
                  data-size="xs"
                  onClick={() =>
                    void openUrl(updateInfo()!.download_url ?? RELEASES_URL)
                  }
                  title={
                    updateInfo()!.install_kind === "deb"
                      ? "then: sudo apt install ./the-downloaded.deb"
                      : updateInfo()!.install_kind === "rpm"
                        ? "then: sudo dnf install ./the-downloaded.rpm"
                        : undefined
                  }
                >
                  <Download size={11} stroke-width={1.5} />
                  {updateInfo()!.install_kind === "deb"
                    ? "download .deb"
                    : updateInfo()!.install_kind === "rpm"
                      ? "download .rpm"
                      : "get release"}
                </button>
              }
            >
              <button
                type="button"
                class="btn"
                data-size="xs"
                onClick={runUpdate}
                disabled={updating() || recording()}
                title={
                  recording()
                    ? "stop the recording and wait for it to save before updating"
                    : undefined
                }
              >
                <Download size={11} stroke-width={1.5} />
                {updating() ? "installing..." : "install + restart"}
              </button>
            </Show>
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
        {/* keyed on the active tab so switching remounts the wrapper and
            re-runs the brief enter animation (matches the existing toast/drop
            motion vocabulary); same-tab clicks are guarded upstream */}
        <Show when={active()} keyed>
          {(activeId) => (
            <div class="view-anim">
              <Suspense
                fallback={
                  <div class="skeleton" style="margin-top: 14px;">
                    <div class="skeleton-line" style="width: 45%;" />
                  </div>
                }
              >
                <Switch>
                  <Match when={activeId === "settings"}>
                    <Settings />
                  </Match>
                  <Match when={activeId === "tasks"}>
                    <Tasks />
                  </Match>
                  <Match when={activeId === "history"}>
                    <History />
                  </Match>
                  <Match when={activeId === "destinations"}>
                    <Destinations />
                  </Match>
                  <Match when={activeId === "marketplace"}>
                    <Marketplace />
                  </Match>
                </Switch>
              </Suspense>
            </div>
          )}
        </Show>
      </main>

      <footer class="statusbar">
        <Show when={recording()}>
          <span class="seg is-rec">
            <span class="seg-k">rec</span>
            <span class="seg-v">{recordingElapsed()}</span>
          </span>
          <span class="seg-sep">│</span>
        </Show>
        <Show when={config()?.output.directory}>
          <button
            type="button"
            class="seg seg-btn"
            onClick={() =>
              void api
                .openInExplorer(config()!.output.directory)
                .catch((e) => pushToast("err", String(e)))
            }
            title="open the output folder"
          >
            <span class="seg-k">out</span>
            <span class="seg-v">{shortDir(config()!.output.directory)}</span>
          </button>
        </Show>
        <Show when={captures()?.[0]}>
          <span class="seg-sep">│</span>
          <button
            type="button"
            class="seg seg-btn"
            onClick={() =>
              void api
                .openInExplorer(captures()![0].path)
                .catch((e) => pushToast("err", String(e)))
            }
            title="reveal the last capture in the file manager"
          >
            <span class="seg-k">last</span>
            <span class="seg-v">{captures()![0].filename}</span>
          </button>
        </Show>
        <Show when={configDirty()}>
          <span class="seg-sep">│</span>
          <span class="seg is-dirty">
            <span class="seg-k">edit</span>
            <span class="seg-v">unsaved</span>
          </span>
        </Show>
        <Show when={hotkeyDiag()?.disabled_globally}>
          <span class="seg-sep">│</span>
          <button
            type="button"
            class="seg seg-btn is-err"
            onClick={async () => {
              try {
                await api.setHotkeysDisabled(false);
                refetchHotkeyDiag();
              } catch (e) {
                pushToast("err", `couldn't re-enable hotkeys: ${e}`);
              }
            }}
            title="hotkeys are off — click to re-enable"
          >
            <span class="seg-k">keys</span>
            <span class="seg-v">off</span>
          </button>
        </Show>
        <span class="grow" />
        <Show when={updateInfo() && updateDismissed()}>
          <button
            type="button"
            class="seg seg-btn is-update"
            onClick={() => setUpdateDismissed(false)}
            title="update available: reopen the banner"
          >
            <span class="seg-k">update</span>
            <span class="seg-v">v{updateInfo()!.version}</span>
          </button>
          <span class="seg-sep">│</span>
        </Show>
        <button
          type="button"
          class="seg seg-btn"
          onClick={() => setShowShortcuts(true)}
          title="keyboard shortcuts"
        >
          <span class="seg-k">help</span>
          <span class="seg-v">F1</span>
        </button>
        <span class="seg-sep">│</span>
        <span class="seg tail">
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
                    onClick={() => writeText(u.url).catch(() => pushToast("err", "clipboard busy — try again"))}
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
                      onClick={() => writeText(u.deleteUrl!).catch(() => pushToast("err", "clipboard busy — try again"))}
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
