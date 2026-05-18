import { createResource, createSignal, Show } from "solid-js";
import { Save } from "lucide-solid";
import { Section } from "../components/Section";
import { api, AppConfig } from "../api";
import { setConfigDirty } from "../dirty";

export function Destinations() {
  const [config, { mutate }] = createResource<AppConfig>(api.getConfig);
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(
    null,
  );

  const save = async () => {
    const c = config();
    if (!c) return;
    setStatus({ tone: "", msg: "writing..." });
    try {
      await api.setConfig(c);
      setStatus({ tone: "ok", msg: "saved." });
      setConfigDirty(false);
    } catch (e) {
      setStatus({ tone: "err", msg: `err: ${e}` });
    }
  };

  const patch = (next: AppConfig["upload"]) => {
    const c = config();
    if (!c) return;
    mutate({ ...c, upload: next });
    setConfigDirty(true);
  };

  return (
    <>
      <div class="view-head">
        <h1>destinations</h1>
        <span class="lede">where uploads go. https only.</span>
      </div>

      <Show
        when={config()}
        fallback={
          <div class="skeleton">
            <div class="skeleton-line" style="width: 40%;" />
            <div class="skeleton-line" style="width: 70%;" />
            <div class="skeleton-line" style="width: 55%;" />
          </div>
        }
      >
        {(c) => (
          <>
            <Section title="active target">
              <div class="field">
                <label class="field-label">target</label>
                <div class="field-control">
                  <select
                    value={c().upload.destination}
                    onChange={(e) =>
                      patch({
                        ...c().upload,
                        destination: e.currentTarget.value as never,
                      })
                    }
                  >
                    <option value="Imgur">imgur (anonymous)</option>
                    <option value="Custom">custom http</option>
                  </select>
                </div>
              </div>
              <div class="field">
                <label class="field-label">copy url to clipboard</label>
                <div class="field-control">
                  <label class="check">
                    <input
                      type="checkbox"
                      checked={c().upload.copy_url_to_clipboard}
                      onChange={(e) =>
                        patch({
                          ...c().upload,
                          copy_url_to_clipboard: e.currentTarget.checked,
                        })
                      }
                    />
                    <span class="check-label">
                      {c().upload.copy_url_to_clipboard
                        ? "auto-copy on success"
                        : "leave clipboard alone"}
                    </span>
                  </label>
                </div>
              </div>
            </Section>

            <Section title="custom http">
              <div class="field">
                <label class="field-label">post url</label>
                <div class="field-control">
                  <input
                    type="text"
                    placeholder="https://i.your-server.example/upload"
                    value={c().upload.custom_url}
                    onInput={(e) =>
                      patch({
                        ...c().upload,
                        custom_url: e.currentTarget.value,
                      })
                    }
                  />
                  <span class="field-hint">https only, plain http rejected</span>
                </div>
              </div>
              <div class="field">
                <label class="field-label">form field</label>
                <div class="field-control">
                  <input
                    type="text"
                    placeholder="file"
                    value={c().upload.custom_form_name}
                    onInput={(e) =>
                      patch({
                        ...c().upload,
                        custom_form_name: e.currentTarget.value,
                      })
                    }
                  />
                  <span class="field-hint">multipart key (often "file")</span>
                </div>
              </div>
              <div class="field">
                <label class="field-label">response path</label>
                <div class="field-control">
                  <input
                    type="text"
                    placeholder="data.link"
                    value={c().upload.custom_response_path}
                    onInput={(e) =>
                      patch({
                        ...c().upload,
                        custom_response_path: e.currentTarget.value,
                      })
                    }
                  />
                  <span class="field-hint">
                    dotted json path to the url, empty = raw body
                  </span>
                </div>
              </div>
            </Section>

            <hr class="rule" />
            <div class="btn-row right">
              <Show when={status()}>
                <span class="flash" data-tone={status()!.tone}>
                  {status()!.msg}
                </span>
              </Show>
              <button class="btn" onClick={save}>
                <Save size={12} stroke-width={1.5} />
                save
              </button>
            </div>
          </>
        )}
      </Show>
    </>
  );
}
