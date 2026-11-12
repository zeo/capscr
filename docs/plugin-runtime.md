# plugin runtime: author guide

capscr's plugin host loads WebAssembly modules and dispatches capscr events
(capture saved, upload success, …) to plugin-exported functions. Plugins load
at launch and reload after an install, an uninstall, an enable toggle, a
settings save or the plugins tab's reload button, so no restart is needed.
The host is the `plugin-runtime` cargo feature, on by default; a
`--no-default-features` build leaves it out.

## on-disk layout

The plugins folder is `%APPDATA%\capscr\capscr\data\plugins\` on Windows and
`~/.local/share/capscr/plugins/` on Linux (`$XDG_DATA_HOME/capscr/plugins/`
when that is set).

```
<plugins-dir>\
    └─ <plugin-id>\
       ├─ plugin.toml      ; manifest, required
       ├─ plugin.wasm      ; module artefact, required when runtime.type = wasm
       └─ config.toml      ; optional, user-edited; exposed via config_get
```

## manifest schema

```toml
# optional top-level flag, managed by the plugins tab's enable/disable toggle.
# defaults to true; when false the host does not instantiate the plugin at all
enabled = true

[plugin]
id          = "my-plugin"           # [a-zA-Z0-9_-]+, used as folder name
name        = "My Plugin"
version     = "0.1.0"
author      = "Someone"             # optional
description = "Does something."     # optional
payload     = "json"                # optional, "text" (default) or "json"

[runtime]
type             = "wasm"           # only "wasm" today
file             = "plugin.wasm"    # relative path, must stay inside the plugin dir
memory_max_bytes = 16777216         # optional, defaults to 64 MiB; host maximum is 256 MiB
time_slice_ms    = 200              # optional, per-hook epoch budget in ms, capped at 2000

# map of capscr event → exported function name
[hooks]
on_capture_saved   = "capscr_on_capture_saved"
on_upload_success  = "capscr_on_upload_success"

# declared capabilities. clipboard/notifications/fetch/image/upload are
# enforced; other keys are still informational
[capabilities]
clipboard      = ["read", "write"]
notifications  = ["show"]
fetch          = ["https://api.example.com/*"]
image          = ["read", "modify"]   # gates the on_capture hook
upload         = ["delete_url"]       # adds delete_url to json upload payloads
                                      # "destination" gates the upload hook
```

Plugins without a `[runtime]` section stay metadata-only — they appear in
the Marketplace tab but the host doesn't instantiate them. A plugin with
`enabled = false` is listed but likewise not instantiated.

A marketplace entry can require a minimum capscr with `min_capscr_version`;
an older capscr greys out the install button and refuses the install. See
[marketplace.md](marketplace.md).

## hooks

| hook                | payload                          | signature                        |
|---------------------|----------------------------------|----------------------------------|
| `on_capture`        | binary image blob (see below)    | `(ptr: i32, len: i32) -> i64`    |
| `on_capture_saved`  | absolute path of the file (utf-8)| `(ptr: i32, len: i32) -> ()`     |
| `on_upload_success` | result URL (utf-8)               | `(ptr: i32, len: i32) -> ()`     |
| `upload`            | file blob (see below)            | `(ptr: i32, len: i32) -> i64`    |

For the two notify hooks the host writes the UTF-8 payload into the plugin's
linear memory at a region allocated via the exported
`capscr_alloc(size: i32) -> i32`, then calls the hook with `(ptr, len)`. The
return value is ignored. Plugins that don't export `capscr_alloc` are skipped
for any hook that needs a payload.

### json payloads

With `payload = "json"` under `[plugin]`, the two notify hooks get a JSON
object instead of the bare path or URL. Plugins that leave the field out keep
the text payloads above.

`on_capture_saved`:

```json
{
  "path": "C:\\Users\\me\\Pictures\\capscr\\capture.gif",
  "kind": "gif",
  "width": 1920,
  "height": 1080,
  "task": "gif-save",
  "duration_ms": 4200
}
```

`kind` is the lowercased file extension. `width` and `height` are the saved
file's pixel size; a GIF reports its scaled output size and an MP4 the
encoded size. `task` is the id of the capture task that recorded it, and
`duration_ms` is the recorded time with pauses left out. Any field the host
doesn't know for a save is `null`; still captures have no `task` or
`duration_ms`.

`on_upload_success`:

```json
{
  "url": "https://i.imgur.com/abc.png",
  "path": "C:\\Users\\me\\Pictures\\capscr\\capture.png",
  "destination": "imgur",
  "delete_url": "https://imgur.com/delete/xyz"
}
```

`destination` is one of `imgur`, `custom`, `sftp`, `s3` or `plugin`. `path` is `null`
when no file on disk backs the upload: an editor upload, or the Upload action
with history saving turned off.
`delete_url` is present only when the host returned one and the manifest
grants `upload = ["delete_url"]`, since anyone holding it can remove the
upload.

### `on_capture` (image blob)

`on_capture` runs after pixels are captured but before they're saved, copied, or
uploaded — so it can observe, **cancel**, or **replace** the capture. It requires
the `image` capability and a richer ABI than the notify hooks.

The host writes a binary blob (via `capscr_alloc`) and calls the hook:

```
input blob:  [width: u32 LE][height: u32 LE][mode: u32 LE][rgba bytes…]
mode:        0 = FullScreen, 1 = Window, 2 = Region, 3 = Gif
```

The hook returns an `i64`:

| return            | meaning                                                        |
|-------------------|----------------------------------------------------------------|
| `0`               | continue — use the capture unchanged                           |
| `< 0`             | cancel — drop the capture (no save/clipboard/upload)           |
| `> 0`             | replace — packed `(ptr << 32) \| len` of a replacement blob    |

A replacement blob the plugin allocates (via `capscr_alloc`) and fills as
`[width: u32 LE][height: u32 LE][rgba bytes…]`; the host reads it back, validates
it (`len == 8 + w*h*4`, dims ≤ 16384, ≤ 256 MB), and uses it as the capture.
Anything malformed is ignored and the original capture is kept.

Capability gating:

- `image = ["read"]` — the hook is called with the pixels; cancel/replace
  returns are **ignored** (read-only observer).
- `image = ["read", "modify"]` — cancel/replace returns are honoured.
- no `image` capability — `on_capture` is not called.

When several plugins subscribe, a replacement from one feeds the next, so
image filters compose in load order.

### `upload` (plugin as a destination)

A plugin that maps `upload = "<export>"` under `[hooks]` and declares
`upload = ["destination"]` under `[capabilities]` shows up as an upload
destination in the tray menu and in Destinations, next to imgur, custom http,
sftp and S3. When the user picks it, every upload goes to this hook instead of
a built-in host.

```
input blob:  [name_len: u32 LE][file name][mime_len: u32 LE][mime][file bytes…]
```

The hook posts the file itself, usually through `fetch_post`, then returns a
packed `(ptr << 32) | len` pointing at a UTF-8 JSON reply of at most 64 KiB:

```json
{ "url": "https://files.example.com/abc.png", "delete_url": "https://files.example.com/delete/abc" }
```

or, to fail the upload with a message the user sees:

```json
{ "error": "quota reached" }
```

`delete_url` is optional. Both URLs pass the same checks as a custom http
host's reply: an http or https URL with a host, no control characters, and a
length cap. A return of `0` or less, a trap,
or a reply that isn't valid JSON fails the upload. The host never retries a
plugin upload, since the plugin may already have sent the file.

While the `upload` hook runs, `fetch_post` accepts bodies up to 33 MiB (the
32 MiB upload cap plus room for form encoding), each call may take 60s, and the
whole hook gets 120s. Files up to 32 MiB arrive in guest memory, so an upload
plugin should raise `memory_max_bytes` to fit the file, its encoded body and
the plugin's own heap, e.g. `memory_max_bytes = 134217728` (128 MiB).

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
(import "capscr" "fetch_post"
  (func $fetch_post (param i32 i32 i32 i32 i32 i32) (result i64))) ; url*, ctype*, body* -> packed
(import "capscr" "config_get"
  (func $config_get (param i32 i32) (result i64)))   ; key ptr, len -> packed value ptr/len
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

### `fetch_post(url_ptr, url_len, ctype_ptr, ctype_len, body_ptr, body_len) -> i64`

POST sibling of `fetch`, for webhook-style plugins (Discord/Slack notifiers,
pastebin uploads). Sends `body` with the given content-type and returns the
response body packed as `(ptr << 32) | len`, `0` on failure/denial — same as
`fetch`. Pass `ctype_len = 0` to omit the content-type header.

It shares everything with `fetch`: the `fetch` capability + URL-pattern match,
the https-only / blocked-port / SSRF guards, disabled redirects, the 1 MiB
response cap, and the per-hook time budget. The request body is also capped at
1 MiB.

### `config_get(key_ptr, key_len) -> i64`

Looks up `key` in the plugin's own config and writes the value into guest memory
via `capscr_alloc`, returning the packed `(ptr << 32) | len`. Returns `0` if the
key is absent (or the args are bad). Always available — no capability — since a
plugin reading its own user-authored config is benign.

The config lives at `<plugins-dir>/<plugin-id>/config.toml`, a flat table the
**user** edits (the sandbox has no filesystem, so this is how a plugin receives
settings such as webhook URLs, styling and thresholds). The host reads it every time
plugins load (at launch, on the plugins tab's reload, and after a settings save)
and stringifies scalar values, e.g.:

```toml
webhook_url = "https://discord.com/api/webhooks/…"
border_size = 8       # config_get("border_size") -> "8"
enabled     = true    # -> "true"
```

Only scalars (string / integer / float / boolean) are exposed; arrays and
sub-tables are skipped. The file is capped at 64 KiB; missing or unparseable
config just means every key reads as absent.

### settings page

Declare `[[settings]]` in `plugin.toml` and the plugins tab shows a settings
button for the plugin. capscr renders the fields itself and writes the answers
into `config.toml`, so the plugin reads them with `config_get` as usual:

```toml
[[settings]]
key = "webhook_url"    # [a-z0-9_], 1-48 chars; the config.toml key
label = "Webhook URL"
type = "string"        # string | bool | number | choice
default = ""           # optional; must match the type
secret = true          # masked input; still plain text in config.toml

[[settings]]
key = "size"
label = "Thumbnail size"
type = "choice"
choices = ["small", "large"]   # required for choice
default = "small"
```

A plugin declares at most 32 settings. A bad key, an unknown type, a choice
setting without choices or a default of the wrong type makes the plugin fail to
load, and the error shows on the plugins tab. Saving keeps any hand-written
keys already in `config.toml`, then reloads every plugin so the next hook sees
the new values.

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
`<plugins-dir>/<id>/plugin.wasm`, write the `plugin.toml`, then press reload
on the plugins tab (or restart capscr) to load it.

## current limits + roadmap

- host imports today: `log`, `clipboard_write_text`, `notify`, `fetch`,
  `fetch_post`, `config_get`. more arrive incrementally
- `on_capture` receives full pixels and can cancel/replace; `fetch`/`fetch_post`
  cover HTTP(S) GET/POST (no custom headers beyond content-type yet, so
  upload plugins pass auth tokens in the URL or form fields)
- capabilities for `clipboard`, `notifications`, `fetch`, `image` and
  `upload` are enforced; other declared capabilities are still informational
- per-hook fuel limit and epoch-deadline trap are active; fuel is fixed per
  hook and `time_slice_ms` only tunes the epoch deadline (500 ms by default,
  capped at 2000 ms)
- only `runtime.type = "wasm"` accepted; a native loader is not planned
