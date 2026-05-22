import { createResource, createSignal, For, Match, onCleanup, Show, Switch } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Section } from "../components/Section";
import { api, AppConfig, HotkeyDiagnostics, SftpKnownHost } from "../api";
import { configDirty, setConfigDirty } from "../dirty";
import { FolderOpen, RotateCcw, Save } from "lucide-solid";

type Pane = "general" | "capture" | "hdr" | "hotkeys" | "ssh" | "notify";

const PANES: { id: Pane; label: string }[] = [
  { id: "general", label: "general" },
  { id: "capture", label: "capture" },
  { id: "hdr", label: "hdr" },
  { id: "hotkeys", label: "hotkeys" },
  { id: "ssh", label: "ssh" },
  { id: "notify", label: "notify" },
];

export function Settings() {
  const [config, { mutate }] = createResource<AppConfig>(api.getConfig);
  const [pane, setPane] = createSignal<Pane>("general");
  const [saving, setSaving] = createSignal(false);
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(
    null,
  );

  const patch = <K extends keyof AppConfig>(key: K, value: AppConfig[K]) => {
    const c = config();
    if (!c) return;
    mutate({ ...c, [key]: value });
    setConfigDirty(true);
  };

  const save = async () => {
    const c = config();
    if (!c) return;
    setSaving(true);
    setStatus({ tone: "", msg: "writing..." });
    try {
      await api.setConfig(c);
      setStatus({ tone: "ok", msg: "saved." });
      setConfigDirty(false);
    } catch (e) {
      setStatus({ tone: "err", msg: `err: ${e}` });
    } finally {
      setSaving(false);
    }
  };

  const resetDefaults = async () => {
    if (!window.confirm("Replace every setting with the defaults? Tasks, hotkeys, and destinations will all reset.")) {
      return;
    }
    setStatus({ tone: "", msg: "loading defaults..." });
    try {
      const defaults = await api.getDefaultConfig();
      mutate(defaults);
      setConfigDirty(true);
      setStatus({ tone: "ok", msg: "loaded — click save to commit." });
    } catch (e) {
      setStatus({ tone: "err", msg: `err: ${e}` });
    }
  };

  return (
    <>
      <div class="view-head">
        <h1>settings</h1>
        <span class="lede">
          <code>%appdata%\capscr\config.toml</code>
        </span>
      </div>

      <nav class="subnav" role="tablist">
        <For each={PANES}>
          {(p) => (
            <button
              type="button"
              role="tab"
              class="subnav-item"
              classList={{ "is-active": pane() === p.id }}
              onClick={() => setPane(p.id)}
              disabled={!config()}
            >
              {p.label}
            </button>
          )}
        </For>
      </nav>

      <Show
        when={config()}
        fallback={
          <div class="skeleton">
            <div class="skeleton-line" style="width: 30%;" />
            <div class="skeleton-line" style="width: 70%;" />
            <div class="skeleton-line" style="width: 45%;" />
            <div class="skeleton-line" style="width: 60%;" />
            <div class="skeleton-line" style="width: 35%;" />
          </div>
        }
      >
        {(c) => (
          <>

            <Switch>
              <Match when={pane() === "general"}>
                <GeneralPane c={c()} patch={patch} />
              </Match>
              <Match when={pane() === "capture"}>
                <CapturePane c={c()} patch={patch} />
              </Match>
              <Match when={pane() === "hdr"}>
                <HdrPane c={c()} patch={patch} />
              </Match>
              <Match when={pane() === "hotkeys"}>
                <HotkeysPane c={c()} />
              </Match>
              <Match when={pane() === "ssh"}>
                <SshPane />
              </Match>
              <Match when={pane() === "notify"}>
                <NotifyPane c={c()} patch={patch} />
              </Match>
            </Switch>

            <hr class="rule" />
            <div class="btn-row right">
              <Show when={status()}>
                <span class="flash" data-tone={status()!.tone}>
                  {status()!.msg}
                </span>
              </Show>
              <button
                class="btn"
                data-variant="ghost"
                onClick={resetDefaults}
                disabled={saving()}
                title="restore every setting to its default"
              >
                <RotateCcw size={12} stroke-width={1.5} />
                reset
              </button>
              <button
                class="btn"
                data-variant={configDirty() ? "primary" : undefined}
                onClick={save}
                disabled={saving() || !configDirty()}
                title={configDirty() ? "commit pending changes" : "no changes to save"}
              >
                <Save size={12} stroke-width={1.5} />
                {saving() ? "saving..." : "save"}
              </button>
            </div>
          </>
        )}
      </Show>
    </>
  );
}

type Patch = <K extends keyof AppConfig>(key: K, value: AppConfig[K]) => void;

function GeneralPane(props: { c: AppConfig; patch: Patch }) {
  const c = () => props.c;
  const pickDirectory = async () => {
    const picked = await openDialog({
      directory: true,
      multiple: false,
      defaultPath: c().output.directory,
      title: "Pick output directory",
    });
    if (typeof picked === "string" && picked.length > 0) {
      props.patch("output", { ...c().output, directory: picked });
    }
  };
  return (
    <Section title="output">
      <div class="field">
        <label class="field-label">directory</label>
        <div class="field-control">
          <div class="input-row">
            <input
              type="text"
              value={c().output.directory}
              onInput={(e) =>
                props.patch("output", { ...c().output, directory: e.currentTarget.value })
              }
            />
            <button
              type="button"
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={pickDirectory}
              title="browse for folder"
            >
              <FolderOpen size={11} stroke-width={1.5} />
              browse
            </button>
          </div>
          <span class="field-hint">absolute path or %env% template</span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">filename template</label>
        <div class="field-control">
          <input
            type="text"
            value={c().output.filename_template}
            onInput={(e) =>
              props.patch("output", {
                ...c().output,
                filename_template: e.currentTarget.value,
              })
            }
          />
          <span class="field-hint">chrono tokens: %Y year · %m month · %d day · %H hour · %M min · %S sec · extension added automatically</span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">format</label>
        <div class="field-control">
          <select
            value={c().output.format}
            onChange={(e) =>
              props.patch("output", {
                ...c().output,
                format: e.currentTarget.value as never,
              })
            }
          >
            <option value="Png">png</option>
            <option value="Jpeg">jpeg</option>
            <option value="Webp">webp</option>
            <option value="Bmp">bmp</option>
          </select>
        </div>
      </div>
      <div class="field">
        <label class="field-label">quality</label>
        <div class="field-control">
          <input
            type="number"
            min={1}
            max={100}
            value={c().output.quality}
            onInput={(e) => {
              const v = parseInt(e.currentTarget.value);
              if (!isNaN(v) && v >= 1 && v <= 100)
                props.patch("output", { ...c().output, quality: v });
            }}
          />
          <span class="field-hint">1-100, ignored for png/bmp</span>
        </div>
      </div>
    </Section>
  );
}

function CapturePane(props: { c: AppConfig; patch: Patch }) {
  const c = () => props.c;
  return (
    <>
      <Section title="cursor">
        <div class="field">
          <label class="field-label">show cursor</label>
          <div class="field-control">
            <label class="check">
              <input
                type="checkbox"
                checked={c().capture.show_cursor}
                onChange={(e) =>
                  props.patch("capture", {
                    ...c().capture,
                    show_cursor: e.currentTarget.checked,
                  })
                }
              />
              <span class="check-label">
                {c().capture.show_cursor ? "captured" : "hidden"}
              </span>
            </label>
          </div>
        </div>
      </Section>

      <Section title="timing">
        <div class="field">
          <label class="field-label">pre-capture delay</label>
          <div class="field-control">
            <input
              type="number"
              min={0}
              max={5000}
              step={100}
              value={c().capture.delay_ms}
              onInput={(e) => {
                const v = parseInt(e.currentTarget.value || "0");
                if (!isNaN(v) && v >= 0 && v <= 5000)
                  props.patch("capture", { ...c().capture, delay_ms: v });
              }}
            />
            <span class="field-hint">ms before grabbing pixels — useful for tooltips / menus (0 = instant)</span>
          </div>
        </div>
      </Section>

      <Section title="gif recording">
        <div class="field">
          <label class="field-label">frame rate</label>
          <div class="field-control">
            <input
              type="number"
              min={1}
              max={60}
              value={c().capture.gif_fps}
              onInput={(e) => {
                const v = parseInt(e.currentTarget.value || "15");
                if (!isNaN(v) && v >= 1 && v <= 60)
                  props.patch("capture", { ...c().capture, gif_fps: v });
              }}
            />
            <span class="field-hint">fps, 1-60</span>
          </div>
        </div>
        <div class="field">
          <label class="field-label">max duration</label>
          <div class="field-control">
            <input
              type="number"
              min={1}
              max={300}
              value={c().capture.gif_max_duration_secs}
              onInput={(e) => {
                const v = parseInt(e.currentTarget.value || "30");
                if (!isNaN(v) && v >= 1 && v <= 300)
                  props.patch("capture", { ...c().capture, gif_max_duration_secs: v });
              }}
            />
            <span class="field-hint">seconds, auto-stops</span>
          </div>
        </div>
      </Section>
    </>
  );
}

function HdrPane(props: { c: AppConfig; patch: Patch }) {
  const c = () => props.c;
  return (
    <>
    <Section title="hdr png sidecar">
      <div class="field">
        <label class="field-label">output format</label>
        <div class="field-control">
          <select
            value={c().capture.hdr.output_format}
            onChange={(e) =>
              props.patch("capture", {
                ...c().capture,
                hdr: { ...c().capture.hdr, output_format: e.currentTarget.value as never },
              })
            }
          >
            <option value="pq">pq (bt.2020, hdr10-style — default)</option>
            <option value="hlg">hlg (bt.2020, broadcast-style)</option>
          </select>
          <span class="field-hint">
            pq is the safest for windows photos / edge. hlg is transcoded from
            the pq source by decoding to nits then applying hlg oetf.
          </span>
        </div>
      </div>
    </Section>
    <Section title="skiv tonemap">
      <div class="field">
        <label class="field-label">mode</label>
        <div class="field-control">
          <select
            value={c().capture.hdr.mode}
            onChange={(e) =>
              props.patch("capture", {
                ...c().capture,
                hdr: { ...c().capture.hdr, mode: e.currentTarget.value as never },
              })
            }
          >
            <option value="map-cll-to-display">map peak to display</option>
            <option value="normalize-to-cll">normalize to peak</option>
          </select>
          <span class="field-hint">
            map = sdr-friendly compress, normalize = preserve relative luminance
          </span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">sdr target</label>
        <div class="field-control">
          <input
            type="number"
            min={1}
            max={10000}
            value={c().capture.hdr.brightness_nits}
            onInput={(e) => {
              const v = parseFloat(e.currentTarget.value);
              if (!isNaN(v) && v >= 1 && v <= 10000)
                props.patch("capture", {
                  ...c().capture,
                  hdr: { ...c().capture.hdr, brightness_nits: v },
                });
            }}
          />
          <span class="field-hint">paper-white target, nits</span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">pre-tonemap scale</label>
        <div class="field-control">
          <input
            type="number"
            min={0.01}
            max={100}
            step={0.05}
            value={c().capture.hdr.user_brightness_scale}
            onInput={(e) => {
              const v = parseFloat(e.currentTarget.value);
              if (!isNaN(v) && v > 0 && v <= 100)
                props.patch("capture", {
                  ...c().capture,
                  hdr: { ...c().capture.hdr, user_brightness_scale: v },
                });
            }}
          />
          <span class="field-hint">multiply luminance before mapping (1.0 = identity)</span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">p99 maxcll</label>
        <div class="field-control">
          <label class="check">
            <input
              type="checkbox"
              checked={c().capture.hdr.use_p99_max_cll}
              onChange={(e) =>
                props.patch("capture", {
                  ...c().capture,
                  hdr: {
                    ...c().capture.hdr,
                    use_p99_max_cll: e.currentTarget.checked,
                  },
                })
              }
            />
            <span class="check-label">
              {c().capture.hdr.use_p99_max_cll
                ? "p99 (sky / lights ignored)"
                : "pure max, every spike counted"}
            </span>
          </label>
        </div>
      </div>
      <div class="field">
        <label class="field-label">preserve hdr</label>
        <div class="field-control">
          <label class="check">
            <input
              type="checkbox"
              checked={c().output.preserve_hdr}
              onChange={(e) =>
                props.patch("output", {
                  ...c().output,
                  preserve_hdr: e.currentTarget.checked,
                })
              }
            />
            <span class="check-label">
              {c().output.preserve_hdr
                ? `writes a 16-bit bt.2020+${c().capture.hdr.output_format} .hdr.png sidecar next to each hdr capture`
                : "tonemaps to sdr only — no hdr sidecar saved"}
            </span>
          </label>
          <span class="field-hint">
            source format is HDR10 (the common case). transfer = whichever
            output format you pick above. fullscreen / active-monitor captures only.
          </span>
        </div>
      </div>
    </Section>
    </>
  );
}


function HotkeysPane(props: { c: AppConfig }) {
  const [diag, { refetch }] = createResource<HotkeyDiagnostics>(api.hotkeyDiagnostics);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  const unlistenPromise = listen("capscr://hotkey-status", () => refetch());
  onCleanup(() => {
    unlistenPromise.then((u) => u());
  });

  const toggle = async () => {
    const d = diag();
    if (!d) return;
    setBusy(true);
    setErr(null);
    try {
      await api.setHotkeysDisabled(!d.disabled_globally);
      await refetch();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const tasks = () => props.c.capture_tasks;

  return (
    <>
      <Section title="global">
        <div class="field">
          <label class="field-label">all hotkeys</label>
          <div class="field-control">
            <button
              class="btn"
              data-variant={diag()?.disabled_globally ? "primary" : "ghost"}
              disabled={busy()}
              onClick={toggle}
            >
              {diag()?.disabled_globally ? "re-enable" : "disable all"}
            </button>
            <span class="field-hint">
              kill switch toggled from the tray menu too. survives restart.
            </span>
          </div>
        </div>
        <Show when={err()}>
          <p class="flash" data-tone="err">{err()}</p>
        </Show>
      </Section>

      <Section title="per-binding status">
        <div class="field-control" style="flex-direction: column; align-items: stretch;">
          <Show when={tasks().length > 0} fallback={<p class="lede">no tasks defined.</p>}>
            <table class="diag-table">
              <thead>
                <tr>
                  <th>task</th>
                  <th>hotkey</th>
                  <th>status</th>
                  <th>reason</th>
                </tr>
              </thead>
              <tbody>
                <For each={tasks()}>
                  {(task) => {
                    const entry = () =>
                      diag()?.statuses.find((s) => s.task_id === task.id) ?? null;
                    const e = entry();
                    return (
                      <tr>
                        <td>{task.name || task.id}</td>
                        <td>{task.hotkey || <span class="warn">unbound</span>}</td>
                        <td>
                          <Show
                            when={task.hotkey}
                            fallback={<span class="lede">—</span>}
                          >
                            <Show
                              when={e}
                              fallback={<span class="lede">unknown</span>}
                            >
                              <span
                                class={
                                  entry()?.status === "live"
                                    ? "chip-live"
                                    : "chip-fail"
                                }
                              >
                                ● {entry()?.status}
                              </span>
                            </Show>
                          </Show>
                        </td>
                        <td>
                          <span class="lede">{entry()?.reason ?? ""}</span>
                        </td>
                      </tr>
                    );
                  }}
                </For>
              </tbody>
            </table>
          </Show>
        </div>
      </Section>
    </>
  );
}

function SshPane() {
  const [hosts, { refetch }] = createResource<SftpKnownHost[]>(api.sftpKnownHosts);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [err, setErr] = createSignal<string | null>(null);

  const forget = async (hostPort: string) => {
    setBusy(hostPort);
    setErr(null);
    try {
      await api.sftpForgetHost(hostPort);
      await refetch();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const formatDate = (unix: number) => {
    if (!unix) return "—";
    return new Date(unix * 1000).toISOString().slice(0, 10);
  };

  return (
    <Section title="known sftp hosts">
      <p class="lede">
        capscr remembers each sftp server's public-key fingerprint on first
        connect and refuses to upload on a later mismatch. forget a host to
        re-trust a rotated key.
      </p>
      <Show when={err()}>
        <p class="flash" data-tone="err">{err()}</p>
      </Show>
      <div class="field-control" style="flex-direction: column; align-items: stretch;">
        <Show when={hosts() && hosts()!.length > 0} fallback={
          <p class="lede">no hosts trusted yet. upload via sftp to populate this list.</p>
        }>
          <table class="diag-table">
            <thead>
              <tr>
                <th>host:port</th>
                <th>fingerprint</th>
                <th>first seen</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              <For each={hosts()}>
                {(h) => (
                  <tr>
                    <td>{h.host_port}</td>
                    <td style="word-break: break-all;">{h.fingerprint}</td>
                    <td>{formatDate(h.first_seen_unix)}</td>
                    <td>
                      <button
                        class="btn"
                        data-variant="ghost"
                        data-size="xs"
                        disabled={busy() === h.host_port}
                        onClick={() => forget(h.host_port)}
                      >
                        forget
                      </button>
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </Show>
      </div>
    </Section>
  );
}

function NotifyPane(props: { c: AppConfig; patch: Patch }) {
  const c = () => props.c;
  return (
    <>
      <Section title="feedback">
        <div class="field">
          <label class="field-label">os notifications</label>
          <div class="field-control">
            <label class="check">
              <input
                type="checkbox"
                checked={c().ui.show_notifications}
                onChange={(e) =>
                  props.patch("ui", {
                    ...c().ui,
                    show_notifications: e.currentTarget.checked,
                  })
                }
              />
              <span class="check-label">
                {c().ui.show_notifications ? "on" : "silent"}
              </span>
            </label>
          </div>
        </div>
        <div class="field">
          <label class="field-label">sound cue</label>
          <div class="field-control">
            <label class="check">
              <input
                type="checkbox"
                checked={c().post_capture.play_sound}
                onChange={(e) =>
                  props.patch("post_capture", {
                    ...c().post_capture,
                    play_sound: e.currentTarget.checked,
                  })
                }
              />
              <span class="check-label">
                {c().post_capture.play_sound
                  ? "win32 playsound on capture / upload"
                  : "silent"}
              </span>
            </label>
          </div>
        </div>
      </Section>

      <Section title="system">
        <div class="field">
          <label class="field-label">launch on boot</label>
          <div class="field-control">
            <label class="check">
              <input
                type="checkbox"
                checked={c().ui.auto_start}
                onChange={(e) =>
                  props.patch("ui", {
                    ...c().ui,
                    auto_start: e.currentTarget.checked,
                  })
                }
              />
              <span class="check-label">
                {c().ui.auto_start
                  ? "registered in windows run keys"
                  : "manual launch only"}
              </span>
            </label>
            <span class="field-hint">applied on next save</span>
          </div>
        </div>
        <div class="field">
          <label class="field-label">minimize to tray</label>
          <div class="field-control">
            <label class="check">
              <input
                type="checkbox"
                checked={c().ui.minimize_to_tray}
                onChange={(e) =>
                  props.patch("ui", {
                    ...c().ui,
                    minimize_to_tray: e.currentTarget.checked,
                  })
                }
              />
              <span class="check-label">close button minimizes to taskbar, doesn't exit</span>
            </label>
          </div>
        </div>
        <div class="field">
          <label class="field-label">check for updates</label>
          <div class="field-control">
            <label class="check">
              <input
                type="checkbox"
                checked={c().ui.check_updates_on_launch}
                onChange={(e) =>
                  props.patch("ui", {
                    ...c().ui,
                    check_updates_on_launch: e.currentTarget.checked,
                  })
                }
              />
              <span class="check-label">
                {c().ui.check_updates_on_launch
                  ? "queries GitHub releases 4s after hub opens"
                  : "no network call, you'll need to grab updates manually"}
              </span>
            </label>
          </div>
        </div>
      </Section>
    </>
  );
}
