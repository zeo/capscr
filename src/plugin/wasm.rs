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
use std::path::Path;
use std::sync::{Arc, Mutex};
use wasmtime::{Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

pub struct WasmPlugin {
    pub id: String,
    store: Mutex<Store<HostState>>,
    instance: Instance,
    memory: Memory,
    /// exported `capscr_alloc(size) -> ptr`; None if the plugin doesn't export it
    alloc: Option<TypedFunc<i32, i32>>,
    /// optional hooks resolved at load time, indexed by hook name
    hooks: std::collections::HashMap<String, TypedFunc<(i32, i32), ()>>,
}

/// per-instantiation state. Currently only the plugin id (for log routing);
/// future capability/permission tracking lands here.
struct HostState {
    plugin_id: String,
}

pub struct WasmHost {
    engine: Engine,
}

impl WasmHost {
    pub fn new() -> Result<Self> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(false); // future: per-hook fuel budget
        let engine = Engine::new(&cfg)
            .map_err(|e| anyhow!("wasmtime engine init: {e}"))?;
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

        let mut store = Store::new(
            &self.engine,
            HostState {
                plugin_id: manifest.plugin.id.clone(),
            },
        );
        if let Some(limit) = runtime.memory_max_bytes {
            store.limiter(move |_| {
                Box::leak(Box::new(MemLimiter { cap: limit })) as &mut dyn wasmtime::ResourceLimiter
            });
        }
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
    ) -> Result<bool, anyhow::Error> {
        Ok(desired <= self.cap)
    }
    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, anyhow::Error> {
        Ok(desired <= 65536)
    }
}

/// shared host instance. Created lazily on first PluginManager::load_all so
/// the engine compile cost is paid only when at least one plugin exists.
pub type SharedWasmHost = Arc<WasmHost>;
