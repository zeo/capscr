# changelog

format follows [keep-a-changelog](https://keepachangelog.com/en/1.1.0/) loosely. dates are release-tag dates.

## [unreleased]

nothing pending. drop ideas in github issues.

## [0.3.31] — 2026-05-19

### added
- **numbered step pins in the editor** — new `[5]` tool drops auto-incrementing numbered circles (1, 2, 3, ...) at click points. ideal for annotating tutorials / bug repros. size slider 8–48 px; uses the active color. undo / redo work the same as every other op, and the next number is re-derived from existing pins so removing #3 makes the next click drop a #3 again.
- **`capscr --version` / `--help` (also `-V` / `-h`)** — invoking capscr.exe from PowerShell with these flags now prints the line and exits cleanly. uses `AttachConsole(ATTACH_PARENT_PROCESS)` so output lands in the invoking shell instead of being lost to the windows subsystem.

### changed
- **active-monitor capture follows the cursor** — `Numpad 5` / tray *Active monitor* / `--jump=fullscreen` previously always grabbed the primary display. now it grabs whichever monitor the cursor is on (both SDR and HDR paths). multi-display setups stop surprising you when the "active" monitor wasn't actually the active one. falls back to primary if the cursor query fails.

### fixed
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
