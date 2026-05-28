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
use image::RgbaImage;
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
    /// the on_capture hook, resolved separately from the notify hooks because it
    /// takes the binary image blob and returns an i64 response (0=continue,
    /// <0=cancel, >0=ptr/len of a replacement image) rather than `(ptr,len)->()`
    capture_hook: Option<TypedFunc<(i32, i32), i64>>,
    /// granted image capabilities, mirrored here so dispatch can gate without
    /// locking the store: read = receive pixels, modify = honour cancel/replace
    image_read: bool,
    image_modify: bool,
}

/// outcome of an on_capture hook, consumed by PluginManager::dispatch
pub enum CaptureOutcome {
    Continue,
    Cancel,
    Modified(RgbaImage),
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
    /// flat key→string config read from <plugin_dir>/config.toml at load time,
    /// surfaced to the plugin via the config_get host import. the sandbox has no
    /// filesystem, so this is how a plugin gets user-authored settings (webhook
    /// urls, styling, …) without a native fs
    config: std::collections::HashMap<String, String>,
}

/// capabilities a plugin declared in its `[capabilities]` manifest table,
/// resolved to the concrete grants the host enforces today.
#[derive(Default)]
struct Capabilities {
    clipboard_write: bool,
    notifications_show: bool,
    image_read: bool,
    image_modify: bool,
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
            image_read: granted("image", "read"),
            image_modify: granted("image", "modify"),
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
/// hard cap on a replacement image a plugin returns from on_capture, so a buggy
/// or malicious plugin can't make the host allocate unbounded memory
const MAX_CAPTURE_BLOB_BYTES: usize = 256 * 1024 * 1024;
/// max width/height for a plugin-returned replacement image
const MAX_CAPTURE_DIM: u32 = 16384;
/// cap on <plugin_dir>/config.toml so a malicious marketplace plugin can't ship
/// a huge config to bloat host memory at load
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
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

        let config = load_plugin_config(plugin_dir);

        let mut store = Store::new(
            &self.engine,
            HostState {
                plugin_id: manifest.plugin.id.clone(),
                caps: Capabilities::from_manifest(&manifest.capabilities),
                deadline_ticks,
                fetch_deadline: None,
                config,
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
                    let body = match host_request(HttpMethod::Get, &url, None, None, timeout) {
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

                    write_guest_response(&mut caller, &body)
                },
            )
            .map_err(|e| anyhow!("link capscr.fetch: {e}"))?;

        // host import: capscr.fetch_post(url*, content_type*, body*) -> i64
        // POST sibling of fetch for webhook-style plugins. same fetch capability,
        // SSRF/https/port guards, per-hook budget, and packed-i64 response.
        linker
            .func_wrap(
                "capscr",
                "fetch_post",
                |mut caller: Caller<'_, HostState>,
                 url_ptr: i32,
                 url_len: i32,
                 ct_ptr: i32,
                 ct_len: i32,
                 body_ptr: i32,
                 body_len: i32|
                 -> i64 {
                    let url = match read_guest_str(&mut caller, url_ptr, url_len) {
                        Some(s) => s,
                        None => return 0,
                    };
                    if !caller.data().caps.fetch_allowed(&url) {
                        tracing::warn!(
                            plugin = %caller.data().plugin_id,
                            "fetch_post denied: {url} not in declared fetch capability"
                        );
                        return 0;
                    }
                    let content_type = if ct_len > 0 {
                        read_guest_str(&mut caller, ct_ptr, ct_len)
                    } else {
                        None
                    };
                    let req_body = match read_guest_bytes(&mut caller, body_ptr, body_len) {
                        Some(b) => b,
                        None => return 0,
                    };
                    if req_body.len() > FETCH_MAX_BYTES {
                        tracing::warn!(plugin = %caller.data().plugin_id, "fetch_post body too large");
                        return 0;
                    }
                    let remaining = caller
                        .data()
                        .fetch_deadline
                        .map(|dl| dl.saturating_duration_since(std::time::Instant::now()))
                        .unwrap_or_default();
                    if remaining.is_zero() {
                        tracing::warn!(
                            plugin = %caller.data().plugin_id,
                            "fetch_post denied: per-hook fetch time budget exhausted"
                        );
                        return 0;
                    }
                    let timeout =
                        remaining.min(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS));
                    let resp = match host_request(
                        HttpMethod::Post,
                        &url,
                        content_type.as_deref(),
                        Some(&req_body),
                        timeout,
                    ) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(plugin = %caller.data().plugin_id, "fetch_post {url} failed: {e}");
                            return 0;
                        }
                    };
                    let deadline = caller.data().deadline_ticks;
                    caller.as_context_mut().set_epoch_deadline(deadline);

                    write_guest_response(&mut caller, &resp)
                },
            )
            .map_err(|e| anyhow!("link capscr.fetch_post: {e}"))?;

        // host import: capscr.config_get(key_ptr, key_len) -> i64
        // looks up a key in the plugin's config.toml (loaded at startup) and
        // writes the value into guest memory via capscr_alloc, returning the
        // packed (ptr<<32)|len. 0 if the key is absent or args are bad. always
        // available — a plugin reading its own user-authored config is benign.
        linker
            .func_wrap(
                "capscr",
                "config_get",
                |mut caller: Caller<'_, HostState>, key_ptr: i32, key_len: i32| -> i64 {
                    let key = match read_guest_str(&mut caller, key_ptr, key_len) {
                        Some(s) => s,
                        None => return 0,
                    };
                    let value = match caller.data().config.get(&key) {
                        Some(v) => v.clone(),
                        None => return 0,
                    };
                    write_guest_response(&mut caller, value.as_bytes())
                },
            )
            .map_err(|e| anyhow!("link capscr.config_get: {e}"))?;

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
        let mut capture_hook = None;
        for (hook_name, export_name) in &manifest.hooks {
            // on_capture has a richer ABI: (ptr,len)->i64, binary image blob in,
            // response out. resolve it separately from the (ptr,len)->() notifies
            if hook_name == "on_capture" {
                match instance.get_typed_func::<(i32, i32), i64>(&mut store, export_name) {
                    Ok(f) => capture_hook = Some(f),
                    Err(e) => tracing::warn!(
                        "plugin '{}' on_capture export '{}' missing or not (i32,i32)->i64: {e}",
                        manifest.plugin.id,
                        export_name
                    ),
                }
                continue;
            }
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

        let image_read = store.data().caps.image_read;
        let image_modify = store.data().caps.image_modify;

        Ok(WasmPlugin {
            id: manifest.plugin.id.clone(),
            store: Mutex::new(store),
            instance,
            memory,
            alloc,
            hooks,
            capture_hook,
            image_read,
            image_modify,
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

    /// true if this plugin subscribes to on_capture and may read pixels —
    /// dispatch only builds + delivers the (large) image blob for these plugins
    pub fn wants_capture(&self) -> bool {
        self.capture_hook.is_some() && self.image_read
    }

    /// deliver the capture blob ([w:u32][h:u32][mode:u32][rgba]) to on_capture
    /// and decode the i64 response. cancel/replace are honoured only with the
    /// image:modify capability; anything malformed degrades to Continue so a
    /// buggy or hostile plugin can never corrupt or silently drop a capture
    pub fn call_capture_hook(&self, blob: &[u8]) -> Result<CaptureOutcome> {
        let hook = match self.capture_hook.as_ref() {
            Some(h) => h,
            None => return Ok(CaptureOutcome::Continue),
        };
        if !self.image_read {
            return Ok(CaptureOutcome::Continue);
        }
        let alloc = self
            .alloc
            .as_ref()
            .ok_or_else(|| anyhow!("plugin '{}' has no capscr_alloc export", self.id))?;
        if blob.len() > i32::MAX as usize {
            return Err(anyhow!("capture blob too large for the guest"));
        }

        let mut store = self
            .store
            .lock()
            .map_err(|_| anyhow!("plugin '{}' store poisoned", self.id))?;

        let _ = store.set_fuel(DEFAULT_HOOK_FUEL);
        let deadline = store.data().deadline_ticks;
        store.set_epoch_deadline(deadline);
        store.data_mut().fetch_deadline = Some(std::time::Instant::now() + FETCH_HOOK_BUDGET);

        let len = blob.len() as i32;
        let ptr = alloc
            .call(&mut *store, len)
            .map_err(|e| anyhow!("capscr_alloc({len}): {e}"))?;
        if ptr <= 0 {
            return Err(anyhow!("capscr_alloc returned {ptr} (out of memory?)"));
        }
        self.memory
            .write(&mut *store, ptr as usize, blob)
            .map_err(|e| anyhow!("memory write: {e}"))?;

        let ret = hook
            .call(&mut *store, (ptr, len))
            .map_err(|e| anyhow!("on_capture trapped: {e}"))?;

        if ret == 0 {
            return Ok(CaptureOutcome::Continue);
        }
        if ret < 0 {
            if self.image_modify {
                return Ok(CaptureOutcome::Cancel);
            }
            tracing::warn!(
                "plugin '{}' tried to cancel a capture without image:modify",
                self.id
            );
            return Ok(CaptureOutcome::Continue);
        }
        // ret > 0: packed (ptr<<32)|len of a replacement [w:u32][h:u32][rgba] blob
        if !self.image_modify {
            tracing::warn!(
                "plugin '{}' returned a modified image without image:modify",
                self.id
            );
            return Ok(CaptureOutcome::Continue);
        }
        let out_ptr = ((ret as u64) >> 32) as usize;
        let out_len = (ret as u64 & 0xffff_ffff) as usize;
        match read_capture_image(self.memory.data(&*store), out_ptr, out_len) {
            Some(img) => Ok(CaptureOutcome::Modified(img)),
            None => {
                tracing::warn!(
                    "plugin '{}' returned a malformed image blob — ignoring",
                    self.id
                );
                Ok(CaptureOutcome::Continue)
            }
        }
    }
}

/// parse a `[w:u32 LE][h:u32 LE][rgba…]` blob (a plugin's replacement image)
/// out of guest memory with bounds + size sanity checks. None on anything off,
/// so the caller falls back to the original capture
fn read_capture_image(mem: &[u8], ptr: usize, len: usize) -> Option<RgbaImage> {
    if !(8..=MAX_CAPTURE_BLOB_BYTES).contains(&len) {
        return None;
    }
    let end = ptr.checked_add(len)?;
    if end > mem.len() {
        return None;
    }
    let blob = &mem[ptr..end];
    let w = u32::from_le_bytes(blob[0..4].try_into().ok()?);
    let h = u32::from_le_bytes(blob[4..8].try_into().ok()?);
    if w == 0 || h == 0 || w > MAX_CAPTURE_DIM || h > MAX_CAPTURE_DIM {
        return None;
    }
    let pixel_bytes = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    if len != 8 + pixel_bytes {
        return None;
    }
    RgbaImage::from_raw(w, h, blob[8..].to_vec())
}

/// read <plugin_dir>/config.toml into a flat key→string map for config_get.
/// scalar values (string/int/float/bool) are stringified; arrays/tables/dates
/// are skipped. missing/oversized/unparseable file → empty map (config_get then
/// returns "absent" for every key). the host reads this — the sandbox can't.
fn load_plugin_config(plugin_dir: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let path = plugin_dir.join("config.toml");
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return out, // no config file is the common case
    };
    if meta.len() > MAX_CONFIG_BYTES {
        tracing::warn!("plugin config.toml at {} exceeds cap; ignoring", path.display());
        return out;
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("reading {}: {e}", path.display());
            return out;
        }
    };
    let table: toml::Table = match toml::from_str(&raw) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("parsing {}: {e}", path.display());
            return out;
        }
    };
    for (k, v) in table {
        let s = match v {
            toml::Value::String(s) => s,
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::Boolean(b) => b.to_string(),
            _ => continue, // skip arrays/tables/datetime — flat scalars only
        };
        out.insert(k, s);
    }
    out
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
#[derive(Clone, Copy)]
enum HttpMethod {
    Get,
    Post,
}

/// blocking HTTP(S) request behind the upload path's SSRF guard, shared by the
/// fetch (GET) and fetch_post (POST) imports. https only, non-web ports refused,
/// redirects disabled, response capped at FETCH_MAX_BYTES.
fn host_request(
    method: HttpMethod,
    url: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
    timeout: std::time::Duration,
) -> Result<Vec<u8>> {
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
    let mut req = match method {
        HttpMethod::Get => client.get(url),
        HttpMethod::Post => client.post(url),
    };
    if let Some(b) = body {
        req = req.body(b.to_vec());
    }
    if let Some(ct) = content_type {
        req = req.header(reqwest::header::CONTENT_TYPE, ct);
    }
    let resp = req.send()?;
    if !resp.status().is_success() {
        return Err(anyhow!("http status {}", resp.status()));
    }
    let mut buf = Vec::new();
    resp.take(FETCH_MAX_BYTES as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// read raw bytes out of guest memory at (ptr,len). None on oob/negative.
fn read_guest_bytes(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Option<Vec<u8>> {
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
    Some(data[start..end].to_vec())
}

/// allocate space in the guest via capscr_alloc, copy `body` there, and return
/// the packed (ptr<<32)|len. 0 on any failure. shared by fetch and fetch_post.
fn write_guest_response(caller: &mut Caller<'_, HostState>, body: &[u8]) -> i64 {
    if body.len() > i32::MAX as usize {
        return 0;
    }
    let len = body.len() as i32;
    let alloc = match caller.get_export("capscr_alloc").and_then(|e| e.into_func()) {
        Some(f) => f,
        None => return 0,
    };
    let alloc = match alloc.typed::<i32, i32>(&*caller) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    let ptr = match alloc.call(&mut *caller, len) {
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
    if mem.write(&mut *caller, ptr as usize, body).is_err() {
        return 0;
    }
    ((ptr as i64) << 32) | (len as i64 & 0xffff_ffff)
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

    // these reject at the scheme/port/SSRF check before any network call, so
    // they're deterministic and offline — and they exercise both methods, since
    // fetch (GET) and fetch_post (POST) share host_request's guards
    const SECOND: std::time::Duration = std::time::Duration::from_secs(1);

    #[test]
    fn host_request_rejects_non_https() {
        let err = host_request(HttpMethod::Get, "http://example.com/", None, None, SECOND)
            .unwrap_err()
            .to_string();
        assert!(err.contains("https"), "expected https rejection, got: {err}");
    }

    #[test]
    fn host_request_rejects_blocked_port() {
        let err = host_request(HttpMethod::Get, "https://example.com:22/", None, None, SECOND)
            .unwrap_err()
            .to_string();
        assert!(err.contains("port"), "expected blocked-port rejection, got: {err}");
    }

    #[test]
    fn host_request_rejects_loopback() {
        // proves the SSRF guard is wired in: a loopback literal is rejected by
        // validate_resolved_host before any DNS or network. the guard phrases
        // loopback as "host not allowed", private ranges as "private ... blocked"
        let err = host_request(HttpMethod::Get, "https://127.0.0.1/", None, None, SECOND)
            .unwrap_err()
            .to_string()
            .to_lowercase();
        assert!(
            err.contains("not allowed") || err.contains("private") || err.contains("blocked"),
            "expected SSRF rejection, got: {err}"
        );
    }

    #[test]
    fn host_request_post_uses_the_same_ssrf_guard() {
        let err = host_request(
            HttpMethod::Post,
            "https://127.0.0.1/hook",
            Some("application/json"),
            Some(b"{}"),
            SECOND,
        )
        .unwrap_err()
        .to_string()
        .to_lowercase();
        assert!(
            err.contains("not allowed") || err.contains("private") || err.contains("blocked"),
            "expected SSRF rejection for POST, got: {err}"
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

    // ---- on_capture (image-blob) end-to-end tests ----

    fn caps_map(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    fn load_capture_plugin(
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
            hooks: [("on_capture".to_string(), "capscr_on_capture".to_string())]
                .into_iter()
                .collect(),
            capabilities: caps,
            enabled: true,
        };
        let host = WasmHost::new().expect("host");
        let plugin = host.load(dir.path(), &manifest).expect("load");
        (dir, plugin)
    }

    // a 2x2 region capture blob: [w=2][h=2][mode=2][16 rgba bytes]
    fn sample_blob() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 16]);
        b
    }

    const CONTINUE_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "capscr_on_capture") (param i32 i32) (result i64) (i64.const 0)))
    "#;
    const CANCEL_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "capscr_on_capture") (param i32 i32) (result i64) (i64.const -1)))
    "#;
    // writes a 1x1 white replacement at offset 4096 and returns (4096<<32)|12
    const MODIFY_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
          (func (export "capscr_on_capture") (param i32 i32) (result i64)
            (i32.store (i32.const 4096) (i32.const 1))
            (i32.store (i32.const 4100) (i32.const 1))
            (i32.store (i32.const 4104) (i32.const -1))
            (i64.or (i64.shl (i64.const 4096) (i64.const 32)) (i64.const 12))))
    "#;

    #[test]
    fn capture_continue_is_continue() {
        let (_d, p) = load_capture_plugin(CONTINUE_WAT, caps_map(&[("image", &["read"])]));
        assert!(matches!(
            p.call_capture_hook(&sample_blob()).unwrap(),
            CaptureOutcome::Continue
        ));
    }

    #[test]
    fn capture_cancel_honoured_with_modify_cap() {
        let (_d, p) = load_capture_plugin(CANCEL_WAT, caps_map(&[("image", &["read", "modify"])]));
        assert!(matches!(
            p.call_capture_hook(&sample_blob()).unwrap(),
            CaptureOutcome::Cancel
        ));
    }

    #[test]
    fn capture_cancel_ignored_without_modify_cap() {
        let (_d, p) = load_capture_plugin(CANCEL_WAT, caps_map(&[("image", &["read"])]));
        assert!(matches!(
            p.call_capture_hook(&sample_blob()).unwrap(),
            CaptureOutcome::Continue
        ));
    }

    #[test]
    fn capture_modify_returns_replacement_image() {
        let (_d, p) = load_capture_plugin(MODIFY_WAT, caps_map(&[("image", &["read", "modify"])]));
        let out = p.call_capture_hook(&sample_blob()).unwrap();
        let CaptureOutcome::Modified(img) = out else {
            panic!("expected Modified");
        };
        assert_eq!(img.width(), 1);
        assert_eq!(img.height(), 1);
        assert_eq!(img.into_raw(), vec![255u8, 255, 255, 255]);
    }

    #[test]
    fn capture_modify_ignored_without_modify_cap() {
        let (_d, p) = load_capture_plugin(MODIFY_WAT, caps_map(&[("image", &["read"])]));
        assert!(matches!(
            p.call_capture_hook(&sample_blob()).unwrap(),
            CaptureOutcome::Continue
        ));
    }

    #[test]
    fn capture_hook_skipped_without_read_cap() {
        // no image:read → wants_capture() false and call_capture_hook short-circuits
        let (_d, p) = load_capture_plugin(MODIFY_WAT, HashMap::new());
        assert!(!p.wants_capture());
        assert!(matches!(
            p.call_capture_hook(&sample_blob()).unwrap(),
            CaptureOutcome::Continue
        ));
    }

    // ---- config_get tests ----

    #[test]
    fn load_plugin_config_stringifies_scalars_and_skips_compound() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("config.toml"),
            "webhook = \"https://x\"\nsize = 8\nratio = 1.5\non = true\narr = [1, 2]\n",
        )
        .expect("write config");
        let cfg = load_plugin_config(dir.path());
        assert_eq!(cfg.get("webhook").map(String::as_str), Some("https://x"));
        assert_eq!(cfg.get("size").map(String::as_str), Some("8"));
        assert_eq!(cfg.get("ratio").map(String::as_str), Some("1.5"));
        assert_eq!(cfg.get("on").map(String::as_str), Some("true"));
        assert!(!cfg.contains_key("arr"), "arrays are skipped");
    }

    #[test]
    fn load_plugin_config_missing_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load_plugin_config(dir.path()).is_empty());
    }

    #[test]
    fn config_get_returns_value_to_guest() {
        // the hook calls config_get("wkey", 4) and stores the packed ptr/len at
        // offset 0. the host wrote the value via capscr_alloc (offset 1024).
        const CFG_WAT: &str = r#"
            (module
              (import "capscr" "config_get" (func $cfg (param i32 i32) (result i64)))
              (memory (export "memory") 1)
              (data (i32.const 100) "wkey")
              (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "capscr_on_capture_saved") (param i32 i32)
                (i64.store (i32.const 0) (call $cfg (i32.const 100) (i32.const 4)))))
        "#;
        let wasm = wat::parse_str(CFG_WAT).expect("wat compiles");
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("plugin.wasm"), &wasm).expect("write wasm");
        std::fs::write(dir.path().join("config.toml"), "wkey = \"hello\"\n").expect("write config");
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
            capabilities: HashMap::new(),
            enabled: true,
        };
        let host = WasmHost::new().expect("host");
        let plugin = host.load(dir.path(), &manifest).expect("load");
        plugin
            .call_hook("on_capture_saved", "x")
            .expect("hook runs");
        let guard = plugin.store.lock().unwrap();
        let mem = plugin.memory.data(&*guard);
        let packed = i64::from_le_bytes(mem[0..8].try_into().unwrap());
        let ptr = ((packed as u64) >> 32) as usize;
        let len = (packed as u64 & 0xffff_ffff) as usize;
        assert_eq!(&mem[ptr..ptr + len], b"hello");
    }

    #[test]
    fn config_get_absent_key_returns_zero() {
        const CFG_WAT: &str = r#"
            (module
              (import "capscr" "config_get" (func $cfg (param i32 i32) (result i64)))
              (memory (export "memory") 1)
              (data (i32.const 100) "nope")
              (func (export "capscr_alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "capscr_on_capture_saved") (param i32 i32)
                (i64.store (i32.const 0) (call $cfg (i32.const 100) (i32.const 4)))))
        "#;
        let wasm = wat::parse_str(CFG_WAT).expect("wat compiles");
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("plugin.wasm"), &wasm).expect("write wasm");
        // no config.toml at all → every key absent
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
            capabilities: HashMap::new(),
            enabled: true,
        };
        let host = WasmHost::new().expect("host");
        let plugin = host.load(dir.path(), &manifest).expect("load");
        plugin.call_hook("on_capture_saved", "x").expect("hook runs");
        let guard = plugin.store.lock().unwrap();
        let mem = plugin.memory.data(&*guard);
        let packed = i64::from_le_bytes(mem[0..8].try_into().unwrap());
        assert_eq!(packed, 0, "absent key must return 0");
    }

    // read_capture_image parses an attacker-controlled blob (a sandboxed plugin's
    // replacement image) out of guest memory. these guard the rejection paths so
    // a malformed/hostile blob can never produce a bad RgbaImage or read OOB.
    fn img_blob(w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(pixels);
        b
    }

    #[test]
    fn read_capture_image_accepts_valid() {
        let blob = img_blob(1, 1, &[10, 20, 30, 40]);
        let img = read_capture_image(&blob, 0, blob.len()).expect("valid 1x1");
        assert_eq!(img.into_raw(), vec![10, 20, 30, 40]);
    }

    #[test]
    fn read_capture_image_rejects_malformed() {
        // too short (< 8 header bytes)
        assert!(read_capture_image(&[0, 0, 0, 0], 0, 4).is_none(), "len<8");
        // over the size cap — checked before any memory access, so a tiny buffer
        // with a huge claimed len must still be rejected without reading OOB
        assert!(
            read_capture_image(&[], 0, MAX_CAPTURE_BLOB_BYTES + 1).is_none(),
            "over cap"
        );
        // ptr+len out of bounds of the provided memory
        let v = vec![0u8; 12];
        assert!(read_capture_image(&v, 8, 12).is_none(), "oob (ptr+len>mem)");
        // ptr+len overflows usize
        assert!(read_capture_image(&v, usize::MAX, 8).is_none(), "overflow");
        // zero dimension
        assert!(read_capture_image(&img_blob(0, 1, &[]), 0, 8).is_none(), "w=0");
        // dimension exceeds MAX_CAPTURE_DIM (caught before the len check, so the
        // buffer only needs the 8-byte header)
        let big = img_blob(MAX_CAPTURE_DIM + 1, 1, &[]);
        assert!(read_capture_image(&big, 0, big.len()).is_none(), "w>max dim");
        // length doesn't match 8 + w*h*4 (claims 2x2 = 24 bytes, supplies 12)
        let mismatch = img_blob(2, 2, &[0, 0, 0, 0]);
        assert!(
            read_capture_image(&mismatch, 0, mismatch.len()).is_none(),
            "len != 8 + w*h*4"
        );
    }
}
