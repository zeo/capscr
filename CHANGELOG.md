# changelog

format follows [keep-a-changelog](https://keepachangelog.com/en/1.1.0/) loosely. dates are release-tag dates.

## [unreleased]

nothing pending. drop ideas in github issues.

## [0.5.21] — 2026-06-04

### security
- upgraded russh 0.49 → 0.60.3, clearing RUSTSEC-2026-0154 (unbounded 32-bit allocation in russh) and RUSTSEC-2026-0153 (russh-cryptovec allocation handling). russh 0.60 requires a crypto backend; the default `aws-lc-rs` is disabled (aws-lc-sys fails to build on the current MSVC) in favour of `ring`. the SFTP client was migrated to the 0.60 API: native-async `Handler` (no `async-trait`), `authenticate_*` now return `AuthResult`, and `PrivateKeyWithHashAlg::new` is infallible

## [0.5.20] — 2026-06-04

### added
- `CAPSCR_FAST_HDR=1` opt-in that skips the fixed ~10ms settle sleep before the first DXGI frame acquire on the CPU-HDR capture path, trimming that latency from HDR captures. off by default — some drivers may rely on the settle time — but the acquire loop and black-frame retry already recover a stale first frame, so it is safe to enable and verify on real HDR hardware

## [0.5.19] — 2026-06-04

### changed
- keyboard focus now uses a greyscale focus ring matching the instrument aesthetic on the sidebar nav, sub-tabs, filter pills, statusbar buttons, history tiles and tile actions, titlebar buttons, and checkboxes, instead of the browser's default (clashing) ring. mouse interaction is unaffected — the ring only shows for keyboard focus (`:focus-visible`)

## [0.5.18] — 2026-06-04

### changed
- added end-to-end regression tests for the HDR tonemap dispatch path (scRGB f16 decode → tonemap routing), which the existing tonemap-math tests bypassed. these lock in byte-stable HDR screenshot output across the capture-path performance work

## [0.5.17] — 2026-06-04

### performance
- removed a leftover diagnostic byte-scan from the scRGB HDR capture path. it scanned up to ~1 MB of the raw frame (and, on an all-black frame, the entire multi-MB buffer) and formatted a hex dump on every HDR capture, purely to log raw-buffer stats — a temporary aid added in 0.3.57 that is no longer needed now HDR capture is confirmed. HDR screenshot output is byte-identical (only logging was removed)

## [0.5.16] — 2026-06-04

### performance
- removed more redundant full-frame image clones: copying a history capture or an edited image to the clipboard and uploading an edited image now move the decoded buffer (`into_rgba8`) instead of cloning it, and JPEG saves convert RGBA→RGB in a single pass instead of cloning into a `DynamicImage` first

## [0.5.15] — 2026-06-04

### performance
- copying a capture to the clipboard no longer clones the whole frame first — the pixels are passed to the clipboard backend by reference, removing a redundant full-frame allocation (tens of MB at 4K) from the default capture-to-clipboard path

## [0.5.14] — 2026-06-04

### changed
- hardened the parallel-capture serial-conversion flag to clear via RAII so it resets even if a capture worker panics (defensive — the capture workers are one-shot today, so this was a latent footgun rather than an active bug)

## [0.5.13] — 2026-06-04

### fixed
- fixed a potential deadlock introduced with the 0.5.12 parallel capture: the HDR-capture serialization lock was held across `capture_one_monitor`, which can re-enter itself through the SDR fallback when the point-based and adapter-based HDR-detection queries momentarily disagree, taking the non-reentrant lock twice on one thread. the lock now guards only the shared-device DXGI readback, so it is never held across that re-entry — and as a bonus no longer serializes the WGC path, which uses its own per-call device

## [0.5.12] — 2026-06-04

### performance
- multi-monitor freeze-frame capture (the snapshot behind region/window/fullscreen capture mode) now grabs the displays concurrently instead of one after another, overlapping the per-display GPU readbacks and spreading the work across cores. SDR displays capture in parallel; HDR displays are captured one-at-a-time under a lock so their shared GPU device is never used concurrently, keeping HDR output byte-identical. single-monitor setups are unchanged, and per-display pixel conversions run serially inside each worker to avoid oversubscribing cores

## [0.5.11] — 2026-06-04

### performance
- the full-virtual-screen BGRA/RGBA conversions on the capture-open path (the freeze-frame swap plus the two selector overlay bitmaps) now run in parallel across cores through a shared helper instead of single-threaded scalar loops over tens of millions of pixels. output is byte-identical — guarded by a new chunk-boundary regression test — so SDR and HDR captures look exactly the same

## [0.5.10] — 2026-06-04

### fixed
- a capture fired in the brief window right after launch (e.g. an autostart plus a jump-list or hotkey shot) could run before the background plugin load finished and silently skip every `on_capture` plugin hook; the capture pipeline now briefly waits for the load to finish before dispatching. interactive region/window captures never wait (the selection time already covers the load) and the wait is bounded so a slow load can't hang the capture

## [0.5.9] — 2026-06-04

### performance
- the SDR/GDI capture path now writes opaque alpha as it copies the pixels, so `capture_one_monitor`'s opacity pass early-returns instead of scanning and then rewriting the whole frame — removing two more full-frame passes from every SDR freeze-frame and single-monitor capture (final pixels are unchanged; a desktop capture is always opaque)

## [0.5.8] — 2026-06-04

### performance
- the SDR/GDI capture path now reads the blitted pixels straight from the DIB section (after a `GdiFlush`) instead of copying them out through `GetDIBits`, removing a full-frame copy from every freeze-frame and single-monitor capture

### changed
- history thumbnails now decode asynchronously (`decoding="async"`) so a large grid renders without blocking the UI thread

## [0.5.7] — 2026-06-04

### performance
- region/window/fullscreen capture now enumerates the on-screen windows on a background thread that overlaps the freeze-frame capture, instead of running the `EnumWindows` + per-window `DwmGetWindowAttribute` walk serially on the selector's critical path, so capture mode opens sooner
- the selector's dimmed backdrop is now baked straight from the freeze frame in a single copy pass, removing the per-open full-screen `BitBlt` + software `AlphaBlend` (GDI's slow software path) that ran every time capture mode opened
- the freeze-frame `RGBA`→`BGRA` conversion for the overlay bitmap is now a single fused copy-and-swap pass instead of a full copy followed by a separate swap loop
- plugins now load (and JIT-compile) on a background thread, and jump-list registration plus output-dir validation moved off the startup thread, so the tray icon and first capture are ready immediately on launch

### changed
- dropped the duplicate `gif` crate (the direct dependency now tracks the 0.14 the image crate already pulls in rather than compiling a separate 0.13), removed the unused `reqwest` `json` feature, and removed the unused direct `imgref`/`rgb` dependencies

### added
- a lightweight CSS boot animation on the loading screen, a brief fade on hub view switches, and `prefers-reduced-motion` handling for the interface animations
- the image editor is now code-split into its own bundle chunk, so the hub window no longer ships the editor's code (and the editor window no longer ships the hub's), trimming first-paint work

## [0.5.6] — 2026-06-01

### fixed
- reverted updater public key in tauri.conf.json back to the unencrypted rotated keypair (EC8E6083D3E21CEB) to resolve signature verification mismatch with release builds signed by GitHub Actions

## [0.5.5] — 2026-05-31

### added
- added support for mouse side buttons (Mouse4/Back and Mouse5/Forward) as bindable global and focused hotkeys. this includes a background low-level mouse hook (`WH_MOUSE_LL`) to capture click events system-wide, a webview `mousedown` handler for focused capturing fallback, and bare risky-keybind whitelisting so side buttons can be assigned without requiring modifier keys

## [0.5.4] — 2026-05-31

### fixed
- resolved a keybind recording issue on the Tasks panel: low-level keyboard hooks on Windows require a valid module instance handle (`hinstance`) of the installing process when registering as a global hook (`dwThreadId = 0`). we now retrieve the current running module handle dynamically using `GetModuleHandleW` to ensure the Windows kernel consistently delivers keyboard hook events across all system environments

## [0.5.3] — 2026-05-31

### fixed
- improved HDR-to-SDR screen capture tonemapping: implemented a power-4 non-linear highlight desaturation curve to preserve richness and vibrancy in saturated HDR highlights while maintaining perfect contrast and legibility for text inside highlights

## [0.5.2] — 2026-05-29

### fixed
- resolved the intermittent black screen bug where entering capture mode occasionally blanked out the primary monitor: implemented robust retry loops and pixel-level checks scanning the mapped texture bytes (first, middle, and last row) to detect and retry on all-zeros (black) frames in both the DXGI Desktop Duplication and Windows Graphics Capture (WGC) pipelines, falling back gracefully to GDI capture on persistent failure

## [0.5.1] — 2026-05-28

### fixed
- plugin dispatch no longer holds an exclusive lock across hook execution: a slow hook (e.g. a `fetch`/`fetch_post` in `on_upload_success`, bounded at 15s) could block a concurrent capture's `on_capture` dispatch and freeze the capture. `dispatch` now takes `&self` and `AppState` holds the plugin manager behind an `RwLock`, so independent events dispatch concurrently while each plugin still serialises its own calls via its store lock
- when an `on_capture` plugin replaces the image, the HDR sidecar is now dropped: a plugin returns 8-bit SDR, so writing the original `.hdr.png` next to the modified capture paired mismatched HDR data with the new SDR image. the modified capture is saved without a sidecar (mirrors the editor overwrite behaviour)

## [0.5.0] — 2026-05-28

### added
- **image-blob plugin API**: the `on_capture` hook now receives the captured pixels (`[width][height][mode][rgba]`) and returns an `i64` — `0` continue, `<0` cancel the capture, `>0` a packed `ptr/len` of a replacement image. plugins can observe, drop, or rewrite a capture before it's saved/copied/uploaded, and replacements compose across plugins in load order
- new `image` capability gates it: `["read"]` to receive pixels, `["read","modify"]` to honour cancel/replace; without it `on_capture` isn't called. replacement images are validated (`len == 8 + w*h*4`, dims ≤ 16384, ≤ 256 MB) and anything malformed is ignored so a buggy plugin can't drop or corrupt a capture
- end-to-end WAT tests cover continue / cancel / replace and every capability-gating case
- new `fetch_post` host import (POST sibling of `fetch`) so webhook-style plugins (Discord/Slack notifiers, pastebin) can send a body + content-type. shares the `fetch` capability, the https-only/blocked-port/SSRF guards, disabled redirects, the per-hook time budget, and 1 MiB request/response caps
- new `config_get` host import + per-plugin `config.toml`: the sandbox has no filesystem, so plugins now receive user-authored settings (webhook URLs, styling, thresholds) from `<plugins-dir>/<id>/config.toml`. the host reads it at load (scalars stringified, 64 KiB cap, arrays/tables skipped) and `config_get("key")` returns the value packed as `ptr/len` (0 if absent). no capability required — a plugin reading its own config is benign

### fixed
- the plugins tab now lists installed WASM plugins: `list_installed_plugins` parsed only the legacy flat `plugin.toml` schema, so a sectioned runtime manifest (the form real WASM plugins use, with `name` under `[plugin]`) failed to parse and the plugin was silently dropped from the list. it now reads the sectioned schema first and falls back to the flat one, so both modern and legacy manifests appear

## [0.4.1] — 2026-05-28

### added
- the plugins tab now surfaces plugin **load failures**: a panel lists any plugin that failed to load at launch (bad `plugin.toml`, missing `plugin.wasm`, drive-absolute `runtime.file`, etc.) instead of the failure being a silent no-op. backed by a new `plugin_load_errors` command; the list reflects the startup load pass, so restart after fixing a plugin

### changed
- removed dead Direct2D capture code left after the 0.4.0 per-display HDR refactor: the unreachable `use_d2d` branch in multi-monitor capture and the now-orphaned `d2d_window_capture` helper. no behaviour change — both were already unreachable (`d2d_capture_at_point` itself stays, still used by the active-monitor path)

## [0.4.0] — 2026-05-28

### added
- **WASM plugin runtime ships on by default.** the `plugin-runtime` feature is now in the default build, so released binaries instantiate and run WASM plugins instead of treating every plugin as metadata-only. plugins drop a `plugin.toml` + `plugin.wasm` into `%APPDATA%\com.capscr.capscr\data\plugins\<id>\` and the host loads them at launch
- the host now dispatches all three documented hooks: `on_capture`, `on_capture_saved`, and `on_upload_success`. previously only `on_capture` fired — `on_capture_saved`/`on_upload_success` were declared but never invoked because the save/upload events were never dispatched. every save path now funnels through one `notify_capture_saved` helper and every upload through `emit_upload_success`, so the hooks can't silently miss a call site
- capability-gated host imports plugins can call (module `capscr`):
  - `clipboard_write_text(ptr, len) -> i32` — gated on `clipboard = ["write"]`
  - `notify(title_ptr, title_len, body_ptr, body_len) -> i32` — gated on `notifications = ["show"]`
  - `fetch(url_ptr, url_len) -> i64` — blocking HTTP(S) GET gated on `fetch = [...url patterns...]`; returns the response body packed as `(ptr << 32) | len`, allocated in guest memory via the plugin's `capscr_alloc`
- capabilities declared in the manifest's `[capabilities]` table are now **enforced** rather than informational — an un-granted import returns a denial code and logs a warning

### security
- plugin `fetch` is https-only (cleartext http rejected) and refuses non-web ports (22/25/445/3306/6379/…), matching the custom-upload destination's posture
- plugin `fetch` reuses the upload path's SSRF guard (`validate_resolved_host`): blocks private/loopback/link-local/cloud-metadata ranges, resolves DNS twice to defeat rebinding, and disables HTTP redirects so a 30x can't escape the initial host check
- responses are capped at 1 MiB and a single fetch is bounded by a 10s timeout (epoch interruption does not fire inside a blocking host call, so the timeout is what bounds it); the per-hook epoch budget is refreshed after the blocking call so the plugin isn't trapped on resume
- all fetches in one hook call share a 15s aggregate wall-clock budget (each call is shortened to the remaining budget), so a fetch loop can't hold the dispatch thread — and the plugin-manager lock it runs under — open indefinitely
- existing wasmtime sandbox guards remain: per-hook fuel limit, epoch-deadline trap for stalls, and a per-instance linear-memory cap
- a plugin toggled off in the UI is no longer instantiated — the host now honours the `enabled` flag in `plugin.toml`, so disabling an untrusted plugin actually stops its code from running at next launch (previously the flag was written but ignored by the runtime)
- manifest `runtime.file` validation now rejects windows drive-absolute paths (e.g. `C:/x`) in addition to `..`/leading-slash/backslash — the old string check would let `Path::join` replace the plugin dir and read an arbitrary file

### fixed
- HDR is now detected per display via `is_hdr_at_point` at each monitor/window centre, replacing the global HDR-availability + env-var gate — on a mixed HDR+SDR multi-monitor setup each capture takes the correct path for the display it lands on
- GDI capture allocates a top-down 32bpp DIB section and BitBlts with `CAPTUREBLT`, fixing pixel format/stride and capturing layered (transparent) windows correctly
- window capture resolves the target centre from the DWM extended frame bounds (falling back to `GetWindowRect`) before choosing the HDR path

### changed
- `default` cargo features are now `["sftp", "plugin-runtime"]` (was `["sftp"]`)
- D3D11 devices are pre-warmed in a background thread at startup, removing the driver-wakeup delay on the first capture
- removed the Direct2D (D2D) capture path; HDR captures now go through WGC or the CPU-HDR pipeline, with GDI as the universal fallback
- refreshed application icons and NSIS installer header/sidebar assets

### tests
- end-to-end runtime tests drive hand-written WAT modules through the live `WasmHost`: the full payload round-trip (compile → link → instantiate → `capscr_alloc` → host write → hook call → host import), fuel/epoch trapping of a runaway hook, the missing-`capscr_alloc` and unsubscribed-hook paths, and runtime capability denial of an un-granted host import (via a `wat` dev-dependency; test-only, not in the shipped binary)

## [0.3.52] — 2026-05-22

### added
- HDR PNG sidecar can now be written with HLG transfer (BT.2020 primaries, cICP 9/18/0/1) in addition to the existing PQ default (cICP 9/16/0/1). HLG is transcoded from the HDR10 PQ source by:
  1. PQ EOTF decode each u16 to linear nits in [0, 10000] (SMPTE ST 2084)
  2. normalise to HLG nominal peak (1000 nits)
  3. HLG OETF encode per BT.2100 Table 5 (ARIB STD-B67)
  4. write as 16-bit BT.2020 PNG with the appropriate cICP chunk
- precomputed 65536-entry PQ→HLG lookup table builds once per encode call so the per-pixel loop is a simple LUT lookup + endian swap
- new Settings → HDR → "output format" selector: PQ (default) or HLG
- 3 new unit tests in `hdr_png::tests`: HLG output writes cICP 9/18 correctly, PQ EOTF passes known reference points (0/1000/10000 nits at 0/0.5081/1.0 PQ encoding), HLG OETF passes BT.2100 stitch-point + endpoints

### changed
- `encode_hdr_png` signature gains an `HdrTransfer` parameter so callers explicitly pick PQ-passthrough vs HLG-transcode
- the "preserve HDR" hint in Settings → HDR now reads the active output_format dynamically — selecting HLG flips the description to "writes a 16-bit bt.2020+hlg .hdr.png sidecar"

### not-yet-shipped
- scRGB output deferred. the format is non-standard for PNG, and zero major viewers parse cICP 1/8/0/1 as scRGB-in-PNG. revisit if/when the Photoshop / Affinity pipeline catches up

## [0.3.51] — 2026-05-22

### added
- test-connection probes for the remaining two upload destinations:
  - **Imgur**: GET api.imgur.com/3/credits with the configured Client-ID. 200 = creds work (rate-limit JSON surfaced as detail), 401/403 = client-id rejected
  - **Custom HTTP**: OPTIONS request to the configured post URL with full SSRF guard (host validation + DNS resolution + private-IP rejection). 2xx/3xx/405 = endpoint reachable
- the diagnostic surface is now symmetric across all four destinations — Destinations view always has a "test connection" button next to the form for the active target

### changed
- `ConnectionTestPanel` renders whenever a report exists, not just for FTP/SFTP, so switching destinations after a probe doesn't strand the result

## [0.3.50] — 2026-05-22

### added
- "fire" button next to each task in the Tasks view. invokes the same `trigger_task` pipeline a hotkey press would, so users can dry-run a binding without leaving the hub. especially useful for PrintScreen and other keys that are awkward to press while focused on the hub window, and for tasks bound during a tray-disabled session
- new `fire_task` invoke; hides the hub before dispatch so a region/window capture overlay isn't painted over by the focused hub
- button respects `configDirty()` — disabled while there are unsaved changes, since the backend looks up tasks by id in the persisted config (firing an unsaved task would 404)

### changed
- Tasks view actions row now has two buttons (fire + delete) with the same xs-ghost styling

## [0.3.49] — 2026-05-22

### added
- "test connection" button on the FTP and SFTP forms in the Destinations view. probes the full upload path — host validation, DNS resolution, connect, login/authenticate, change to remote directory — without uploading anything. failures surface a per-step diagnostic table so users debugging credentials know exactly which step broke (e.g. `auth-publickey: server rejected the key (not in authorized_keys?)` vs `cwd: 550 No such directory`)
- new `test_upload_connection` invoke + `ConnectionTestReport`/`TestStep` types — frontend renders the result as a labelled table with ● ok / ● fail chips matching the per-task hotkey status styling
- `upload::test_connection_ftp` and `upload::test_connection_sftp` (feature-gated) mirror the upload paths step-for-step, including the same VerifyHostKey TOFU check and the publickey-then-password auth precedence shipped in 0.3.47/0.3.48

### changed
- Destinations view layout: probe report renders above the destination form when present, so the user can compare a failing step against the input that produced it without scrolling
- SFTP probe reads the remote_dir (or `.` if empty) to surface permission issues — listing a directory doesn't require write access, so this verifies "can I reach this path?" without leaving artefacts

## [0.3.48] — 2026-05-22

### added
- SSH key authentication for the SFTP destination. when `private_key_path` is set the upload path tries public-key auth first (`russh::client::Session::authenticate_publickey` against `russh::keys::key::PrivateKeyWithHashAlg`), only falling through to password auth if key auth fails AND a password is configured
- new `private_key_path`, `private_key_passphrase` (plaintext slot), and `private_key_passphrase_encrypted` (DPAPI vault) fields on `SftpUploadConfig`. passphrase gets the same migrate-on-save vault treatment as the SFTP password
- `load_private_key` helper parses OpenSSH PEM with `ssh-key`'s `PrivateKey::from_openssh`; if the key is encrypted, decrypts with the configured passphrase via `key.decrypt(...)`. friendly error on bad passphrase or missing passphrase for an encrypted key
- Destinations → SFTP form: file-picker for the private key path (filters for `.pem`, `.key`, no extension), conditional passphrase input (only shown when a key path is set), inline hints clarifying the precedence order (key → password fallback)

### changed
- `set_config` now preserves the encrypted SFTP password AND the encrypted key-passphrase blobs when the UI sends empty plaintext inputs (parity with the FTP password preservation shipped in 0.3.43)
- friendlier auth-failure error: surfaces the per-method diagnostic instead of russh's generic "authentication rejected". e.g. `"SFTP authentication failed — publickey: server rejected the key (not in authorized_keys?); password: server rejected the password"`

## [0.3.47] — 2026-05-22

### security
- SFTP no longer accepts any server key blindly. trust-on-first-use (TOFU) host-key store closes the MITM gap noted in 0.3.46's changelog: first connect to a host:port records the SHA256 fingerprint, every subsequent connect refuses to upload on mismatch with a structured "stored X, server now offering Y — forget the host in Settings → SSH if this is intentional rotation" error message
- store lives at `<config_dir>/ssh_known_hosts.toml`, written atomically via temp-rename so a crash mid-save can't truncate it; corrupt files fall back to an empty in-memory map and tracing::warn instead of crashing the upload pipeline

### added
- new `src/upload/known_hosts.rs` module (HashMap of "host:port" → { fingerprint, first_seen_unix }, TOML on disk)
- `sftp_known_hosts` and `sftp_forget_host` invokes
- Settings → SSH panel: lists every trusted host:port with its fingerprint + first-seen date, "forget" button per row to clear so legitimate key rotation isn't a dead-end
- 4 new unit tests in `upload::known_hosts::tests` (roundtrip, missing-file-as-empty, forget, corrupt-fallback)

### changed
- `upload_sftp`'s `check_server_key` handler is now `VerifyHostKey` instead of `AcceptAny`. host-key mismatch surfaces through an `Arc<Mutex<Option<String>>>` captured from the async closure so the structured message reaches the user instead of getting buried under russh's generic "connection aborted by handler" error
- `tempfile` added as a dev-dependency (used only by the new known_hosts tests)

## [0.3.46] — 2026-05-22

### added
- SFTP upload destination behind a new `sftp` cargo feature (default-on). pure-rust transport via russh + russh-sftp, no nasm/openssl-sys/libssh2 C compilation. backed by tokio's current-thread runtime inside the otherwise-blocking upload pipeline
- `SftpUploadConfig` mirrors `FtpUploadConfig`'s shape — host/port/username/remote_dir/public_url_template plus password (legacy) + password_encrypted (DPAPI vault, same treatment as the FTP path shipped in 0.3.43)
- `UploadDestination::Sftp` + `TaskUploadTarget::Sftp` so SFTP is selectable as the global destination AND as a per-task override
- tray "Upload destination" submenu now lists SFTP alongside Imgur/Custom/FTP and respects the active marker
- Destinations view: new "sftp (ssh)" panel mirroring the FTP form, with DPAPI placeholder hint when a vault blob is present
- `Config::migrate_secrets` now wraps the SFTP password too on first save after upgrade; load-time auto-migration triggers when either FTP or SFTP has a plaintext slot but no encrypted blob

### changed
- default cargo feature set now includes `sftp`. opt out with `--no-default-features` for the smallest possible binary (the `upload_sftp` shim returns a friendly "not compiled in" error in that build)
- server host-key policy on first connect: accept-any (matches the FTP path's "trust the configured host" stance). a known-host pin store is queued for a follow-up release — until then SFTP users who care about MITM should tunnel via a trusted network

## [0.3.45] — 2026-05-22

### fixed
- pressing [X] on the hub now hides to the tray instead of minimising to the taskbar. the tray (left-click) brings it back; tray → Exit remains the only true process exit. `intercept_hub_close` calls `window.hide()` instead of `window.minimize()`
- tray menu and any other native Win32 context menu now render in Windows 11's dark theme to match the hub aesthetic. previously the menu was light-on-grey with white submenu fly-outs that clashed with the dark monospace UI

### added
- `src/win_darkmode.rs` — opt-in via `uxtheme.dll!SetPreferredAppMode(AllowDark)` (ordinal 135). same approach used by Edge, Notepad, Visual Studio. resolved at runtime via `LoadLibraryW + GetProcAddress`, so Win10 builds older than 1903 silently fall back to light menus instead of crashing
- `FlushMenuThemes` follow-up call so menus painted before the toggle pick up the new mode

## [0.3.44] — 2026-05-22

### fixed
- global hotkeys now win against other screen-capture tools that may have registered the same chord. previously `RegisterHotKey` lost the race silently when another app had already claimed the binding (notably `PrintScreen`), and capscr offered no signal that registration had failed
- replaced the `RegisterHotKey` dispatch path with a `WH_KEYBOARD_LL` low-level keyboard hook on a dedicated thread (`src/hotkeys/ll_hook.rs`). LL hooks fire ahead of `RegisterHotKey` for any process, so capscr wins universally against competing tools that take the legacy path. matched key presses are consumed (`LRESULT(1)`) so the loser tool also doesn't double-fire
- per-hook dispatcher thread debounces key auto-repeat (250ms window) — holding the chord no longer queues multiple captures
- `capscr-llkeyboard` thread runs a `GetMessage` pump as required by Windows for LL hook callbacks

### added
- `disabled_globally` field on `HotkeyConfig` — the tray "Disable all hotkeys" toggle now persists across restarts (previously reset to enabled every cold start, contributing to the "hotkeys appear broken" reports)
- statusbar `keys off` chip in the hub when the kill switch is active — click to re-enable in one tap (`App.tsx`)
- per-task hotkey status (`● live` / `● rejected`) inline in the Tasks view, sourced from a new `capscr://hotkey-status` event the backend emits after every registration pass — silent rejections (risky-bare, parse fail) are now visible without digging through logs
- new `hotkey_diagnostics` invoke + Settings → Hotkeys panel: kill-switch button + per-binding status table with rejection reasons
- new `set_hotkeys_disabled` invoke (the hub statusbar + Settings panel call this; tray toggle now routes through it too for a single source of truth)

### changed
- `HotkeyManager` is now a pure binding registry (no `GlobalHotKeyManager` field) — the LL hook is the only event source. `try_register` still validates chords + records errors as before; the live binding set is flushed to the hook via `HotkeyManager::flush_to_hook`
- tray toggle handler reuses `commands::set_hotkeys_disabled` instead of mutating `AtomicBool` directly, so config persistence + LL hook state stay in sync
- LL hook test coverage: modifier-vk recognition, binding insertion/readback, enabled toggle (3 new unit tests in `hotkeys::ll_hook::tests`)

## [0.3.43] — 2026-05-22

### security
- **wasmtime 29 → 43**: clears 16 RUSTSEC advisories (2 critical sandbox-escape, 14 lower) reported by `cargo audit` against the 0.3.42 lockfile
- WASM host hardened: per-hook epoch deadline (default 500ms) + per-hook fuel budget (5M instructions) + background bumper thread incrementing the engine epoch every 10ms. Plugins that block or busy-loop now trap instead of freezing the capture path
- WASM Config: dropped `wasm_threads`/`wasm_reference_types` calls (removed in wasmtime 43) — features are off by default with the `cranelift + runtime + std` feature set
- **DPAPI vault for FTP password**: new `src/secret.rs` wraps Win32 `CryptProtectData` / `CryptUnprotectData`. Bound to the current user account — copying config.toml elsewhere makes the blob unrecoverable
- `FtpUploadConfig::password_plaintext()` reads encrypted first, falls back to plaintext for not-yet-migrated configs
- `Config::load` auto-migrates plaintext FTP passwords on first launch after upgrade (encrypt + clear plaintext + save)
- `set_config` preserves the encrypted blob when the UI sends an empty password input (so Save with an untouched form doesn't wipe the vault)

### added
- `cargo` feature gate clarified: `plugin-runtime` builds the WASM host; default build still excludes it
- DPAPI roundtrip unit test (`secret::tests::roundtrip`)

### changed
- frontend Destinations: FTP password input now shows `"(stored — leave blank to keep current)"` placeholder when an encrypted blob exists; field-hint says "encrypted at rest with Windows DPAPI (per-user)" instead of the old "stored in config.toml (plaintext)"
- `show_notification` deduplicates identical (title, body) pairs fired within 1.5s — retry-loop notification storms ("Capture saved", "Clipboard busy") now collapse to a single toast

## [0.3.42] — 2026-05-22

### added
- 0.4 WASM plugin runtime foundation, behind `plugin-runtime` cargo feature (default build unchanged)
- `src/plugin/manifest.rs`: serde-backed `plugin.toml` schema (id, name, version, runtime{type/file/memory_max/time_slice_ms}, hooks{}, capabilities{})
- `src/plugin/wasm.rs`: wasmtime-backed host. exposes `capscr.log(level, ptr, len)`; loads `plugin.wasm`, resolves manifest-declared hooks, owns the Store + Memory + capscr_alloc handle
- `PluginManager::load_all` scans `%APPDATA%/com.capscr.capscr/data/plugins/<id>/`, parses each manifest, instantiates WASM plugins under the feature flag
- `docs/plugin-runtime.md`: full author guide — manifest schema, hook signatures, host imports, minimal Rust cdylib example

### changed
- `wasmtime 29` back as an optional dep (was removed in 0.3.1 — security-advisory liability with no consumer; now consumed by the host)
- existing metadata-only plugins keep working: a plugin without a `[runtime]` section is listed but never instantiated

## [0.3.41] — 2026-05-22

### changed
- NSIS installer ships with a custom template (`installer/installer.nsi`) — patched from tauri-bundler 2.9.1's default
- `Page custom PageReinstall PageLeaveReinstall` registration removed: manual reinstalls overwrite in place instead of prompting "uninstall current version before installing"
- `MUI_BGCOLOR 0d0d0d` + `MUI_TEXTCOLOR c4c4c4` + `MUI_BRANDINGTEXT "capscr"` for a darker page chrome (MUI2 only — native list/edit controls still follow Windows theme)
- installer sidebar BMP regenerated: 80×80 icon centred in upper third on a 164×314 dark fill (was a 164×164 close-up that read as a stock-photo camera-lens crop)
- installer header BMP regenerated: 32×32 icon flush-left at y=12 on a 150×57 dark strip (was a 50×50 icon with a 3px corner gap)

### fixed
- `scripts/build-hook.mjs` candidate-loop now falls through on cmd.exe's "is not recognized" status as well as ENOENT — the `tauri`/`cargo tauri` resolution works under both PowerShell and `cmd /c` shells

## [0.3.40] — 2026-05-21

### added
- tray menu: **Recent uploads** submenu (last 5 URLs, click to copy)
- tray menu: **Upload destination** switcher submenu (●/○ marker on current choice)
- tray menu: **Open hub →** submenu with direct entries for Settings / Tasks / History / Destinations / Plugins. tray fires `capscr://goto-tab` so the hub lands on the chosen tab
- tray menu: **Disable / Enable all hotkeys** stateful toggle (replaces the panic unbind — disabling no longer clears bindings from config, just unregisters them with the OS)

### changed
- tray menu rebuilds on every state-changing event (upload landed, destination switched, hotkeys toggled) so the visible items always reflect current state
- `set_config` respects the tray's hotkey-disabled toggle — saving the hub no longer silently re-registers hotkeys you turned off from the tray

### fixed
- `record_upload` ring (cap 5) replaces a single `last_upload` slot; back-compat with the existing Copy-last-URL tray path

## [0.3.39] — 2026-05-21

### fixed
- save button now lights up (filled paper) when there are unsaved changes; greyed otherwise so you know when there's nothing to commit
- bare letter/digit hotkeys are now rejected at registration (frontend + backend). previously, binding `T` stole the key globally and locked you out of typing it anywhere else
- application-window capture retries once after 80 ms when xcap transiently misses the window during z-order shuffles
- friendlier toast when the selected window vanishes before capture ("the selected window vanished before we could capture it…")

### added
- tray menu: **Unbind all hotkeys (panic)** — clears every task hotkey and persists, recovery path if a bound key ever traps you
- save button gets a primary `paper` variant for any view with a configDirty signal (settings / tasks / destinations)

### changed
- dropped the decorative `+` corner registration marks. statusbar gets a touch more right-edge padding so help/version don't crowd the window chrome
- `HotkeyInput` keeps capturing after a risky bind is rejected, with an inline warning explaining the modifier requirement

### known
- NSIS installer still uses Windows default light theme and shows the "Already Installed" page on manual reinstall — full installer redesign queued. Auto-updates (post 0.3.39) bypass the installer entirely so most upgrades won't see it.

## [0.3.38] — 2026-05-20

### security
- ssrf coverage extended to ipv6 ula, link-local, ipv4-mapped (is_private_ip + bracket-stripping is_private_ip_string)
- redirect chains revalidate every hop (custom reqwest policy replacing Policy::limited)
- imgur response url validated before clipboard / display (validate_returned_url)

### fixed
- editor save overwrites wrong file when reopened (Editor.tsx reused stale image state)
- windows backslashes break asset:// urls (slash-normalised before encodeURIComponent)
- simultaneous captures could overwrite each other (get_unique_filepath now uses create_new)
- active gif recording saved on app exit (tray exit routes through exit_app)
- install_update waits up to 5s for in-flight captures before restart
- save_edited_image rejects empty byte arrays
- finalize_gif_recording polls RecordingState::Processing instead of sleeping 250ms
- duplicate non-empty hotkeys rejected at config load (matches frontend guard)
- hotkey channel close surfaced as error notification, not debug log
- post-capture upload action shows a notification with the url
- reupload_capture sends raw bytes with correct mime (was re-encoding everything as png)
- history sort honours millisecond mtime precision (modified_unix treated as ms)
- gif task acquires capture gate before opening the selector (prevents overlay stacking)
- hotkey reload conflicts emit an os notification (matches startup path)
- history reupload button shows a timed error flash on failure
- editor paste shows a status when non-image content is pasted
- editor paste contributes to dirty-state (close confirm distinguishes pasted vs annotated)
- color picker plays the screenshot sound (was the only silent post-capture path)
- overlay gdi init validates CreateCompatibleDC / CreateCompatibleBitmap return values
- marketplace buttons lock during any in-flight op (busyId() guard across install/uninstall/enable/disable)
- settings filename template hint corrected; quality + hdr float inputs bounds-checked
- gif recorder file handle dropped before post-write size check (windows lock timing)
- stop_gif_recording signals encoder before clearing recording_task_id (finalize-before-stop race)
- PlaySoundW return value logged at warn instead of discarded
- copy_url_to_clipboard goes through ClipboardManager retry path; CopyToClipboard is non-fatal on contention
- config writes are atomic (.toml.tmp + rename)
- ftp stream closed on build_url failure after successful put
- tasks save shows duplicate-hotkey validation errors instead of silent overwrite
- history + app toast timeouts cleared in onCleanup

### changed
- PostActionArg::Prompt opens the editor instead of silently saving + copying

## [0.3.37] — 2026-05-19

### fixed
- per-task upload target now actually works (task.target_destination wired through run_capture_pipeline + apply_gif_post_action)
- hdr sidecar deleted on editor save (stale <stem>.hdr.png no longer left next to overwritten sdr)
- second gif-task hotkey shows a toast instead of silently doing nothing
- ftps checkbox disabled in ftp ui (backend rejects use_tls=true; label notes ftps planned for v0.4)
- ftp port field rejects non-numeric input (NaN guard before patching config)
- tasks auto-set upload target to imgur when switching post-action to "upload"
- deleteCapture error shown as inline flash for 6s (was silently swallowed)

## [0.3.36] — 2026-05-19

### fixed
- hotkeys stop working after settings save (HotkeyManager::unregister_all now calls GlobalHotKeyManager::unregister_all on the OS)
- empty-hotkey tasks no longer trigger spurious startup-conflict notifications
- 11 hotkey unit tests covering parse roundtrip, PrintScreen, empty-string skip, silent-empty-error

## [0.3.35] — 2026-05-19

### changed
- default screenshot action is now save + clipboard on fresh installs
- settings hotkeys pane removed (the [hotkeys] config values were never registered; all hotkeys live in the tasks system)
- new task default is save + clipboard
- destinations lede no longer says "https only" (ftp doesn't use https)

## [0.3.34] — 2026-05-19

### added
- ftp destination in settings ui (host, port, username, password, remote dir, explicit-tls, public-url template)
- pre-capture delay setting in capture → timing pane (0–5000 ms, step 100)

## [0.3.33] — 2026-05-19

### changed
- taskbar jump list accessible (hub X minimises instead of hides; right-click taskbar shows jump list)
- window-state plugin no longer restores hub visibility (StateFlags excludes VISIBLE)

### fixed
- loading screen during WebView2 cold start (`capscr · loading...` until js bundle executes)
- list_captures uses entry.file_type() instead of path.is_file() (one less stat syscall)

## [0.3.32] — 2026-05-19

### fixed
- first capture sound delayed 200–500 ms (sound::warm_audio_subsystem fires PlaySoundW(null, SND_PURGE) at startup)
- hdr tonemap target ignored display SDR-white (default brightness_nits=0.0 sentinel; effective_params auto-fills only when ≤0)

## [0.3.31] — 2026-05-19

### added
- numbered step pins in editor ([5] tool, auto-incrementing, size 8–48 px)
- three more annotation tools: [6] line, [7] ellipse, [8] highlighter
- HDR badge in History; raw .hdr.png sidecars hidden from the grid
- history filename search + type filter (all / images / gifs / hdr)
- `capscr --version` / `--help` (and short forms) via AttachConsole(ATTACH_PARENT_PROCESS)
- history live-refreshes on capture (capscr://capture-saved event, 250ms coalesce)
- editor dirty-state guard on Escape and titlebar X
- save_edited_image writes atomically (.editing.tmp + rename)
- hub re-opens instantly after closing (close-requested hidden, WebView2 stays warm; +20 MB idle)

### changed
- active-monitor capture follows the cursor (was always primary); falls back on cursor-query failure

### fixed
- HDR sidecar no longer attaches to the wrong file (run_post_action returns the saved path)
- DXGI ACCESS_LOST detected, duplication recreated, surfaced as "display capture is locked"
- ftp connection cleanly closed on every error path; failed put_file removes partial remote file
- corrupt config copied to config.bad.YYYYMMDD-HHMMSS.toml before falling back to defaults
- in-flight capture gate prevents stacked worker threads on hotkey mash
- first-run silent failure when captures folder unreachable now raises an os notification
- "saved + copied" no longer claimed when clipboard write failed
- deleting a capture removes its HDR sidecar atomically
- DXGI staging texture unmap via MapGuard Drop (was leaked on Vec::with_capacity OOM)
- uploads retry transient network failures 3x with 300→600ms backoff (timeout, reset, 5xx, TLS, DNS); auth/shape errors not retried
- capture.delay_ms setting now honoured (was dead code)
- capture.show_cursor setting now honoured (new src/capture/cursor.rs; GetCursorInfo + DrawIconEx alpha-composite)
- clippy `duplicated attribute` on src/jumplist.rs silenced (module is already cfg(windows) at use-site)

## [0.3.30] — 2026-05-19

### added
- README cross-links to rot.lt and new `## Plugins` section pointing at docs/marketplace.md

### changed
- marketplace empty-state copy matches rot.lt wording for unified read

## [0.3.29] — 2026-05-18

### added
- marketplace client end-to-end (src/marketplace.rs, sha256-verified install, path-traversal-defended unzip)
- tauri commands: marketplace_browse / marketplace_install / marketplace_uninstall
- marketplace tab rewritten with live browse/install/uninstall
- config.marketplace.registry_url (defaults to https://rot.lt/capscr/registry.json)
- docs/marketplace.md and docs/registry.example.json

## [0.3.25] — 2026-05-18

### added
- recording timer in statusbar (`rec mm:ss`)
- dynamic tray-icon tooltip while recording (`capscr · recording '<task>'`)
- editor zoom: Ctrl+=, Ctrl+-, Ctrl+0, ctrl-wheel, toolbar buttons; 50–400% steps; image-rendering: pixelated

### changed
- capture errors humanised before the toast (d3d11 / missing-monitor / permission-denied / hdr / shader / clipboard / invalid-region)

## [0.3.24] — 2026-05-18

### added
- CONTRIBUTING.md, CODE_OF_CONDUCT.md, .github/pull_request_template.md
- editor paste-from-clipboard (Ctrl+V replaces canvas)
- drag-drop capped at 5 files per drop with overflow toast

### changed
- toast container uses `<output role="status" aria-live="polite">` for screen-reader announcement

## [0.3.23] — 2026-05-18

### added
- Alt+S / Alt+T / Alt+H / Alt+D / Alt+M sidebar tab switching (titles advertised them; now they work)
- imgur client-id override in destinations (defaults to bundled key)

## [0.3.22] — 2026-05-18

### added
- output-directory folder picker ([browse] next to path field)
- reset-to-defaults button in Settings
- gif-stop hint toast naming the stop hotkey when recording starts

### fixed
- recording-overlay flicker (InvalidateRect now passes erase=false)

## [0.3.21] — 2026-05-18

### added
- background update-check + install banner (4s after hub mount; silent on network failure)

## [0.3.20] — 2026-05-18

### added
- editor redo (Ctrl+Y / Ctrl+Shift+Z, toolbar button; redo stack clears on new edit)
- first-run hint in History empty state naming the wired hotkeys

### changed
- hotkey thread is event-driven (crossbeam_channel::select! replaced 100ms poll loop)
- selector back-buffer cached for selector window lifetime (was allocating ~32 MB GDI bitmap per WM_PAINT)
- notifications set explicit AUMID `io.rot.capscr` so Action Center groups under capscr

## [0.3.19] — 2026-05-18

### added
- dirty-state guard on settings tab switches + window close; `<edit unsaved>` segment in statusbar
- toast + upload-card arrays capped (8 / 6) to prevent DOM pileups
- startup hotkey conflicts raise an os notification

## [0.3.18] — 2026-05-18

### added
- windows taskbar jump list (Capture region / window / fullscreen / Open captures folder / Open hub) via `--jump=<kind>` + tauri-plugin-single-instance argv forwarding
- explicit AppUserModelID set early in main() so toasts + jump list share identity

## [0.3.17] — 2026-05-18

### changed
- WebView2 pre-warm in setup() so first tray click is window.show() instead of cold start

## [0.3.16] — 2026-05-18

### added
- diagnostic-console redesign of the hub ui (corner registration marks, dot-grid bg, pipe-separated statusbar, bracketed-key sidebar nav)
- new master 4K icon source; installer-header (150×57) + installer-sidebar (164×314) lanczos-downscaled from it

## [0.3.15] and earlier

see commit log: `git log --oneline v0.3.15`.
