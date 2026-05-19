# changelog

format follows [keep-a-changelog](https://keepachangelog.com/en/1.1.0/) loosely. dates are release-tag dates.

## [unreleased]

nothing pending. drop ideas in github issues.

## [0.3.36] — 2026-05-19

### fixed
- **hotkeys stop working after settings save** — `HotkeyManager::unregister_all` was only clearing the in-memory id→task map but never calling `GlobalHotKeyManager::unregister_all` on the OS. kernel-level registrations survived across reloads, so re-registering the same key after a save failed silently and subsequent presses were swallowed. now correctly unregisters all hotkeys from the OS before re-registering.
- **empty-hotkey tasks produced spurious error notifications** — tasks with no hotkey assigned (e.g. a freshly created task) were passed to `try_register("")`, which returned a parse error and could trigger a startup-conflict os notification. empty hotkey strings are now silently skipped.
- **11 hotkey unit tests** covering parse roundtrip, PrintScreen, empty-string skip, and the empty-error-silent behavior.

## [0.3.35] — 2026-05-19

### changed
- **default screenshot action is now save + clipboard** — fresh installs get `PrintScreen → region → save + clipboard` instead of clipboard-only. existing configs are unaffected. this matches what most users expect from a screen capture tool (file stays on disk, url/image immediately pasteable).
- **settings hotkeys pane removed** — the `[hotkeys]` config section (`hotkeys.screenshot`, `hotkeys.record_gif`) was displayed in settings but those values were never registered as global hotkeys — all hotkey registration runs through the tasks system. showing editable-but-inert fields was misleading. global hotkeys live entirely in the tasks tab now.
- **new task default is save + clipboard** — clicking `new` in the tasks tab pre-selects `save-and-clipboard` instead of `clipboard`. consistent with the fresh-install default.
- **destinations lede no longer says "https only"** — ftp doesn't use https; the incorrect note is removed.

## [0.3.34] — 2026-05-19

### added
- **ftp destination in settings ui** — the ftp upload backend has been wired since 0.3.31 but had no ui. destinations tab now shows an ftp/ftps option with fields for host, port, username, password, remote directory, explicit-tls toggle, and a public-url template (`{filename}` placeholder).
- **pre-capture delay setting** — capture → timing pane exposes `capture.delay_ms` (0–5000 ms, step 100). the backend already honored this config value; now users can set it without hand-editing `config.toml`. useful for capturing tooltips or context menus that need a moment to appear.

## [0.3.33] — 2026-05-19

### changed
- **taskbar jump list now accessible** — closing the hub via the X button now minimizes the window instead of hiding it completely. a minimized window keeps its taskbar button, so right-clicking it shows the jump list (Capture region / Capture window / Capture fullscreen / Open captures folder / Open hub). taskbar "Close window" still hides to tray; the setting label updated to match.
- **window state plugin no longer restores hub visibility** — the `tauri-plugin-window-state` was configured with `StateFlags::all()` by default, which includes `VISIBLE`. if the hub was visible when the app last quit, it would reappear at the next launch instead of staying hidden until the user clicks the tray icon. now excludes `VISIBLE` from saved/restored state.

### fixed
- **loading screen during WebView2 cold start** — first time the hub opens (or after a system reboot / cache clear), WebView2 initialises asynchronously; the window was blank white during this period. the HTML now shows `capscr · loading...` before the js bundle executes, making the wait visible rather than looking like a hang.
- **hardcoded hotkey labels updated** — history empty-state and F1 shortcuts overlay still advertised `Numpad 5` / `Pause` as the capture hotkeys. updated to `PrintScreen` / `Ctrl+Shift+G` to match the defaults shipped in 0.3.32.
- **`list_captures` stat optimisation** — was calling `path.is_file()` (extra stat syscall) after already fetching the directory entry; now uses `entry.file_type()` (cached by the kernel for most filesystems). no behavioural change.

## [0.3.32] — 2026-05-19

Driven by user testing feedback. Two real bugs that made the prior build feel "not ready to ship as a daily-driver":

### fixed
- **first capture sound was delayed 200–500 ms** — Win32 audio subsystem (waveOut) initialises lazily on first PlaySoundW. `sound::warm_audio_subsystem()` now fires in a background thread at startup via `PlaySoundW(null, SND_PURGE)`, so the first real screenshot beep is instant.
- **HDR tonemap target ignored the display's actual SDR white level** — default `hdr.brightness_nits` was 80, but `effective_params()` only auto-fills from the display when the value is ≤ 0. So on a display with the SDR slider at 300 nits, the tonemap was still targeting 80 nits, producing washed-out / clipped highlights on HDR captures. Default is now `0.0` (the documented "auto" sentinel); user-set explicit values still override. Validation now accepts 0.

## [0.3.31] — 2026-05-19

### added
- **numbered step pins in the editor** — new `[5]` tool drops auto-incrementing numbered circles (1, 2, 3, ...) at click points. ideal for annotating tutorials / bug repros. size slider 8–48 px; uses the active color. undo / redo work the same as every other op, and the next number is re-derived from existing pins so removing #3 makes the next click drop a #3 again.
- **three more annotation tools** in the editor — `[6]` line (straight stroke, no arrowhead), `[7]` ellipse (circular emphasis), `[8]` highlighter (semi-transparent multiply-blend marker, 4× stroke). brings parity with sharex's annotation set.
- **HDR badge in History** — tiles whose capture has a `<stem>.hdr.png` sidecar show an `HDR` tag next to the size / date line. raw `.hdr.png` sidecars are now hidden from the History grid (they were polluting it as duplicate-looking PNGs).
- **History filename search + type filter** — substring-match input plus `all / images / gifs / hdr` pill row. the lede flips to `N of M files match` when filtered. minimal CSS — keeps the diagnostic-console aesthetic.
- **`capscr --version` / `--help` (also `-V` / `-h`)** — invoking capscr.exe from PowerShell with these flags now prints the line and exits cleanly. uses `AttachConsole(ATTACH_PARENT_PROCESS)` so output lands in the invoking shell instead of being lost to the windows subsystem.

### changed
- **active-monitor capture follows the cursor** — `Numpad 5` / tray *Active monitor* / `--jump=fullscreen` previously always grabbed the primary display. now it grabs whichever monitor the cursor is on (both SDR and HDR paths). multi-display setups stop surprising you when the "active" monitor wasn't actually the active one. falls back to primary if the cursor query fails.

### fixed
- **HDR sidecar could attach to the wrong file** — after a clipboard-only / upload-only capture, the next save's HDR sidecar was being written next to the *previous* capture's basename. Refactored `run_post_action` to return the saved path so the sidecar is always tied to the file we just wrote.
- **DXGI ACCESS_LOST stalled HDR capture on display change** — unplugging a monitor, sleep/wake, RDP attach, or another exclusive-mode app would silently leave the capture path failing for 10 retries with no useful error. Now detect ACCESS_LOST explicitly, recreate the duplication, and surface a real "display capture is locked by another app" message after the second attempt.
- **FTP connection leaked on every error path** — `quit()` only ran on the success branch. Login / cwd / put_file failures left the socket dangling until the OS reaped it, which on some servers blocked the next upload while the slot expired. All error paths now close cleanly, and a failed `put_file` also issues a `RM` on the partial remote file so retries don't pile up corrupt artefacts.
- **corrupt config silently replaced with defaults** — if `config.toml` failed to parse or validate, the user's hand-edited file was lost on the next save. We now copy the broken file to `config.bad.YYYYMMDD-HHMMSS.toml` next to it before falling back, and log loudly.
- **hung capture accumulated worker threads** — mashing a capture hotkey while a previous capture was stuck on a held D3D11 device created one stalled thread per press. Added an atomic in-flight gate in `run_capture_pipeline` that drops new triggers until the previous one returns (or unwinds).
- **first-run silent failure when the captures folder is unreachable** — if `%PICTURE%/capscr` couldn't be created at startup (network drive, permission denied), captures failed later with no indication why. Now an OS notification fires at startup pointing the user at Settings → Output.
- **"Capture saved + copied" was lying when the clipboard was busy** — the clipboard step's error was being swallowed. The notification now says "Capture saved (clipboard busy)" if the clipboard write actually failed.
- **deleting a capture from History orphaned its HDR sidecar** — `delete_capture` only removed the SDR file, leaving `<stem>.hdr.png` on disk. Now removes both atomically (best-effort on the sidecar).
- **DXGI staging texture stayed mapped if `Vec::with_capacity` OOM'd** — the manual `Unmap` calls had to be repeated at every error path. Replaced with a `MapGuard` Drop struct so the unmap is unconditional, even on panic.
- **uploads didn't retry transient network failures** — a flaky link / 5xx burp during an imgur upload returned an error immediately. Now retries 3 times with 300ms → 600ms backoff, but only for transient markers (timeout, connection reset, 502/503/504, TLS handshake, DNS, etc) — never for auth errors or response-shape errors. New tests `transient_classifier_retries_network_failures` and `transient_classifier_skips_real_failures` lock the policy.
- **`capture.delay_ms` setting was dead code** — exposed in Settings, accepted in config, validated on save, but never actually slept. The capture pipeline now honours it: after the selector returns (or immediately for Active Monitor mode) the pipeline sleeps for `delay_ms` before grabbing pixels. Useful for capturing tooltips / menus that need a moment to appear.
- **`capture.show_cursor` toggle was equally dead** — also exposed but no code path read it. New `src/capture/cursor.rs` module fetches the live cursor via `GetCursorInfo` + `CopyIcon` + `DrawIconEx` into a 32-bit top-down DIB, converts BGRA→RGBA, and alpha-composites onto the captured `RgbaImage` at the screen-relative position (subtracting the cursor's hotspot so the tip lines up). Handles cursors that hang off the capture edge, alpha-less classic cursors, and silently no-ops on any Win32 failure — cursor compositing must never take down a capture. Origin lookup for each capture variant uses xcap's `Window`/`Monitor` accessors.

### added (continued)
- **History live-refreshes on capture** — new `capscr://capture-saved` event fires from the save path (including the OpenEditor and GIF-save branches). History.tsx subscribes and refetches with a 250ms coalesce so a rapid burst (e.g. PNG + HDR sidecar landing back-to-back) only triggers one re-read. No more hitting "reload" after every screenshot.
- **editor dirty-state guard** — Escape and the titlebar X button now warn before discarding unsaved annotations. Same pattern the Settings tab already uses. Closes a real data-loss footgun: drawing 10 arrows then Esc-ing out used to silently throw them all away.
- **`save_edited_image` writes atomically** — was overwriting the target in-place; a disk-full or permission-denied mid-write would truncate the original and lose the un-edited capture too. Now stages to `.<basename>.editing.tmp` and atomically renames, with cleanup on either failure path. Also fires `capscr://capture-saved` so the History tile picks up the new mtime.
- **hub re-opens instantly after closing** — was paying multi-second cold-boot every time the user closed and re-opened the hub (the startup prewarm only helped the very first click). Close-requested is now intercepted and the window is hidden instead, keeping the WebView2 process alive. Idle cost: ~20 MB. Next tray-click is instant.
- silence a clippy `duplicated attribute` warning on `src/jumplist.rs` — the module is already gated `#[cfg(windows)]` at the use-site, so the inner `#![cfg(windows)]` was redundant.

## [0.3.30] — 2026-05-19

### added
- README cross-links — `rot.lt/work/capscr` as homepage, `rot.lt/work/capscr/plugins` as marketplace, `lintowe/capscr-plugins` as source-of-truth registry. New `## Plugins` section pointing at `docs/marketplace.md` as the publishing contract.

### changed
- Marketplace tab empty-state copy bumped to match rot.lt's wording for a unified read across the two surfaces: *"there are no plugins to install yet — the plugin runtime (event hooks, wasm host) ships in v0.4."*

## [0.3.29] — 2026-05-18

### added
- **Marketplace client** — functional end-to-end. New `src/marketplace.rs` (registry fetch + sha256-verified plugin install + zip extraction with path-traversal defence). New Tauri commands `marketplace_browse / marketplace_install / marketplace_uninstall`. Marketplace tab rewritten with live browse / install / uninstall UI. Config field `marketplace.registry_url` (defaults to `https://rot.lt/capscr/registry.json`). Server-side contract documented in `docs/marketplace.md` + `docs/registry.example.json`.

## [0.3.25] — 2026-05-18

### added
- recording timer in the statusbar (`rec mm:ss`) — live counter once per second while a GIF is recording.
- dynamic tray-icon tooltip during recording (`capscr · recording '<task>'`).
- editor zoom controls — `Ctrl+=` / `Ctrl+-` / `Ctrl+0`, ctrl-wheel, toolbar buttons. 50/75/100/150/200/300/400% steps. canvas uses `image-rendering: pixelated` so zoomed-in pixel edits show real pixels.

### changed
- capture errors are humanised before the toast — d3d11 / missing-monitor / permission-denied / hdr / shader / clipboard / invalid-region get plain-english messages; unknown errors fall through with the raw anyhow chain so debug info isn't lost.

## [0.3.24] — 2026-05-18

### added
- `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `.github/pull_request_template.md` — required-ish hygiene for OSS grant applications.
- editor: paste-from-clipboard (`Ctrl+V`) replaces the current canvas with a pasted image.
- drag-drop now caps at 5 files per drop with an overflow toast — prevents UI thrash from 50-file dumps.

### changed
- toast container is now `<output role="status" aria-live="polite">` — screen readers actually announce uploads / errors.

## [0.3.23] — 2026-05-18

### added
- **Alt+S / Alt+T / Alt+H / Alt+D / Alt+M** for sidebar tab switching. titles advertised them; now they work.
- imgur **client-id override** in destinations — defaults to the bundled key, paste your own from api.imgur.com if you hit rate limits.

## [0.3.22] — 2026-05-18

### added
- output-directory folder picker — `[browse]` button next to the path field, opens a native folder dialog.
- **reset-to-defaults** button in Settings — loads `Config::default()` into the form (still requires Save to commit).
- gif-stop hint toast — when recording starts, a toast tells you the exact hotkey to press to stop.

### fixed
- recording-overlay flicker — `InvalidateRect(hwnd, None, true)` was erasing the bg to white before each WM_PAINT; changed to `false`. The red border no longer flashes through white frames.

## [0.3.21] — 2026-05-18

### added
- background update check + install banner. 4s after hub mount, capscr asks GitHub for the latest release. if newer than the running version, a dismissible banner across the top of the hub shows `install + restart`. silent on network failure.

## [0.3.20] — 2026-05-18

### added
- editor **redo** — `Ctrl+Y` / `Ctrl+Shift+Z`. toolbar button. redo stack clears when a new edit lands.
- first-run hint in the History empty state — tells you `Numpad 5` and `Pause` are the wired hotkeys.

### changed
- **hotkey thread is now event-driven** — replaced a 100ms `std::thread::sleep` poll loop with `crossbeam_channel::select!` on the OS hotkey channel + reload channel. zero idle wakeups. major laptop-battery win.
- **selector back buffer cached** for the lifetime of the selector window — WM_PAINT no longer allocates / frees a screen-sized GDI bitmap (~32 MB at 4K) per mouse move. fixes the "lots of flicker" report from earlier sessions.
- notifications now set the explicit AUMID `io.rot.capscr` on `notify_rust::Notification` — Windows Action Center groups toasts under "capscr" with our icon instead of the PowerShell fallback.

## [0.3.19] — 2026-05-18

### added
- **dirty-state guard** — tab switches and window close prompt to confirm if there are unsaved settings changes. `<edit unsaved>` segment lights up in the statusbar while edits are pending.
- toast / upload-card arrays are capped (8 / 6) so error storms can't pile up DOM nodes.
- startup hotkey conflicts now raise an OS notification — they used to only log to tracing.

## [0.3.18] — 2026-05-18

### added
- **Windows taskbar jump list** — right-click the hub's taskbar button to get `Capture region` / `Capture window` / `Capture fullscreen` / `Open captures folder` / `Open hub`. items launch `capscr.exe --jump=<kind>`; `tauri-plugin-single-instance` forwards argv to the running instance.
- explicit `AppUserModelID` set early in `main()` so notification toasts and the jump list anchor to the same app identity.

## [0.3.17] — 2026-05-18

### changed
- **WebView2 pre-warm at startup** — hub window is created hidden during `setup()` so the first tray click is `window.show()` (instant) instead of a cold-start `WebViewWindowBuilder::new` (multi-second, observed >1min on some systems).

## [0.3.16] — 2026-05-18

### added
- **diagnostic-console redesign** of the hub UI — corner registration marks, dot-grid content background, pipe-separated statusbar segments, inline-rule section heads, bracketed-key sidebar nav (`[s] settings` etc).
- new master 4K icon source — installer-header and installer-sidebar BMPs are lanczos-downscaled from it at exact NSIS dimensions (150×57 and 164×314) for crisp installers.

## [0.3.15] and earlier

see commit log: `git log --oneline v0.3.15`.
