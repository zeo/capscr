// wasmtime-backed plugin host. Compiled only with the `plugin-runtime`
// feature. The default capscr build skips this entire module and falls back
// to the metadata-only PluginManager from 0.3.x.
//
// host API exposed to plugins (capscr.* module imports):
//   capscr.log(level: i32, msg_ptr: i32, msg_len: i32)
//     level: 0 = error, 1 = warn, 2 = info, 3 = debug
//     msg_*: pointer + length into the plugin's linear memory
//
// hook entry points (exported by the plugin, called by the host):
//   capscr_on_capture_saved(path_ptr: i32, path_len: i32)
//   capscr_on_upload_success(url_ptr: i32, url_len: i32)
//
// pointer + length pairs index into the plugin's linear memory. Strings are
// UTF-8. The host writes hook payloads into a region allocated by the plugin
// via the exported `capscr_alloc(size: i32) -> i32` function — plugins that
// don't export `capscr_alloc` are skipped for hooks that need a payload.

use super::manifest::PluginManifest;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use wasmtime::{AsContextMut, Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

pub struct WasmPlugin {
    pub id: String,
    store: Mutex<Store<HostState>>,
    instance: Instance,
    memory: Memory,
    /// exported `capscr_alloc(size) -> ptr`; None if the plugin doesn't export it
    alloc: Option<TypedFunc<i32, i32>>,
    /// optional hooks resolved at load time, indexed by hook name
    hooks: HashMap<String, TypedFunc<(i32, i32), ()>>,
}

/// per-instantiation state threaded through every host import via Caller::data.
/// holds the plugin id (for log routing), the granted capabilities (enforced by
/// the clipboard/notify/fetch imports), and the per-hook epoch budget (so the
/// fetch import can refresh it after a blocking network call).
struct HostState {
    plugin_id: String,
    caps: Capabilities,
    deadline_ticks: u64,
    /// instant after which fetch is refused for the current hook call. set by
    /// call_hook before each invocation; None outside a hook (fetch denied)
    fetch_deadline: Option<std::time::Instant>,
}

/// capabilities a plugin declared in its `[capabilities]` manifest table,
/// resolved to the concrete grants the host enforces today.
#[derive(Default)]
struct Capabilities {
    clipboard_write: bool,
    notifications_show: bool,
    /// allowed fetch URL patterns; a trailing `*` is a prefix wildcard
    fetch_allow: Vec<String>,
}

impl Capabilities {
    fn from_manifest(caps: &HashMap<String, Vec<String>>) -> Self {
        let granted = |key: &str, val: &str| {
            caps.get(key)
                .map(|v| v.iter().any(|s| s == val))
                .unwrap_or(false)
        };
        Capabilities {
            clipboard_write: granted("clipboard", "write"),
            notifications_show: granted("notifications", "show"),
            fetch_allow: caps.get("fetch").cloned().unwrap_or_default(),
        }
    }

    fn fetch_allowed(&self, url: &str) -> bool {
        self.fetch_allow
            .iter()
            .any(|pattern| url_pattern_matches(pattern, url))
    }
}

/// trailing-`*` prefix wildcard, otherwise exact match. deliberately simple and
/// predictable — no regex, no path-segment globbing
fn url_pattern_matches(pattern: &str, url: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => url.starts_with(prefix),
        None => url == pattern,
    }
}

// host-import return codes (i32). fetch uses a packed i64 instead (see below)
const HOST_OK: i32 = 0;
const HOST_ERR_DENIED: i32 = -1; // capability not granted in the manifest
const HOST_ERR_ARGS: i32 = -2; // ptr/len out of bounds or not valid utf-8
const HOST_ERR_FAILED: i32 = -3; // the host-side operation itself failed

/// wall-clock cap on a single plugin fetch. epoch interruption does not fire
/// inside a blocking host call, so this is what actually bounds one call
const FETCH_TIMEOUT_SECS: u64 = 10;
/// aggregate wall-clock budget for *all* fetches within a single hook call. the
/// fetch import refreshes the epoch deadline after each blocking call (so a
/// legitimate slow fetch isn't trapped on resume), which removes the per-hook
/// epoch backstop — this budget is what re-bounds a fetch loop so a plugin can't
/// hold the dispatch thread (and the plugin-manager lock) indefinitely
const FETCH_HOOK_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
/// hard cap on a fetched response body so a plugin can't exhaust host memory
const FETCH_MAX_BYTES: usize = 1 << 20;
/// ports a plugin fetch may not target, mirroring the custom-upload destination
/// guard — non-web services where even a refused https probe leaks reachability
const FETCH_BLOCKED_PORTS: &[u16] = &[0, 22, 23, 25, 110, 143, 445, 3306, 3389, 5432, 6379, 27017];

pub struct WasmHost {
    engine: Engine,
}

/// default fuel budget per hook call (~10ms of cranelift-compiled code on
/// commodity hardware). manifest's `time_slice_ms` overrides — we treat
/// the ms value as fuel units * a calibration factor.
const DEFAULT_HOOK_FUEL: u64 = 5_000_000;
/// epoch ticks the host advances between hook calls. one tick = ~10ms on
/// the bumper thread below
const HOOK_EPOCH_DEADLINE: u64 = 1;

impl WasmHost {
    pub fn new() -> Result<Self> {
        let mut cfg = wasmtime::Config::new();
        // fuel + epoch interruption together bound the time a malicious or
        // buggy plugin can spend in a single hook call. fuel catches tight
        // loops; epoch catches `loop {}`-style stalls inside host imports
        cfg.consume_fuel(true);
        cfg.epoch_interruption(true);
        // we don't enable the `component-model` cargo feature on the
        // wasmtime crate, so component model is already off. reference
        // types and threads are off by default in this configuration; we
        // rely on the feature-gate defaults rather than calling the
        // (removed-in-v43) wasm_threads / wasm_reference_types setters

        let engine = Engine::new(&cfg)
            .map_err(|e| anyhow!("wasmtime engine init: {e}"))?;

        // background bumper: increments the engine epoch every 10ms so
        // plugins that exceed their per-hook deadline trap promptly. one
        // bumper covers every plugin sharing this engine.
        let engine_clone = engine.clone();
        std::thread::Builder::new()
            .name("capscr-wasm-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
                engine_clone.increment_epoch();
            })
            .ok();

        Ok(Self { engine })
    }

    pub fn load(
        &self,
        plugin_dir: &Path,
        manifest: &PluginManifest,
    ) -> Result<WasmPlugin> {
        let runtime = manifest
            .runtime
            .as_ref()
            .ok_or_else(|| anyhow!("plugin has no [runtime] section"))?;
        let wasm_path = plugin_dir.join(&runtime.file);
        let module = Module::from_file(&self.engine, &wasm_path)
            .map_err(|e| anyhow!("compiling {}: {e}", wasm_path.display()))?;

        let deadline_ticks = runtime
            .time_slice_ms
            .map(|ms| (ms / 10).max(1))
            .unwrap_or(50);

        let mut store = Store::new(
            &self.engine,
            HostState {
                plugin_id: manifest.plugin.id.clone(),
                caps: Capabilities::from_manifest(&manifest.capabilities),
                deadline_ticks,
                fetch_deadline: None,
            },
        );
        if let Some(limit) = runtime.memory_max_bytes {
            store.limiter(move |_| {
                Box::leak(Box::new(MemLimiter { cap: limit })) as &mut dyn wasmtime::ResourceLimiter
            });
        }
        // trap when the epoch deadline is exceeded — bumper thread advances
        // the engine epoch every 10ms, so HOOK_EPOCH_DEADLINE ticks ≈ 10ms.
        // call_hook bumps the deadline up to time_slice_ms / 10 before each
        // invocation; this Store::set_epoch_deadline only sets the initial
        // value before any hook fires
        store.set_epoch_deadline(HOOK_EPOCH_DEADLINE);
        store.epoch_deadline_trap();
        let mut linker: Linker<HostState> = Linker::new(&self.engine);

        // host import: capscr.log(level, ptr, len)
        linker
            .func_wrap(
                "capscr",
                "log",
                |mut caller: Caller<'_, HostState>, level: i32, ptr: i32, len: i32| {
                    let id = caller.data().plugin_id.clone();
                    let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return,
                    };
                    let data = mem.data(&caller);
                    let start = ptr as usize;
                    let end = start.saturating_add(len as usize);
                    let msg = if end <= data.len() {
                        std::str::from_utf8(&data[start..end]).unwrap_or("<utf-8 err>").to_string()
                    } else {
                        String::from("<oob log>")
                    };
                    match level {
                        0 => tracing::error!(plugin = %id, "{msg}"),
                        1 => tracing::warn!(plugin = %id, "{msg}"),
                        2 => tracing::info!(plugin = %id, "{msg}"),
                        _ => tracing::debug!(plugin = %id, "{msg}"),
                    }
                },
            )
            .map_err(|e| anyhow!("link capscr.log: {e}"))?;

        // host import: capscr.clipboard_write_text(ptr, len) -> i32
        // gated on the `clipboard = ["write"]` capability
        linker
            .func_wrap(
                "capscr",
                "clipboard_write_text",
                |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                    if !caller.data().caps.clipboard_write {
                        tracing::warn!(
                            plugin = %caller.data().plugin_id,
                            "clipboard_write_text denied: missing clipboard:write capability"
                        );
                        return HOST_ERR_DENIED;
                    }
                    let text = match read_guest_str(&mut caller, ptr, len) {
                        Some(s) => s,
                        None => return HOST_ERR_ARGS,
                    };
                    match crate::clipboard::ClipboardManager::new()
                        .and_then(|mut c| c.copy_text(&text))
                    {
                        Ok(()) => HOST_OK,
                        Err(e) => {
                            tracing::warn!(plugin = %caller.data().plugin_id, "clipboard_write_text failed: {e}");
                            HOST_ERR_FAILED
                        }
                    }
                },
            )
            .map_err(|e| anyhow!("link capscr.clipboard_write_text: {e}"))?;

        // host import: capscr.notify(title_ptr, title_len, body_ptr, body_len) -> i32
        // gated on the `notifications = ["show"]` capability
        linker
            .func_wrap(
                "capscr",
                "notify",
                |mut caller: Caller<'_, HostState>,
                 title_ptr: i32,
                 title_len: i32,
                 body_ptr: i32,
                 body_len: i32|
                 -> i32 {
                    if !caller.data().caps.notifications_show {
                        tracing::warn!(
                            plugin = %caller.data().plugin_id,
                            "notify denied: missing notifications:show capability"
                        );
                        return HOST_ERR_DENIED;
                    }
                    let title = match read_guest_str(&mut caller, title_ptr, title_len) {
                        Some(s) => s,
                        None => return HOST_ERR_ARGS,
                    };
                    let body = match read_guest_str(&mut caller, body_ptr, body_len) {
                        Some(s) => s,
                        None => return HOST_ERR_ARGS,
                    };
                    match crate::clipboard::show_notification(&title, &body) {
                        Ok(()) => HOST_OK,
                        Err(e) => {
                            tracing::warn!(plugin = %caller.data().plugin_id, "notify failed: {e}");
                            HOST_ERR_FAILED
                        }
                    }
                },
            )
            .map_err(|e| anyhow!("link capscr.notify: {e}"))?;

        // host import: capscr.fetch(url_ptr, url_len) -> i64
        // performs a blocking HTTP(S) GET and writes the response body into guest
        // memory via the plugin's capscr_alloc, returning a packed pointer/length
        // (ptr << 32 | len). returns 0 on any failure or denial. gated on the
        // `fetch = [...patterns...]` capability and guarded against SSRF.
        linker
            .func_wrap(
                "capscr",
                "fetch",
                |mut caller: Caller<'_, HostState>, url_ptr: i32, url_len: i32| -> i64 {
                    let url = match read_guest_str(&mut caller, url_ptr, url_len) {
                        Some(s) => s,
                        None => return 0,
                    };
                    if !caller.data().caps.fetch_allowed(&url) {
                        tracing::warn!(
                            plugin = %caller.data().plugin_id,
                            "fetch denied: {url} not in declared fetch capability"
                        );
                        return 0;
                    }
                    // enforce the aggregate per-hook fetch budget, and bound this
                    // single call to whatever budget remains (capped at the
                    // per-call timeout). without this a fetch loop could block the
                    // dispatch thread indefinitely, since we refresh the epoch
                    // deadline after each call below
                    let remaining = caller
                        .data()
                        .fetch_deadline
                        .map(|dl| dl.saturating_duration_since(std::time::Instant::now()))
                        .unwrap_or_default();
                    if remaining.is_zero() {
                        tracing::warn!(
                            plugin = %caller.data().plugin_id,
                            "fetch denied: per-hook fetch time budget exhausted"
                        );
                        return 0;
                    }
                    let timeout =
                        remaining.min(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS));
                    let body = match host_fetch(&url, timeout) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(plugin = %caller.data().plugin_id, "fetch {url} failed: {e}");
                            return 0;
                        }
                    };
                    // the blocking GET may have spent seconds; epoch interruption
                    // doesn't fire inside host calls, so refresh the budget here so
                    // the plugin isn't trapped the instant it resumes to read the body
                    let deadline = caller.data().deadline_ticks;
                    caller.as_context_mut().set_epoch_deadline(deadline);

                    let len = body.len();
                    if len > i32::MAX as usize {
                        return 0;
                    }
                    let alloc = match caller
                        .get_export("capscr_alloc")
                        .and_then(|e| e.into_func())
                    {
                        Some(f) => f,
                        None => return 0,
                    };
                    let alloc = match alloc.typed::<i32, i32>(&caller) {
                        Ok(f) => f,
                        Err(_) => return 0,
                    };
                    let ptr = match alloc.call(&mut caller, len as i32) {
                        Ok(p) => p,
                        Err(_) => return 0,
                    };
                    if ptr <= 0 {
                        return 0;
                    }
                    let mem = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return 0,
                    };
                    if mem.write(&mut caller, ptr as usize, &body).is_err() {
                        return 0;
                    }
                    ((ptr as i64) << 32) | (len as i64 & 0xffff_ffff)
                },
            )
            .map_err(|e| anyhow!("link capscr.fetch: {e}"))?;

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| anyhow!("instantiating: {e}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow!("plugin must export `memory`"))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "capscr_alloc")
            .ok();

        let mut hooks = std::collections::HashMap::new();
        for (hook_name, export_name) in &manifest.hooks {
            match instance.get_typed_func::<(i32, i32), ()>(&mut store, export_name) {
                Ok(f) => {
                    hooks.insert(hook_name.clone(), f);
                }
                Err(e) => {
                    tracing::warn!(
                        "plugin '{}' hook '{}' missing export '{}': {e}",
                        manifest.plugin.id,
                        hook_name,
                        export_name
                    );
                }
            }
        }

        Ok(WasmPlugin {
            id: manifest.plugin.id.clone(),
            store: Mutex::new(store),
            instance,
            memory,
            alloc,
            hooks,
        })
    }
}

impl WasmPlugin {
    pub fn call_hook(&self, name: &str, payload: &str) -> Result<()> {
        let hook = match self.hooks.get(name) {
            Some(h) => h,
            None => return Ok(()), // plugin opted out of this hook
        };
        let alloc = self
            .alloc
            .as_ref()
            .ok_or_else(|| anyhow!("plugin '{}' has no capscr_alloc export", self.id))?;

        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("plugin '{}' store poisoned", self.id))?;

        // refresh per-hook budgets before each call so a plugin that exhausted
        // fuel or epoch ticks in a previous hook gets a fresh deadline
        let _ = store.set_fuel(DEFAULT_HOOK_FUEL);
        let deadline = store.data().deadline_ticks;
        store.set_epoch_deadline(deadline);
        // start the aggregate fetch budget for this hook invocation
        store.data_mut().fetch_deadline = Some(std::time::Instant::now() + FETCH_HOOK_BUDGET);

        let bytes = payload.as_bytes();
        let len = bytes.len() as i32;
        let ptr = alloc
            .call(&mut *store, len)
            .map_err(|e| anyhow!("capscr_alloc({len}): {e}"))?;
        if ptr <= 0 {
            return Err(anyhow!("capscr_alloc returned {ptr} (out of memory?)"));
        }
        self.memory
            .write(&mut *store, ptr as usize, bytes)
            .map_err(|e| anyhow!("memory write: {e}"))?;
        hook.call(&mut *store, (ptr, len))
            .map_err(|e| anyhow!("hook '{name}' trapped: {e}"))?;
        Ok(())
    }
}

struct MemLimiter {
    cap: usize,
}

impl wasmtime::ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= self.cap)
    }
    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 65536)
    }
}

/// shared host instance. Created lazily on first PluginManager::load_all so
/// the engine compile cost is paid only when at least one plugin exists.
pub type SharedWasmHost = Arc<WasmHost>;

/// read a UTF-8 string out of the guest's linear memory at (ptr, len).
/// returns None on a negative pointer/length, an out-of-bounds range, a missing
/// `memory` export, or invalid utf-8 — callers map that to HOST_ERR_ARGS
fn read_guest_str(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Option<String> {
    if ptr < 0 || len < 0 {
        return None;
    }
    let mem = caller.get_export("memory").and_then(|e| e.into_memory())?;
    let data = mem.data(&caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    if end > data.len() {
        return None;
    }
    std::str::from_utf8(&data[start..end]).ok().map(str::to_owned)
}

/// blocking HTTP(S) GET behind the same SSRF guard the upload path uses.
/// redirects are disabled so a 30x to a private IP can't slip past the initial
/// host check; the body is capped at FETCH_MAX_BYTES and the call at `timeout`
fn host_fetch(url: &str, timeout: std::time::Duration) -> Result<Vec<u8>> {
    use std::io::Read;

    let parsed = url::Url::parse(url).map_err(|e| anyhow!("bad url: {e}"))?;
    // https only, matching the custom-upload destination — cleartext http is
    // MITM-able and a plugin's response drives host actions
    if parsed.scheme() != "https" {
        return Err(anyhow!("only https is allowed (got {})", parsed.scheme()));
    }
    let host = parsed.host_str().ok_or_else(|| anyhow!("url has no host"))?;
    let port = parsed.port().unwrap_or(443);
    if FETCH_BLOCKED_PORTS.contains(&port) {
        return Err(anyhow!("port {port} is blocked"));
    }
    crate::upload::validate_resolved_host(host, port)?;

    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let resp = client.get(url).send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("http status {}", resp.status()));
    }
    let mut buf = Vec::new();
    resp.take(FETCH_MAX_BYTES as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_pattern_exact_and_wildcard() {
        assert!(url_pattern_matches(
            "https://api.example.com/v1",
            "https://api.example.com/v1"
        ));
        assert!(!url_pattern_matches(
            "https://api.example.com/v1",
            "https://api.example.com/v2"
        ));
        assert!(url_pattern_matches(
            "https://api.example.com/*",
            "https://api.example.com/anything/here"
        ));
        assert!(!url_pattern_matches(
            "https://api.example.com/*",
            "https://evil.example.com/"
        ));
    }

    #[test]
    fn capabilities_resolve_from_manifest() {
        let mut raw: HashMap<String, Vec<String>> = HashMap::new();
        raw.insert("clipboard".into(), vec!["read".into(), "write".into()]);
        raw.insert("notifications".into(), vec!["show".into()]);
        raw.insert("fetch".into(), vec!["https://api.example.com/*".into()]);
        let caps = Capabilities::from_manifest(&raw);
        assert!(caps.clipboard_write);
        assert!(caps.notifications_show);
        assert!(caps.fetch_allowed("https://api.example.com/data"));
        assert!(!caps.fetch_allowed("https://other.example.com/data"));
    }

    #[test]
    fn capabilities_default_deny() {
        let caps = Capabilities::from_manifest(&HashMap::new());
        assert!(!caps.clipboard_write);
        assert!(!caps.notifications_show);
        assert!(!caps.fetch_allowed("https://api.example.com/"));
    }

    // both of these reject at the scheme/port check before any network call,
    // so they're deterministic and offline
    #[test]
    fn host_fetch_rejects_non_https() {
        let err = host_fetch("http://example.com/", std::time::Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("https"), "expected https rejection, got: {err}");
    }

    #[test]
    fn host_fetch_rejects_blocked_port() {
        let err = host_fetch("https://example.com:22/", std::time::Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("port"), "expected blocked-port rejection, got: {err}");
    }

    #[test]
    fn host_fetch_rejects_loopback() {
        // proves the SSRF guard is actually wired into fetch: a loopback literal
        // is rejected by validate_resolved_host before any DNS or network, so
        // this stays deterministic and offline. the guard phrases loopback as
        // "host not allowed", private ranges as "private ... blocked"
        let err = host_fetch("https://127.0.0.1/", std::time::Duration::from_secs(1))
            .unwrap_err()
            .to_string()
            .to_lowercase();
        assert!(
            err.contains("not allowed") || err.contains("private") || err.contains("blocked"),
            "expected SSRF rejection, got: {err}"
        );
    }

    // ---- end-to-end runtime tests ----
    // these compile a hand-written WAT module, stage it like a real plugin, and
    // drive it through the live WasmHost so the whole round-trip (compile, link,
    // instantiate, capscr_alloc, host write, hook call, host imports) is covered

    use crate::plugin::manifest::{PluginMeta, RuntimeSpec};

    fn load_plugin(
        wat_src: &str,
        caps: HashMap<String, Vec<String>>,
    ) -> (tempfile::TempDir, WasmPlugin) {
        let wasm = wat::parse_str(wat_src).expect("wat should compile to wasm");
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("plugin.wasm"), &wasm).expect("write wasm");
        let manifest = PluginManifest {
            plugin: PluginMeta {
                id: "test".into(),
                name: "Test".into(),
                version: "1.0.0".into(),
                author: None,
                description: None,
            },
            runtime: Some(RuntimeSpec {
                runtime_type: "wasm".into(),
                file: "plugin.wasm".into(),
                memory_max_bytes: None,
                time_slice_ms: None,
            }),
            hooks: [(
                "on_capture_saved".to_string(),
                "capscr_on_capture_saved".to_string(),
            )]
            .into_iter()
            .collect(),
            capabilities: caps,
            enabled: true,
        };
        let host = WasmHost::new().expect("host");
        let plugin = host.load(dir.path(), &manifest).expect("load");
        (dir, plugin)
    }

    // a bump-free allocator that always hands back offset 1024, a hook that
    // copies its payload to offset 2048 (proving it ran and read its args) and
    // echoes it through the log host import
    const ECHO_WAT: &str = r#"
        (module
          (import "capscr" "log" (func $log (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "capscr_on_capture_saved") (param $ptr i32) (param $len i32)
            (local $i i32)
            (block $done
              (loop $copy
                (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                (i32.store8
                  (i32.add (i32.const 2048) (local.get $i))
                  (i32.load8_u (i32.add (local.get $ptr) (local.get $i))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $copy)))
            (call $log (i32.const 2) (i32.const 2048) (local.get $len))))
    "#;

    #[test]
    fn hook_roundtrip_passes_payload_to_guest() {
        let (_dir, plugin) = load_plugin(ECHO_WAT, HashMap::new());
        let payload = "C:/captures/shot-001.png";
        plugin
            .call_hook("on_capture_saved", payload)
            .expect("hook should run cleanly");
        // the hook copied the host-written payload to offset 2048; reading it
        // back proves alloc + host write + hook execution + arg read all worked
        let guard = plugin.store.lock().unwrap();
        let mem = plugin.memory.data(&*guard);
        assert_eq!(&mem[2048..2048 + payload.len()], payload.as_bytes());
    }

    #[test]
    fn infinite_loop_hook_is_trapped() {
        // fuel exhaustion (or the epoch deadline) must stop a runaway hook
        const SPIN_WAT: &str = r#"
            (module
              (memory (export "memory") 1)
              (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "capscr_on_capture_saved") (param i32 i32)
                (loop $l (br $l))))
        "#;
        let (_dir, plugin) = load_plugin(SPIN_WAT, HashMap::new());
        let err = plugin
            .call_hook("on_capture_saved", "p")
            .expect_err("runaway hook must trap");
        assert!(err.to_string().contains("trapped"), "got: {err}");
    }

    #[test]
    fn hook_without_alloc_errors() {
        const NO_ALLOC_WAT: &str = r#"
            (module
              (memory (export "memory") 1)
              (func (export "capscr_on_capture_saved") (param i32 i32)))
        "#;
        let (_dir, plugin) = load_plugin(NO_ALLOC_WAT, HashMap::new());
        let err = plugin
            .call_hook("on_capture_saved", "p")
            .expect_err("missing capscr_alloc must error");
        assert!(err.to_string().contains("capscr_alloc"), "got: {err}");
    }

    #[test]
    fn unsubscribed_hook_is_noop() {
        // plugin exports alloc + memory but not the hook → opted out, no error
        const NO_HOOK_WAT: &str = r#"
            (module
              (memory (export "memory") 1)
              (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024)))
        "#;
        let (_dir, plugin) = load_plugin(NO_HOOK_WAT, HashMap::new());
        plugin
            .call_hook("on_capture_saved", "p")
            .expect("unsubscribed hook should be a silent no-op");
    }

    #[test]
    fn clipboard_import_denied_without_capability() {
        // stores the clipboard_write_text return code at offset 0; with no
        // clipboard capability the host must deny in-band (never touching the
        // OS clipboard) and return HOST_ERR_DENIED
        const CLIP_WAT: &str = r#"
            (module
              (import "capscr" "clipboard_write_text" (func $cb (param i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "capscr_on_capture_saved") (param $ptr i32) (param $len i32)
                (i32.store (i32.const 0) (call $cb (local.get $ptr) (local.get $len)))))
        "#;
        let (_dir, plugin) = load_plugin(CLIP_WAT, HashMap::new());
        plugin
            .call_hook("on_capture_saved", "hello")
            .expect("hook runs; denial is an in-band return code");
        let guard = plugin.store.lock().unwrap();
        let mem = plugin.memory.data(&*guard);
        let code = i32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]);
        assert_eq!(code, HOST_ERR_DENIED);
    }
}
