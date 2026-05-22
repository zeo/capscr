import { createResource, createSignal, Show } from "solid-js";
import { Save, FolderOpen } from "lucide-solid";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Section } from "../components/Section";
import { api, AppConfig } from "../api";
import { configDirty, setConfigDirty } from "../dirty";

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
        <span class="lede">where uploads go.</span>
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
                    <option value="Ftp">ftp / ftps</option>
                    <option value="Sftp">sftp (ssh)</option>
                  </select>
                </div>
              </div>
              <Show when={c().upload.destination === "Imgur"}>
                <div class="field">
                  <label class="field-label">imgur client-id</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="546c25a59c58ad7"
                      value={c().upload.imgur_client_id}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          imgur_client_id: e.currentTarget.value,
                        })
                      }
                    />
                    <span class="field-hint">
                      leave blank for capscr's shared key; paste your own from
                      api.imgur.com to avoid rate-limit pile-ups.
                    </span>
                  </div>
                </div>
              </Show>
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

            <Show when={c().upload.destination === "Ftp"}>
              <Section title="ftp / ftps">
                <div class="field">
                  <label class="field-label">host</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="ftp.example.com"
                      value={c().upload.ftp.host}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          ftp: { ...c().upload.ftp, host: e.currentTarget.value },
                        })
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">port</label>
                  <div class="field-control">
                    <input
                      type="number"
                      min={1}
                      max={65535}
                      value={c().upload.ftp.port}
                      onInput={(e) => {
                        const v = parseInt(e.currentTarget.value);
                        if (!isNaN(v) && v >= 1 && v <= 65535) {
                          patch({
                            ...c().upload,
                            ftp: { ...c().upload.ftp, port: v },
                          });
                        }
                      }}
                    />
                    <span class="field-hint">21 plain, 990 implicit tls</span>
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">username</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="anonymous"
                      value={c().upload.ftp.username}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          ftp: { ...c().upload.ftp, username: e.currentTarget.value },
                        })
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">password</label>
                  <div class="field-control">
                    <input
                      type="password"
                      placeholder={
                        c().upload.ftp.password_encrypted
                          ? "(stored — leave blank to keep current)"
                          : ""
                      }
                      value={c().upload.ftp.password}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          ftp: { ...c().upload.ftp, password: e.currentTarget.value },
                        })
                      }
                    />
                    <span class="field-hint">
                      {c().upload.ftp.password_encrypted
                        ? "encrypted at rest with Windows DPAPI (per-user)"
                        : "encrypted at rest with Windows DPAPI on save"}
                    </span>
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">remote dir</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="/uploads"
                      value={c().upload.ftp.remote_dir}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          ftp: { ...c().upload.ftp, remote_dir: e.currentTarget.value },
                        })
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">use tls</label>
                  <div class="field-control">
                    <label class="check">
                      <input
                        type="checkbox"
                        checked={c().upload.ftp.use_tls}
                        disabled
                        onChange={(e) =>
                          patch({
                            ...c().upload,
                            ftp: { ...c().upload.ftp, use_tls: e.currentTarget.checked },
                          })
                        }
                      />
                      <span class="check-label">plain ftp only — ftps planned for v0.4</span>
                    </label>
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">public url template</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="https://cdn.example.com/{filename}"
                      value={c().upload.ftp.public_url_template}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          ftp: {
                            ...c().upload.ftp,
                            public_url_template: e.currentTarget.value,
                          },
                        })
                      }
                    />
                    <span class="field-hint">
                      {`{filename} → basename, empty = no url returned`}
                    </span>
                  </div>
                </div>
              </Section>
            </Show>

            <Show when={c().upload.destination === "Sftp"}>
              <Section title="sftp (ssh)">
                <div class="field">
                  <label class="field-label">host</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="sftp.example.com"
                      value={c().upload.sftp.host}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          sftp: { ...c().upload.sftp, host: e.currentTarget.value },
                        })
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">port</label>
                  <div class="field-control">
                    <input
                      type="number"
                      min={1}
                      max={65535}
                      value={c().upload.sftp.port}
                      onInput={(e) => {
                        const v = parseInt(e.currentTarget.value);
                        if (!isNaN(v) && v >= 1 && v <= 65535) {
                          patch({
                            ...c().upload,
                            sftp: { ...c().upload.sftp, port: v },
                          });
                        }
                      }}
                    />
                    <span class="field-hint">22 standard ssh</span>
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">username</label>
                  <div class="field-control">
                    <input
                      type="text"
                      value={c().upload.sftp.username}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          sftp: { ...c().upload.sftp, username: e.currentTarget.value },
                        })
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">private key</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="C:\Users\you\.ssh\id_ed25519 (blank = password auth)"
                      value={c().upload.sftp.private_key_path}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          sftp: { ...c().upload.sftp, private_key_path: e.currentTarget.value },
                        })
                      }
                    />
                    <button
                      class="btn"
                      data-variant="ghost"
                      data-size="xs"
                      onClick={async () => {
                        const picked = await openDialog({
                          multiple: false,
                          directory: false,
                          filters: [
                            { name: "OpenSSH key", extensions: ["pem", "key", ""] },
                          ],
                        });
                        if (typeof picked === "string") {
                          patch({
                            ...c().upload,
                            sftp: { ...c().upload.sftp, private_key_path: picked },
                          });
                        }
                      }}
                    >
                      <FolderOpen size={11} stroke-width={1.5} />
                      browse
                    </button>
                    <span class="field-hint">
                      openssh format. ed25519 / rsa / ecdsa supported.
                    </span>
                  </div>
                </div>
                <Show when={c().upload.sftp.private_key_path}>
                  <div class="field">
                    <label class="field-label">key passphrase</label>
                    <div class="field-control">
                      <input
                        type="password"
                        placeholder={
                          c().upload.sftp.private_key_passphrase_encrypted
                            ? "(stored — leave blank to keep current)"
                            : "leave blank if the key is unencrypted"
                        }
                        value={c().upload.sftp.private_key_passphrase}
                        onInput={(e) =>
                          patch({
                            ...c().upload,
                            sftp: { ...c().upload.sftp, private_key_passphrase: e.currentTarget.value },
                          })
                        }
                      />
                      <span class="field-hint">
                        encrypted at rest with Windows DPAPI on save.
                      </span>
                    </div>
                  </div>
                </Show>
                <div class="field">
                  <label class="field-label">password</label>
                  <div class="field-control">
                    <input
                      type="password"
                      placeholder={
                        c().upload.sftp.password_encrypted
                          ? "(stored — leave blank to keep current)"
                          : "fallback if key auth fails or no key set"
                      }
                      value={c().upload.sftp.password}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          sftp: { ...c().upload.sftp, password: e.currentTarget.value },
                        })
                      }
                    />
                    <span class="field-hint">
                      {c().upload.sftp.password_encrypted
                        ? "encrypted at rest with Windows DPAPI (per-user)"
                        : "encrypted at rest with Windows DPAPI on save"}
                    </span>
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">remote dir</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="/var/www/uploads"
                      value={c().upload.sftp.remote_dir}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          sftp: { ...c().upload.sftp, remote_dir: e.currentTarget.value },
                        })
                      }
                    />
                  </div>
                </div>
                <div class="field">
                  <label class="field-label">public url template</label>
                  <div class="field-control">
                    <input
                      type="text"
                      placeholder="https://cdn.example.com/{filename}"
                      value={c().upload.sftp.public_url_template}
                      onInput={(e) =>
                        patch({
                          ...c().upload,
                          sftp: {
                            ...c().upload.sftp,
                            public_url_template: e.currentTarget.value,
                          },
                        })
                      }
                    />
                    <span class="field-hint">
                      {`{filename} → basename, empty = no url returned`}
                    </span>
                  </div>
                </div>
              </Section>
            </Show>

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
              <button
                class="btn"
                data-variant={configDirty() ? "primary" : undefined}
                onClick={save}
                disabled={!configDirty()}
                title={configDirty() ? "commit pending changes" : "no changes to save"}
              >
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
