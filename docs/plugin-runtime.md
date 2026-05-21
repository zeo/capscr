# plugin runtime — 0.4 author guide

capscr's plugin host loads WebAssembly modules at startup and dispatches
capscr events (capture saved, upload success, …) to plugin-exported
functions. The host is feature-gated behind `plugin-runtime` so the
default build stays small while the API surface stabilises.

## on-disk layout

```
%APPDATA%\com.capscr.capscr\data\plugins\
    └─ <plugin-id>\
       ├─ plugin.toml      ; manifest, required
       └─ plugin.wasm      ; module artefact, required when runtime.type = wasm
```

## manifest schema

```toml
[plugin]
id          = "my-plugin"           # [a-zA-Z0-9_-]+, used as folder name
name        = "My Plugin"
version     = "0.1.0"
author      = "Someone"             # optional
description = "Does something."     # optional

[runtime]
type             = "wasm"           # only "wasm" today
file             = "plugin.wasm"    # relative to the plugin dir
memory_max_bytes = 16777216         # optional, defaults to wasmtime default
time_slice_ms    = 200              # optional, reserved for fuel limiting

# map of capscr event → exported function name
[hooks]
on_capture_saved   = "capscr_on_capture_saved"
on_upload_success  = "capscr_on_upload_success"

# declared capabilities; today purely informational. Future versions will
# gate host APIs on these.
[capabilities]
clipboard      = ["read", "write"]
notifications  = ["show"]
fetch          = ["https://api.example.com/*"]
```

Plugins without a `[runtime]` section stay metadata-only — they appear in
the Marketplace tab but the host doesn't instantiate them.

## hooks

| hook                | payload                       | signature                       |
|---------------------|-------------------------------|---------------------------------|
| `on_capture`        | `"Region"` / `"Window"` / …   | `(ptr: i32, len: i32) -> ()`    |
| `on_capture_saved`  | absolute path of the file     | `(ptr: i32, len: i32) -> ()`    |
| `on_upload_success` | result URL                    | `(ptr: i32, len: i32) -> ()`    |

The host writes the UTF-8 payload into the plugin's linear memory at a
region allocated via the exported `capscr_alloc(size: i32) -> i32`
function, then calls the hook with `(ptr, len)`. Plugins that don't
export `capscr_alloc` are skipped for any hook that needs a payload.

## host imports (module `capscr`)

```wat
(import "capscr" "log"
  (func $capscr_log (param i32 i32 i32)))   ; level, ptr, len
```

| level | meaning |
|-------|---------|
| 0     | error   |
| 1     | warn    |
| 2     | info    |
| 3     | debug   |

UTF-8 messages, host-side they route to capscr's `tracing` subscriber.

## minimal Rust example

```rust
// in your Cargo.toml:
//   [lib] crate-type = ["cdylib"]
//   [package] edition = "2021"

#[no_mangle]
pub extern "C" fn capscr_alloc(size: i32) -> i32 {
    let v: Vec<u8> = Vec::with_capacity(size as usize);
    let ptr = v.as_ptr() as i32;
    std::mem::forget(v);
    ptr
}

#[link(wasm_import_module = "capscr")]
extern "C" {
    fn log(level: i32, ptr: i32, len: i32);
}

#[no_mangle]
pub extern "C" fn capscr_on_capture_saved(ptr: i32, len: i32) {
    let msg = format!("plugin saw a capture at offset {ptr}, len {len}");
    unsafe { log(2, msg.as_ptr() as i32, msg.len() as i32); }
}
```

Build with `cargo build --release --target wasm32-unknown-unknown`,
copy `target/wasm32-unknown-unknown/release/your_crate.wasm` to
`<plugins-dir>/<id>/plugin.wasm`, write the `plugin.toml`, and capscr
will load it at next launch.

## current limits + roadmap

- single host import (`capscr.log`) — toast/notification/clipboard/fetch
  arrive incrementally
- payloads are strings only; image bytes for `on_capture` come once the
  host blob API is settled
- capabilities are declared but not enforced
- no per-hook fuel limit yet (manifest field reserved)
- only `runtime.type = "wasm"` accepted; a native loader is not planned
