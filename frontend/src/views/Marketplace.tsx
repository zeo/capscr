import { createResource, createSignal, For, Show } from "solid-js";
import { FolderOpen, RefreshCw, Power } from "lucide-solid";
import { Section } from "../components/Section";
import { api, InstalledPlugin } from "../api";

export function Marketplace() {
  const [plugins, { refetch }] = createResource<InstalledPlugin[]>(async () => {
    try {
      return await api.listInstalledPlugins();
    } catch {
      return [];
    }
  });
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(
    null,
  );

  const reload = async () => {
    setStatus({ tone: "", msg: "re-scanning..." });
    try {
      await refetch();
      setStatus({ tone: "ok", msg: "done." });
    } catch (e) {
      setStatus({ tone: "err", msg: `err: ${e}` });
    }
  };

  const openFolder = async () => {
    try {
      await api.openPluginsFolder();
    } catch (e) {
      setStatus({
        tone: "err",
        msg: `backend cmd 'open_plugins_folder' missing — wire it in rust.`,
      });
    }
  };

  return (
    <>
      <div class="view-head">
        <h1>plugins</h1>
        <span class="lede">drop a plugin dir into the folder, reload.</span>
      </div>

      <Section title="installed">
        <div class="row between" style="margin: 4px 0 14px;">
          <div class="btn-row">
            <button class="btn" onClick={openFolder}>
              <FolderOpen size={12} stroke-width={1.5} />
              open folder
            </button>
            <button class="btn" data-variant="ghost" onClick={reload}>
              <RefreshCw size={12} stroke-width={1.5} />
              reload
            </button>
          </div>
          <Show when={status()}>
            <span class="flash" data-tone={status()!.tone}>
              {status()!.msg}
            </span>
          </Show>
        </div>

        <Show
          when={(plugins() ?? []).length > 0}
          fallback={
            <div class="empty">
              <span class="stick" />
              none installed
              <p>
                drop a plugin dir into the folder, reload. format docs land with v0.4.0.
              </p>
            </div>
          }
        >
          <div class="list">
            <For each={plugins()!}>
              {(p) => (
                <div class="list-item">
                  <div class="list-item-body">
                    <div class="list-item-title">{p.name}</div>
                    <div class="list-item-meta">
                      <span>
                        <span class="k">v </span>
                        <span class="v">{p.version}</span>
                      </span>
                      <span>
                        <span class="k">status </span>
                        <span class="v">
                          <span
                            class="tag"
                            data-tone={p.enabled ? "on" : "off"}
                          >
                            {p.enabled ? "enabled" : "disabled"}
                          </span>
                        </span>
                      </span>
                    </div>
                    <Show when={p.description}>
                      <div
                        class="muted"
                        style="margin-top: 6px; font-size: 11px;"
                      >
                        {p.description}
                      </div>
                    </Show>
                  </div>
                  <div class="list-item-actions">
                    <button class="btn" data-variant="ghost" data-size="xs">
                      <Power size={11} stroke-width={1.5} />
                      {p.enabled ? "disable" : "enable"}
                    </button>
                  </div>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Section>

      <Section title="browse">
        <div class="empty">
          <span class="stick" />
          marketplace
          <p>
            curated registry from <code>github.com/lintowe/capscr-plugins</code>, lands with v0.4.0.
          </p>
        </div>
      </Section>
    </>
  );
}
