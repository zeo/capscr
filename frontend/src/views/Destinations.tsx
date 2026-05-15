import { createResource, createSignal, Show } from "solid-js";
import { api, AppConfig } from "../api";

export function Destinations() {
  const [config, { mutate }] = createResource<AppConfig>(api.getConfig);
  const [status, setStatus] = createSignal("");

  const save = async () => {
    const c = config();
    if (!c) return;
    setStatus("Saving...");
    try {
      await api.setConfig(c);
      setStatus("Saved");
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
  };

  return (
    <>
      <h1>Upload destinations</h1>
      <Show when={config()}>
        {(c) => (
          <>
            <div class="field">
              <label>Active destination</label>
              <select
                value={c().upload.destination}
                onChange={(e) =>
                  mutate({
                    ...c(),
                    upload: {
                      ...c().upload,
                      destination: e.currentTarget.value as never,
                    },
                  })
                }
              >
                <option value="Imgur">Imgur (anon)</option>
                <option value="Custom">Custom HTTP</option>
              </select>
            </div>
            <div class="field">
              <label>Copy URL to clipboard</label>
              <input
                type="checkbox"
                checked={c().upload.copy_url_to_clipboard}
                onChange={(e) =>
                  mutate({
                    ...c(),
                    upload: {
                      ...c().upload,
                      copy_url_to_clipboard: e.currentTarget.checked,
                    },
                  })
                }
              />
            </div>

            <h2>Custom HTTP</h2>
            <div class="field">
              <label>POST URL (HTTPS only)</label>
              <input
                type="text"
                value={c().upload.custom_url}
                onInput={(e) =>
                  mutate({
                    ...c(),
                    upload: {
                      ...c().upload,
                      custom_url: e.currentTarget.value,
                    },
                  })
                }
              />
            </div>
            <div class="field">
              <label>Form field name</label>
              <input
                type="text"
                value={c().upload.custom_form_name}
                onInput={(e) =>
                  mutate({
                    ...c(),
                    upload: {
                      ...c().upload,
                      custom_form_name: e.currentTarget.value,
                    },
                  })
                }
              />
            </div>
            <div class="field">
              <label>Response URL JSON path</label>
              <input
                type="text"
                value={c().upload.custom_response_path}
                onInput={(e) =>
                  mutate({
                    ...c(),
                    upload: {
                      ...c().upload,
                      custom_response_path: e.currentTarget.value,
                    },
                  })
                }
              />
            </div>

            <div class="row" style="margin-top: 24px;">
              <button onClick={save}>Save destinations</button>
              <span class="status">{status()}</span>
            </div>
          </>
        )}
      </Show>
    </>
  );
}
