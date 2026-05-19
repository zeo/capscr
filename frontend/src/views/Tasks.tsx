import { createResource, createSignal, For, Show } from "solid-js";
import { Plus, Save, Trash2 } from "lucide-solid";
import { api, AppConfig, CaptureTask } from "../api";
import { setConfigDirty } from "../dirty";
import { HotkeyInput } from "../components/HotkeyInput";

const CAPTURE_MODES: { id: CaptureTask["capture_mode"]; label: string }[] = [
  { id: "region", label: "region (drag a rect)" },
  { id: "window", label: "window (pick one)" },
  { id: "fullscreen", label: "fullscreen (primary)" },
  { id: "active-monitor", label: "active monitor" },
  { id: "region-gif", label: "region gif" },
];

const POST_ACTIONS: { id: CaptureTask["post_action"]; label: string }[] = [
  { id: "clipboard", label: "clipboard only" },
  { id: "save-file", label: "save to output dir" },
  { id: "save-and-clipboard", label: "save + clipboard" },
  { id: "upload", label: "upload" },
  { id: "open-editor", label: "open in editor" },
  { id: "prompt", label: "prompt" },
];

const UPLOAD_TARGETS: NonNullable<CaptureTask["target_destination"]>[] = [
  "imgur",
  "custom",
  "ftp",
];

export function Tasks() {
  const [config, { mutate }] = createResource<AppConfig>(api.getConfig);
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(
    null,
  );

  const updateTask = (index: number, partial: Partial<CaptureTask>) => {
    const c = config();
    if (!c) return;
    const next = [...c.capture_tasks];
    next[index] = { ...next[index], ...partial } as CaptureTask;
    mutate({ ...c, capture_tasks: next });
    setConfigDirty(true);
  };

  const deleteTask = (index: number) => {
    const c = config();
    if (!c) return;
    const next = c.capture_tasks.filter((_, i) => i !== index);
    mutate({ ...c, capture_tasks: next });
    setConfigDirty(true);
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
    mutate({ ...c, capture_tasks: [...c.capture_tasks, newTask] });
    setConfigDirty(true);
  };

  const save = async () => {
    const c = config();
    if (!c) return;

    // Duplicate hotkey guard — two tasks sharing a hotkey means only one fires
    const bound = c.capture_tasks.map((t) => t.hotkey).filter(Boolean);
    const dupes = bound.filter((h, i) => bound.indexOf(h) !== i);
    if (dupes.length > 0) {
      setStatus({ tone: "err", msg: `duplicate hotkey: ${[...new Set(dupes)].join(", ")} — each task needs a unique key combo` });
      return;
    }

    setStatus({ tone: "", msg: "re-registering hotkeys..." });
    try {
      await api.setConfig(c);
      setStatus({
        tone: "ok",
        msg: `${c.capture_tasks.length} task${c.capture_tasks.length === 1 ? "" : "s"} live.`,
      });
      setConfigDirty(false);
    } catch (e) {
      setStatus({ tone: "err", msg: `err: ${e}` });
    }
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
                <button class="btn" data-variant="ghost" onClick={save}>
                  <Save size={12} stroke-width={1.5} />
                  save
                </button>
              </div>
              <Show when={status()}>
                <span class="flash" data-tone={status()!.tone}>
                  {status()!.msg}
                </span>
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
                                onInput={(e) =>
                                  updateTask(i(), {
                                    name: e.currentTarget.value,
                                  })
                                }
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
                                onChange={(e) =>
                                  updateTask(i(), {
                                    capture_mode: e.currentTarget
                                      .value as never,
                                  })
                                }
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
                                <For each={POST_ACTIONS}>
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
                        </div>
                      </div>

                      <div class="list-item-actions">
                        <button
                          class="btn"
                          data-variant="ghost"
                          data-size="xs"
                          onClick={() => deleteTask(i())}
                        >
                          <Trash2 size={11} stroke-width={1.5} />
                          delete
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
