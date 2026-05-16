import { createResource, createSignal, For, Show } from "solid-js";
import { api, AppConfig, CaptureTask } from "../api";

const CAPTURE_MODES: CaptureTask["capture_mode"][] = [
  "region",
  "window",
  "fullscreen",
  "active-monitor",
  "region-gif",
];

const POST_ACTIONS: CaptureTask["post_action"][] = [
  "clipboard",
  "save-file",
  "upload",
  "save-and-clipboard",
  "open-editor",
  "prompt",
];

const UPLOAD_TARGETS: NonNullable<CaptureTask["target_destination"]>[] = [
  "imgur",
  "custom",
  "ftp",
];

export function Tasks() {
  const [config, { mutate }] = createResource<AppConfig>(api.getConfig);
  const [status, setStatus] = createSignal("");

  const updateTask = (index: number, partial: Partial<CaptureTask>) => {
    const c = config();
    if (!c) return;
    const next = [...c.capture_tasks];
    next[index] = { ...next[index], ...partial } as CaptureTask;
    mutate({ ...c, capture_tasks: next });
  };

  const deleteTask = (index: number) => {
    const c = config();
    if (!c) return;
    const next = c.capture_tasks.filter((_, i) => i !== index);
    mutate({ ...c, capture_tasks: next });
  };

  const addTask = () => {
    const c = config();
    if (!c) return;
    const id = `task-${Date.now().toString(36)}`;
    const newTask: CaptureTask = {
      id,
      name: "New task",
      hotkey: "",
      capture_mode: "region",
      post_action: "clipboard",
      target_destination: null,
    };
    mutate({ ...c, capture_tasks: [...c.capture_tasks, newTask] });
  };

  const save = async () => {
    const c = config();
    if (!c) return;
    setStatus("Saving...");
    try {
      await api.setConfig(c);
      setStatus("Saved. Hotkeys re-registered.");
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
  };

  return (
    <>
      <h1>Capture tasks</h1>
      <p style="color: var(--fg-dim); max-width: 600px;">
        Each task binds a global hotkey to a capture mode plus a post-action
        (clipboard, save, upload, etc). Press the hotkey from anywhere to fire
        the task.
      </p>
      <Show when={config()}>
        {(c) => (
          <>
            <div class="row" style="margin-bottom: 12px;">
              <button onClick={addTask}>Add task</button>
              <button onClick={save}>Save tasks</button>
              <span class="status">{status()}</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: 12px;">
              <For each={c().capture_tasks}>
                {(task, i) => (
                  <div class="card" style="padding: 12px;">
                    <div class="field">
                      <label>Name</label>
                      <input
                        type="text"
                        value={task.name}
                        onInput={(e) =>
                          updateTask(i(), { name: e.currentTarget.value })
                        }
                      />
                    </div>
                    <div class="field">
                      <label>Hotkey</label>
                      <input
                        type="text"
                        placeholder="e.g. Numpad5, Ctrl+Shift+S"
                        value={task.hotkey}
                        onInput={(e) =>
                          updateTask(i(), { hotkey: e.currentTarget.value })
                        }
                      />
                    </div>
                    <div class="field">
                      <label>Capture mode</label>
                      <select
                        value={task.capture_mode}
                        onChange={(e) =>
                          updateTask(i(), {
                            capture_mode: e.currentTarget.value as never,
                          })
                        }
                      >
                        <For each={CAPTURE_MODES}>
                          {(m) => <option value={m}>{m}</option>}
                        </For>
                      </select>
                    </div>
                    <div class="field">
                      <label>Post-action</label>
                      <select
                        value={task.post_action}
                        onChange={(e) =>
                          updateTask(i(), {
                            post_action: e.currentTarget.value as never,
                          })
                        }
                      >
                        <For each={POST_ACTIONS}>
                          {(p) => <option value={p}>{p}</option>}
                        </For>
                      </select>
                    </div>
                    <Show when={task.post_action === "upload"}>
                      <div class="field">
                        <label>Upload target</label>
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
                    </Show>
                    <div class="row" style="margin-top: 8px;">
                      <button class="ghost" onClick={() => deleteTask(i())}>
                        Delete
                      </button>
                      <span style="color: var(--fg-dim); font-size: 11px;">
                        id: {task.id}
                      </span>
                    </div>
                  </div>
                )}
              </For>
            </div>
          </>
        )}
      </Show>
    </>
  );
}
