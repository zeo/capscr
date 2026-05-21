# changelog

format follows [keep-a-changelog](https://keepachangelog.com/en/1.1.0/) loosely. dates are release-tag dates.

## [unreleased]

nothing pending. drop ideas in github issues.

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
