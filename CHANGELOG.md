# changelog

format follows [keep-a-changelog](https://keepachangelog.com/en/1.1.0/) loosely. dates are release-tag dates.

## [unreleased]

nothing pending. drop ideas in github issues.

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
