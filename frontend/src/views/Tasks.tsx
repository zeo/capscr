import { createResource, createSignal, For, onCleanup, Show } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { Plus, Trash2, Zap } from "lucide-solid";
import { api, type CaptureTask, type HotkeyDiagnostics } from "../api";
import { HotkeyInput } from "../components/HotkeyInput";
import {
  captureTaskSaveState,
  config,
  queueCaptureTasks,
  retryCaptureTaskSave,
} from "../store";

const CAPTURE_MODES: { id: CaptureTask["capture_mode"]; label: string }[] = [
  { id: "region", label: "region (drag a rect)" },
  { id: "region-last", label: "region (last — no drag)" },
  { id: "window", label: "window (pick one)" },
  { id: "fullscreen", label: "fullscreen (primary)" },
  { id: "active-monitor", label: "active monitor" },
  { id: "region-gif", label: "region gif" },
  { id: "region-mp4", label: "region mp4 (video)" },
];

const POST_ACTIONS: { id: CaptureTask["post_action"]; label: string }[] = [
  { id: "clipboard", label: "clipboard only" },
  { id: "save-file", label: "save to output dir" },
  { id: "save-and-clipboard", label: "save + clipboard" },
  { id: "upload", label: "upload" },
  { id: "open-editor", label: "open in editor" },
  { id: "copy-text", label: "copy detected text (ocr)" },
  { id: "prompt", label: "prompt" },
  { id: "do-nothing", label: "do nothing" },
];

const isRecordingMode = (mode: CaptureTask["capture_mode"]) =>
  mode === "region-gif" || mode === "region-mp4";

// recordings can't be edited or OCR'd (the editor would flatten the animation,
// and there's no still frame to read text from), so those post-actions are only
// offered for still-image modes
const postActionsFor = (mode: CaptureTask["capture_mode"]) =>
  isRecordingMode(mode)
    ? POST_ACTIONS.filter((p) => p.id !== "open-editor" && p.id !== "copy-text")
    : POST_ACTIONS;

const UPLOAD_TARGETS: NonNullable<CaptureTask["target_destination"]>[] = [
  "imgur",
  "custom",
  "sftp",
  "s3",
];

export function Tasks() {
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(
    null,
  );
  const [diag, { refetch: refetchDiag }] = createResource<HotkeyDiagnostics>(
    api.hotkeyDiagnostics,
  );
  let disposed = false;
  const unlistenPromise = listen("capscr://hotkey-status", () => {
    refetchDiag();
  }).catch((error) => {
    if (!disposed) {
      setStatus({ tone: "err", msg: `hotkey listener failed: ${error}` });
    }
    return undefined;
  });
  onCleanup(() => {
    disposed = true;
    void unlistenPromise.then((unlisten) => unlisten?.());
  });

  const statusFor = (taskId: string) => {
    const d = diag();
    if (!d) return null;
    return d.statuses.find((s) => s.task_id === taskId) ?? null;
  };

  // deleting a task persists immediately, so guard it the way History guards a
  // capture delete: first click arms, a second within 4s confirms
  const [confirmDelete, setConfirmDelete] = createSignal<string | null>(null);
  let armTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(armTimer));
  const armDelete = (id: string) => {
    clearTimeout(armTimer);
    setConfirmDelete(id);
    armTimer = setTimeout(() => {
      if (confirmDelete() === id) setConfirmDelete(null);
    }, 4000);
  };

  const visibleStatus = () => {
    const local = status();
    if (local) return local;
    const save = captureTaskSaveState();
    if (save.kind === "saving") return { tone: "", msg: "saving..." };
    if (save.kind === "saved") {
      return {
        tone: "ok",
        msg: `${save.count} task${save.count === 1 ? "" : "s"} live.`,
      };
    }
    if (save.kind === "error") return { tone: "err", msg: `err: ${save.message}` };
    return null;
  };

  const applyTasks = (captureTasks: CaptureTask[]) => {
    const bound = captureTasks.map((task) => task.hotkey).filter(Boolean);
    const duplicates = [...new Set(bound.filter((hotkey, i) => bound.indexOf(hotkey) !== i))];
    if (duplicates.length > 0) {
      setStatus({
        tone: "err",
        msg: `duplicate hotkey: ${duplicates.join(", ")}, each task needs a unique key combo`,
      });
      return;
    }
    setStatus(null);
    queueCaptureTasks(captureTasks);
  };

  const updateTask = (index: number, partial: Partial<CaptureTask>) => {
    const c = config();
    if (!c) return;
    const next = [...c.capture_tasks];
    next[index] = { ...next[index], ...partial } as CaptureTask;
    applyTasks(next);
  };

  const deleteTask = (index: number) => {
    const c = config();
    if (!c) return;
    clearTimeout(armTimer);
    setConfirmDelete(null);
    const next = c.capture_tasks.filter((_, i) => i !== index);
    applyTasks(next);
  };

  const addTask = () => {
    const c = config();
    if (!c) return;
    const id = `task-${Date.now().toString(36)}`;
    const newTask: CaptureTask = {
      id,
      name: "new task",
      hotkey: "",
      capture_mode: "region",
      post_action: "save-and-clipboard",
      target_destination: null,
    };
    applyTasks([...c.capture_tasks, newTask]);
  };

  return (
    <>
      <div class="view-head">
        <h1>tasks</h1>
        <span class="lede">
          one hotkey, one capture, one post-action.
        </span>
      </div>

      <Show
        when={config()}
        fallback={
          <div class="skeleton">
            <div class="skeleton-line" style="width: 35%;" />
            <div class="skeleton-line" style="width: 60%;" />
            <div class="skeleton-line" style="width: 45%;" />
            <div class="skeleton-line" style="width: 70%;" />
          </div>
        }
      >
        {(c) => (
          <>
            <div class="row between" style="margin-bottom: 18px;">
              <div class="btn-row">
                <button class="btn" onClick={addTask}>
                  <Plus size={12} stroke-width={1.5} />
                  new
                </button>
              </div>
              <Show when={visibleStatus()}>
                <div class="btn-row">
                  <span
                    class="flash"
                    data-tone={visibleStatus()!.tone}
                    role={visibleStatus()!.tone === "err" ? "alert" : "status"}
                    aria-live={visibleStatus()!.tone === "err" ? "assertive" : "polite"}
                    aria-atomic="true"
                  >
                    {visibleStatus()!.msg}
                  </span>
                  <Show when={captureTaskSaveState().kind === "error"}>
                    <button class="btn" data-size="xs" onClick={retryCaptureTaskSave}>
                      retry
                    </button>
                  </Show>
                </div>
              </Show>
            </div>

            <Show
              when={c().capture_tasks.length > 0}
              fallback={
                <div class="empty">
                  <span class="stick" />
                  no tasks
                  <p>
                    press <kbd>new</kbd>, give it a hotkey, save.
                  </p>
                </div>
              }
            >
              <div class="list">
                <For each={c().capture_tasks}>
                  {(task, i) => (
                    <div class="list-item">
                      <div class="list-item-body">
                        <div class="list-item-title">{task.name || "—"}</div>
                        <div class="list-item-meta">
                          <span>
                            <span class="k">mode </span>
                            <span class="v">{task.capture_mode}</span>
                          </span>
                          <span>
                            <span class="k">post </span>
                            <span class="v">{task.post_action}</span>
                          </span>
                          <Show
                            when={task.hotkey}
                            fallback={
                              <span class="warn">
                                <span class="k">key </span>
                                <span class="v">unbound</span>
                              </span>
                            }
                          >
                            <span>
                              <span class="k">key </span>
                              <span class="v">{task.hotkey}</span>
                            </span>
                            <Show when={statusFor(task.id)}>
                              {(s) => (
                                <span
                                  class={
                                    s().status === "live"
                                      ? "chip-live"
                                      : "chip-fail"
                                  }
                                  title={s().reason ?? "registered with the keyboard hook"}
                                >
                                  ● {s().status === "live"
                                    ? "live"
                                    : "rejected"}
                                </span>
                              )}
                            </Show>
                          </Show>
                          <Show when={!task.name.trim()}>
                            <span class="warn">
                              <span class="k">name </span>
                              <span class="v">required</span>
                            </span>
                          </Show>
                          <span>
                            <span class="k">id </span>
                            <span class="v">{task.id}</span>
                          </span>
                        </div>

                        <div class="list-item-fields">
                          <div class="field">
                            <label class="field-label">name</label>
                            <div class="field-control">
                              <input
                                type="text"
                                value={task.name}
                                onChange={(e) => {
                                  updateTask(i(), {
                                    name: e.currentTarget.value,
                                  });
                                }}
                              />
                            </div>
                          </div>
                          <div class="field">
                            <label class="field-label">hotkey</label>
                            <div class="field-control">
                              <HotkeyInput
                                value={task.hotkey}
                                onChange={(v) =>
                                  updateTask(i(), { hotkey: v })
                                }
                              />
                              <span class="field-hint">
                                click, press combo
                              </span>
                            </div>
                          </div>
                          <div class="field">
                            <label class="field-label">capture mode</label>
                            <div class="field-control">
                              <select
                                value={task.capture_mode}
                                onChange={(e) => {
                                  const mode = e.currentTarget
                                    .value as CaptureTask["capture_mode"];
                                  const update: Partial<CaptureTask> = {
                                    capture_mode: mode,
                                  };
                                  // editor and ocr post-actions don't apply to
                                  // recordings — fall back to save so the task
                                  // isn't left on a filtered-out action that
                                  // silently no-ops when it fires
                                  if (
                                    isRecordingMode(mode) &&
                                    (task.post_action === "open-editor" ||
                                      task.post_action === "copy-text")
                                  ) {
                                    update.post_action = "save-file";
                                  }
                                  updateTask(i(), update);
                                }}
                              >
                                <For each={CAPTURE_MODES}>
                                  {(m) => (
                                    <option value={m.id}>{m.label}</option>
                                  )}
                                </For>
                              </select>
                            </div>
                          </div>
                          <div class="field">
                            <label class="field-label">post-action</label>
                            <div class="field-control">
                              <select
                                value={task.post_action}
                                onChange={(e) => {
                                  const action = e.currentTarget.value as CaptureTask["post_action"];
                                  const update: Partial<CaptureTask> = { post_action: action };
                                  // auto-pick imgur when switching to upload so target is never null
                                  if (action === "upload" && !task.target_destination) {
                                    update.target_destination = "imgur";
                                  }
                                  updateTask(i(), update);
                                }}
                              >
                                <For each={postActionsFor(task.capture_mode)}>
                                  {(p) => (
                                    <option value={p.id}>{p.label}</option>
                                  )}
                                </For>
                              </select>
                            </div>
                          </div>
                          <Show when={task.post_action === "upload"}>
                            <div class="field">
                              <label class="field-label">target</label>
                              <div class="field-control">
                                <select
                                  value={task.target_destination ?? "imgur"}
                                  onChange={(e) =>
                                    updateTask(i(), {
                                      target_destination: e.currentTarget
                                        .value as never,
                                    })
                                  }
                                >
                                  <For each={UPLOAD_TARGETS}>
                                    {(t) => <option value={t}>{t}</option>}
                                  </For>
                                </select>
                              </div>
                            </div>
                          </Show>
                          <Show when={!isRecordingMode(task.capture_mode)}>
                            <div class="field">
                              <label class="field-label">delay</label>
                              <div class="field-control">
                                <input
                                  type="number"
                                  min={0}
                                  max={30000}
                                  step={100}
                                  placeholder="global"
                                  value={task.delay_ms ?? ""}
                                  onChange={(e) => {
                                    const raw = e.currentTarget.value.trim();
                                    const v =
                                      raw === ""
                                        ? null
                                        : Math.min(30000, Math.max(0, Math.round(Number(raw) || 0)));
                                    e.currentTarget.value = v === null ? "" : String(v);
                                    updateTask(i(), { delay_ms: v });
                                  }}
                                />
                                <span class="field-hint">
                                  ms before capture — blank uses the global delay
                                </span>
                              </div>
                            </div>
                          </Show>
                        </div>
                      </div>

                      <div class="list-item-actions">
                        <button
                          class="btn"
                          data-variant="ghost"
                          data-size="xs"
                          title="run this task now (same path as pressing its hotkey)"
                          onClick={() => {
                            api
                              .fireTask(task.id)
                              .catch((e) =>
                                setStatus({ tone: "err", msg: `fire: ${e}` }),
                              );
                          }}
                        >
                          <Zap size={11} stroke-width={1.5} />
                          fire
                        </button>
                        <button
                          class="btn"
                          data-variant="ghost"
                          data-size="xs"
                          classList={{ "is-arm": confirmDelete() === task.id }}
                          title={
                            confirmDelete() === task.id
                              ? "click again to confirm"
                              : "delete this task"
                          }
                          onClick={() =>
                            confirmDelete() === task.id
                              ? deleteTask(i())
                              : armDelete(task.id)
                          }
                        >
                          <Trash2 size={11} stroke-width={1.5} />
                          {confirmDelete() === task.id ? "confirm?" : "delete"}
                        </button>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            </Show>
          </>
        )}
      </Show>
    </>
  );
}
