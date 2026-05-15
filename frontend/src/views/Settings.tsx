import { createResource, createSignal, Show } from "solid-js";
import { api, AppConfig } from "../api";

export function Settings() {
  const [config, { mutate }] = createResource<AppConfig>(api.getConfig);
  const [saving, setSaving] = createSignal(false);
  const [status, setStatus] = createSignal<string>("");

  const updateAnd = <K extends keyof AppConfig>(
    key: K,
    value: AppConfig[K],
  ) => {
    const current = config();
    if (!current) return;
    mutate({ ...current, [key]: value });
  };

  const save = async () => {
    const current = config();
    if (!current) return;
    setSaving(true);
    setStatus("Saving...");
    try {
      await api.setConfig(current);
      setStatus("Saved");
    } catch (e) {
      setStatus(`Error: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <h1>Settings</h1>
      <Show when={config()}>
        {(c) => (
          <>
            <h2>Output</h2>
            <div class="field">
              <label>Directory</label>
              <input
                type="text"
                value={c().output.directory}
                onInput={(e) =>
                  updateAnd("output", {
                    ...c().output,
                    directory: e.currentTarget.value,
                  })
                }
              />
            </div>
            <div class="field">
              <label>Format</label>
              <select
                value={c().output.format}
                onChange={(e) =>
                  updateAnd("output", {
                    ...c().output,
                    format: e.currentTarget.value as never,
                  })
                }
              >
                <option value="Png">PNG</option>
                <option value="Jpeg">JPEG</option>
                <option value="Webp">WebP</option>
                <option value="Bmp">BMP</option>
              </select>
            </div>
            <div class="field">
              <label>Quality (1-100)</label>
              <input
                type="number"
                min={1}
                max={100}
                value={c().output.quality}
                onInput={(e) =>
                  updateAnd("output", {
                    ...c().output,
                    quality: parseInt(e.currentTarget.value || "0"),
                  })
                }
              />
            </div>

            <h2>HDR</h2>
            <div class="field">
              <label>Compression mode</label>
              <select
                value={c().capture.hdr.mode}
                onChange={(e) =>
                  updateAnd("capture", {
                    ...c().capture,
                    hdr: {
                      ...c().capture.hdr,
                      mode: e.currentTarget.value as never,
                    },
                  })
                }
              >
                <option value="map-cll-to-display">
                  Map peak to display (SDR-friendly)
                </option>
                <option value="normalize-to-cll">
                  Normalize to peak (HDR-friendly)
                </option>
              </select>
            </div>
            <div class="field">
              <label>SDR brightness target (nits)</label>
              <input
                type="number"
                min={1}
                max={10000}
                step={1}
                value={c().capture.hdr.brightness_nits}
                onInput={(e) =>
                  updateAnd("capture", {
                    ...c().capture,
                    hdr: {
                      ...c().capture.hdr,
                      brightness_nits: parseFloat(
                        e.currentTarget.value || "80",
                      ),
                    },
                  })
                }
              />
            </div>
            <div class="field">
              <label>Pre-tonemap brightness scale</label>
              <input
                type="number"
                min={0.01}
                max={100}
                step={0.05}
                value={c().capture.hdr.user_brightness_scale}
                onInput={(e) =>
                  updateAnd("capture", {
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
            </div>
            <div class="field">
              <label>Use P99 MaxCLL (smooths spikes)</label>
              <input
                type="checkbox"
                checked={c().capture.hdr.use_p99_max_cll}
                onChange={(e) =>
                  updateAnd("capture", {
                    ...c().capture,
                    hdr: {
                      ...c().capture.hdr,
                      use_p99_max_cll: e.currentTarget.checked,
                    },
                  })
                }
              />
            </div>

            <h2>Hotkeys</h2>
            <div class="field">
              <label>Screenshot</label>
              <input
                type="text"
                value={c().hotkeys.screenshot}
                onInput={(e) =>
                  updateAnd("hotkeys", {
                    ...c().hotkeys,
                    screenshot: e.currentTarget.value,
                  })
                }
              />
            </div>
            <div class="field">
              <label>Record GIF</label>
              <input
                type="text"
                value={c().hotkeys.record_gif}
                onInput={(e) =>
                  updateAnd("hotkeys", {
                    ...c().hotkeys,
                    record_gif: e.currentTarget.value,
                  })
                }
              />
            </div>

            <h2>Capture</h2>
            <div class="field">
              <label>Show cursor</label>
              <input
                type="checkbox"
                checked={c().capture.show_cursor}
                onChange={(e) =>
                  updateAnd("capture", {
                    ...c().capture,
                    show_cursor: e.currentTarget.checked,
                  })
                }
              />
            </div>
            <div class="field">
              <label>GIF FPS (1-60)</label>
              <input
                type="number"
                min={1}
                max={60}
                value={c().capture.gif_fps}
                onInput={(e) =>
                  updateAnd("capture", {
                    ...c().capture,
                    gif_fps: parseInt(e.currentTarget.value || "15"),
                  })
                }
              />
            </div>
            <div class="field">
              <label>GIF max duration (s)</label>
              <input
                type="number"
                min={1}
                max={300}
                value={c().capture.gif_max_duration_secs}
                onInput={(e) =>
                  updateAnd("capture", {
                    ...c().capture,
                    gif_max_duration_secs: parseInt(
                      e.currentTarget.value || "30",
                    ),
                  })
                }
              />
            </div>

            <h2>UI</h2>
            <div class="field">
              <label>Show OS notifications</label>
              <input
                type="checkbox"
                checked={c().ui.show_notifications}
                onChange={(e) =>
                  updateAnd("ui", {
                    ...c().ui,
                    show_notifications: e.currentTarget.checked,
                  })
                }
              />
            </div>
            <div class="field">
              <label>Play sound after capture</label>
              <input
                type="checkbox"
                checked={c().post_capture.play_sound}
                onChange={(e) =>
                  updateAnd("post_capture", {
                    ...c().post_capture,
                    play_sound: e.currentTarget.checked,
                  })
                }
              />
            </div>

            <div class="row" style="margin-top: 24px;">
              <button onClick={save} disabled={saving()}>
                {saving() ? "Saving..." : "Save settings"}
              </button>
              <span class="status">{status()}</span>
            </div>
          </>
        )}
      </Show>
    </>
  );
}
