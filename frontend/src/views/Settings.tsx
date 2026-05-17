import { createResource, createSignal, For, Match, Show, Switch } from "solid-js";
import { Section } from "../components/Section";
import { HotkeyInput } from "../components/HotkeyInput";
import { api, AppConfig } from "../api";
import { Save } from "lucide-solid";

type Pane = "general" | "capture" | "hdr" | "hotkeys" | "notify";

const PANES: { id: Pane; num: string; label: string }[] = [
  { id: "general", num: "i", label: "general" },
  { id: "capture", num: "ii", label: "capture" },
  { id: "hdr", num: "iii", label: "hdr" },
  { id: "hotkeys", num: "iv", label: "hotkeys" },
  { id: "notify", num: "v", label: "notify" },
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
  };

  const save = async () => {
    const c = config();
    if (!c) return;
    setSaving(true);
    setStatus({ tone: "", msg: "writing..." });
    try {
      await api.setConfig(c);
      setStatus({ tone: "ok", msg: "saved." });
    } catch (e) {
      setStatus({ tone: "err", msg: `err: ${e}` });
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div class="view-head">
        <span class="num">i</span>
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
              <span style="opacity: .55; margin-right: 8px;">{p.num}</span>
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
                <HotkeysPane c={c()} patch={patch} />
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
              <button class="btn" onClick={save} disabled={saving()}>
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
  return (
    <Section num="i" title="output">
      <div class="field">
        <label class="field-label">directory</label>
        <div class="field-control">
          <input
            type="text"
            value={c().output.directory}
            onInput={(e) =>
              props.patch("output", { ...c().output, directory: e.currentTarget.value })
            }
          />
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
          <span class="field-hint">{`tokens: {date} {time} {seq} {ext}`}</span>
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
            onInput={(e) =>
              props.patch("output", {
                ...c().output,
                quality: parseInt(e.currentTarget.value || "0"),
              })
            }
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
      <Section num="i" title="cursor">
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

      <Section num="ii" title="gif recording">
        <div class="field">
          <label class="field-label">frame rate</label>
          <div class="field-control">
            <input
              type="number"
              min={1}
              max={60}
              value={c().capture.gif_fps}
              onInput={(e) =>
                props.patch("capture", {
                  ...c().capture,
                  gif_fps: parseInt(e.currentTarget.value || "15"),
                })
              }
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
              onInput={(e) =>
                props.patch("capture", {
                  ...c().capture,
                  gif_max_duration_secs: parseInt(
                    e.currentTarget.value || "30",
                  ),
                })
              }
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
    <Section num="i" title="skiv tonemap">
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
            onInput={(e) =>
              props.patch("capture", {
                ...c().capture,
                hdr: {
                  ...c().capture.hdr,
                  brightness_nits: parseFloat(e.currentTarget.value || "80"),
                },
              })
            }
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
            onInput={(e) =>
              props.patch("capture", {
                ...c().capture,
                hdr: {
                  ...c().capture.hdr,
                  user_brightness_scale: parseFloat(
                    e.currentTarget.value || "1",
                  ),
                },
              })
            }
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
    </Section>
  );
}

function HotkeysPane(props: { c: AppConfig; patch: Patch }) {
  const c = () => props.c;
  return (
    <Section num="i" title="quick hotkeys">
      <div class="field">
        <label class="field-label">screenshot</label>
        <div class="field-control">
          <HotkeyInput
            value={c().hotkeys.screenshot}
            onChange={(v) =>
              props.patch("hotkeys", { ...c().hotkeys, screenshot: v })
            }
          />
          <span class="field-hint">click, press combo. esc cancels.</span>
        </div>
      </div>
      <div class="field">
        <label class="field-label">record gif</label>
        <div class="field-control">
          <HotkeyInput
            value={c().hotkeys.record_gif}
            onChange={(v) =>
              props.patch("hotkeys", { ...c().hotkeys, record_gif: v })
            }
          />
          <span class="field-hint">for per-task hotkeys, see tasks tab</span>
        </div>
      </div>
    </Section>
  );
}

function NotifyPane(props: { c: AppConfig; patch: Patch }) {
  const c = () => props.c;
  return (
    <>
      <Section num="i" title="feedback">
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

      <Section num="ii" title="system">
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
              <span class="check-label">close button hides window, doesn't exit</span>
            </label>
          </div>
        </div>
      </Section>
    </>
  );
}
