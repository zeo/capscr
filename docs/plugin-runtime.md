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

All string arguments are `(ptr, len)` pairs into the plugin's linear memory,
UTF-8. Imports that touch a resource are gated on the matching entry in the
manifest's `[capabilities]` table — an un-granted call returns a denial code
and logs a warning host-side instead of performing the action.

```wat
(import "capscr" "log"
  (func $log (param i32 i32 i32)))                 ; level, ptr, len -> ()
(import "capscr" "clipboard_write_text"
  (func $clipboard_write_text (param i32 i32) (result i32)))   ; ptr, len -> code
(import "capscr" "notify"
  (func $notify (param i32 i32 i32 i32) (result i32)))         ; title*, body* -> code
(import "capscr" "fetch"
  (func $fetch (param i32 i32) (result i64)))      ; url ptr, len -> packed ptr/len
```

### `log(level, ptr, len)`

| level | meaning |
|-------|---------|
| 0     | error   |
| 1     | warn    |
| 2     | info    |
| 3     | debug   |

Routes to capscr's `tracing` subscriber. Always available — no capability
required.

### `clipboard_write_text(ptr, len) -> i32`

Sets the system clipboard to the given UTF-8 text. Requires
`clipboard = ["write"]`.

### `notify(title_ptr, title_len, body_ptr, body_len) -> i32`

Shows a native notification. Requires `notifications = ["show"]`.

`i32` return codes for the two imports above:

| code | meaning                                        |
|------|------------------------------------------------|
| 0    | ok                                             |
| -1   | denied — capability not granted in the manifest|
| -2   | bad args — ptr/len out of bounds or not utf-8  |
| -3   | the host operation itself failed               |

### `fetch(url_ptr, url_len) -> i64`

Performs a **blocking** HTTP(S) GET and writes the response body into the
plugin's linear memory via the exported `capscr_alloc`, returning the location
packed as `(ptr << 32) | len`. A return value of `0` means failure or denial
(check the host log). Decode in the plugin with
`ptr = (ret >> 32) as i32; len = ret as i32`.

Requires `fetch = [...patterns...]`, where each pattern is matched against the
full request URL: a trailing `*` is a prefix wildcard, otherwise it's an exact
match (no regex, no path-segment globbing). The URL must be `https` — cleartext
`http` is rejected, matching the upload destinations.
Patterns match on the raw string prefix, so include the path separator —
`https://api.example.com/*` is host-scoped, but `https://api.example.com*` would
also match `https://api.example.com.attacker.test/`.

Safety bounds:

- the same SSRF guard as the upload path — private, loopback, link-local, and
  cloud-metadata addresses are rejected, and DNS is resolved twice to defeat
  rebinding
- non-web ports (22, 23, 25, 445, 3306, 6379, …) are refused, so a plugin
  can't use fetch to probe service reachability
- HTTP redirects are disabled, so a `30x` can't escape the host allowlist
- the response body is capped at 1 MiB
- a single fetch is bounded by a 10s timeout; the per-hook epoch budget is
  refreshed afterwards so the plugin isn't trapped the instant it resumes
- all fetches within one hook call share a 15s aggregate wall-clock budget, and
  each call is shortened to whatever budget remains — so a fetch loop can't hold
  the dispatch thread (and the plugin-manager lock) open indefinitely

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

- host imports today: `log`, `clipboard_write_text`, `notify`, `fetch`. more
  arrive incrementally
- payloads are strings only; image bytes for `on_capture` come once the
  host blob API is settled
- capabilities for `clipboard`, `notifications`, and `fetch` are enforced;
  other declared capabilities are still informational
- per-hook fuel limit and epoch-deadline trap are active; `time_slice_ms`
  tunes the epoch budget (defaults to ~500ms)
- only `runtime.type = "wasm"` accepted; a native loader is not planned
