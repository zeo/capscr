import { createMemo, createResource, createSignal, For, Show } from "solid-js";
import { FolderOpen, RefreshCw, Power, Download, Trash2, ExternalLink } from "lucide-solid";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Section } from "../components/Section";
import { api, InstalledPlugin, RegistryEntry } from "../api";

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

export function Marketplace() {
  const [plugins, { refetch: refetchInstalled }] = createResource<InstalledPlugin[]>(async () => {
    try {
      return await api.listInstalledPlugins();
    } catch {
      return [];
    }
  });
  const [registry, { refetch: refetchRegistry }] = createResource<RegistryEntry[]>(async () => {
    try {
      return await api.marketplaceBrowse();
    } catch (e) {
      throw e;
    }
  });
  const [loadErrors, { refetch: refetchErrors }] = createResource<string[]>(async () => {
    try {
      return await api.pluginLoadErrors();
    } catch {
      return [];
    }
  });
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(
    null,
  );
  const [busyId, setBusyId] = createSignal<string | null>(null);

  const reload = async () => {
    setStatus({ tone: "", msg: "re-scanning..." });
    try {
      await Promise.all([refetchInstalled(), refetchRegistry(), refetchErrors()]);
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
        msg: `couldn't open plugins folder: ${e}`,
      });
    }
  };

  const install = async (entry: RegistryEntry) => {
    setBusyId(entry.id);
    setStatus({ tone: "", msg: `installing ${entry.name}...` });
    try {
      await api.marketplaceInstall(entry.id);
      setStatus({ tone: "ok", msg: `installed ${entry.name} v${entry.version}.` });
      await refetchInstalled();
    } catch (e) {
      setStatus({ tone: "err", msg: `install failed: ${e}` });
    } finally {
      setBusyId(null);
    }
  };

  const toggleEnabled = async (entry: InstalledPlugin) => {
    setBusyId(entry.id);
    const next = !entry.enabled;
    setStatus({ tone: "", msg: `${next ? "enabling" : "disabling"} ${entry.name}...` });
    try {
      await api.togglePluginEnabled(entry.id, next);
      setStatus({ tone: "ok", msg: `${entry.name} ${next ? "enabled" : "disabled"}.` });
      await refetchInstalled();
    } catch (e) {
      setStatus({ tone: "err", msg: `toggle failed: ${e}` });
    } finally {
      setBusyId(null);
    }
  };

  const uninstall = async (entry: InstalledPlugin) => {
    if (!window.confirm(`Uninstall ${entry.name}? Plugin files will be deleted.`)) {
      return;
    }
    setBusyId(entry.id);
    setStatus({ tone: "", msg: `uninstalling ${entry.name}...` });
    try {
      await api.marketplaceUninstall(entry.id);
      setStatus({ tone: "ok", msg: `removed ${entry.name}.` });
      await refetchInstalled();
    } catch (e) {
      setStatus({ tone: "err", msg: `uninstall failed: ${e}` });
    } finally {
      setBusyId(null);
    }
  };

  // index installed plugins by id so the registry list does O(1) lookups
  // instead of an O(installed) scan per browse entry on every render
  const installedById = createMemo(() => {
    const map = new Map<string, InstalledPlugin>();
    for (const p of plugins() ?? []) map.set(p.id, p);
    return map;
  });
  const isInstalled = (entryId: string): InstalledPlugin | undefined =>
    installedById().get(entryId);

  return (
    <>
      <div class="view-head">
        <h1>plugins</h1>
        <span class="lede">browse rot.lt registry · drop plugin dirs into the folder.</span>
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

        <Show when={(loadErrors() ?? []).length > 0}>
          <div
            class="flash"
            data-tone="err"
            style="display: block; margin: 0 0 14px; white-space: normal;"
          >
            <strong>{(loadErrors() ?? []).length} plugin(s) failed to load</strong>
            <ul style="margin: 6px 0 0; padding-left: 18px;">
              <For each={loadErrors()!}>{(err) => <li>{err}</li>}</For>
            </ul>
            <div class="muted" style="margin-top: 6px;">
              load runs at launch — restart capscr after fixing a plugin.
            </div>
          </div>
        </Show>

        <Show
          when={plugins.loading || (plugins() ?? []).length > 0}
          fallback={
            <div class="empty">
              <span class="stick" />
              none installed
              <p>
                browse below, or drop a plugin dir into the plugins folder and reload.
              </p>
            </div>
          }
        >
          <div class="list">
            <For each={plugins()!}>
              {(p) => {
                return (
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
                      <button
                        class="btn"
                        data-variant="ghost"
                        data-size="xs"
                        disabled={busyId() !== null}
                        onClick={() => toggleEnabled(p)}
                      >
                        <Power size={11} stroke-width={1.5} />
                        {p.enabled ? "disable" : "enable"}
                      </button>
                      <button
                        class="btn"
                        data-variant="ghost"
                        data-size="xs"
                        disabled={busyId() !== null}
                        onClick={() => uninstall(p)}
                      >
                        <Trash2 size={11} stroke-width={1.5} />
                        uninstall
                      </button>
                    </div>
                  </div>
                );
              }}
            </For>
          </div>
        </Show>
      </Section>

      <Section title="browse">
        <Show
          when={!registry.loading}
          fallback={
            <div class="skeleton">
              <div class="skeleton-line" style="width: 50%;" />
              <div class="skeleton-line" style="width: 70%;" />
              <div class="skeleton-line" style="width: 60%;" />
            </div>
          }
        >
          <Show
            when={registry.error}
            fallback={
              <Show
                when={(registry() ?? []).length > 0}
                fallback={
                  <div class="empty">
                    <span class="stick" />
                    empty registry
                    <p>
                      there are no plugins to install yet — the plugin
                      runtime (event hooks, wasm host) is live as of v0.4.
                    </p>
                  </div>
                }
              >
                <div class="list">
                  <For each={registry()!}>
                    {(entry) => {
                      const installed = () => isInstalled(entry.id);
                      const upToDate = () =>
                        installed()?.version === entry.version;
                      return (
                        <div class="list-item">
                          <div class="list-item-body">
                            <div class="list-item-title">{entry.name}</div>
                            <div class="list-item-meta">
                              <span>
                                <span class="k">v </span>
                                <span class="v">{entry.version}</span>
                              </span>
                              <Show when={entry.author}>
                                <span>
                                  <span class="k">by </span>
                                  <span class="v">{entry.author}</span>
                                </span>
                              </Show>
                              <span>
                                <span class="k">size </span>
                                <span class="v">{formatSize(entry.size_bytes)}</span>
                              </span>
                              <Show when={entry.license}>
                                <span>
                                  <span class="k">license </span>
                                  <span class="v">{entry.license}</span>
                                </span>
                              </Show>
                            </div>
                            <Show when={entry.description}>
                              <div
                                class="muted"
                                style="margin-top: 6px; font-size: 11px;"
                              >
                                {entry.description}
                              </div>
                            </Show>
                            <Show when={entry.tags.length > 0}>
                              <div style="margin-top: 6px;">
                                <For each={entry.tags}>
                                  {(tag) => (
                                    <span
                                      class="tag"
                                      data-tone="off"
                                      style="margin-right: 4px;"
                                    >
                                      {tag}
                                    </span>
                                  )}
                                </For>
                              </div>
                            </Show>
                          </div>
                          <div class="list-item-actions">
                            <Show when={entry.homepage}>
                              <button
                                class="btn"
                                data-variant="ghost"
                                data-size="xs"
                                onClick={() => void openUrl(entry.homepage)}
                                title={entry.homepage}
                              >
                                <ExternalLink size={11} stroke-width={1.5} />
                                site
                              </button>
                            </Show>
                            <Show
                              when={!installed() || !upToDate()}
                              fallback={
                                <span class="tag" data-tone="on">
                                  installed
                                </span>
                              }
                            >
                              <button
                                class="btn"
                                data-size="xs"
                                disabled={busyId() !== null}
                                onClick={() => install(entry)}
                              >
                                <Download size={11} stroke-width={1.5} />
                                {busyId() === entry.id
                                  ? "..."
                                  : installed()
                                  ? `update to v${entry.version}`
                                  : "install"}
                              </button>
                            </Show>
                          </div>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </Show>
            }
          >
            <div class="empty">
              <span class="stick" />
              registry unreachable
              <p>
                {String(registry.error)}
              </p>
              <p class="muted" style="margin-top: 8px; font-size: 11px;">
                check Settings → destinations to point at a mirror, or wait for rot.lt to come back.
              </p>
            </div>
          </Show>
        </Show>
      </Section>
    </>
  );
}
