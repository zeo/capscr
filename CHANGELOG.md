# changelog

format follows [keep-a-changelog](https://keepachangelog.com/en/1.1.0/) loosely. dates are release-tag dates.

## [Unreleased]

### fixed
- **fullscreen game capture on Wayland.** the wayland still chain now ends in a screencast source, so grabbing a fullscreen game no longer fails when the one-shot screenshot apis return an incomplete frame. this hits KWin on NVIDIA in particular, where a direct-scanned-out fullscreen buffer reads back empty through ScreenShot2 (and therefore through the screenshot portal too). screencast pulls the frame off a pipewire node instead, so it works where the cheaper sources give up. it stays the last resort behind kwin-screenshot2, ext-image-copy, wlr-screencopy, and the screenshot portal, and only trips a one-time source picker (the choice is remembered by restore token after that), so normal desktop captures keep taking the fast path with no prompt.

## [0.5.45] - 2026-07-18

### added
- **Linux support.** capscr now runs on Linux (X11 and Wayland), shipped as deb, rpm, and AppImage packages, with the auto-updater tracking the AppImage and walking deb/rpm installs through their native update. what carries over:
  - the whole still-capture pipeline: the freeze-frame selector (region drag, window pick, alt+click color picker, shift aspect snap, ctrl fine-tune, loupe) across every monitor, all capture modes, history, uploads, the editor, and pinning. on X11, KDE, and wlroots the selector picks windows itself; GNOME routes window mode through the portal's own picker
  - region GIF and MP4 recording on X11 and Wayland with the same wall-clock playback guarantees as Windows, and system audio off the PulseAudio/PipeWire monitor of the default output
  - global hotkeys everywhere: X11 grabs, the GlobalShortcuts portal on Wayland, and an opt-in evdev fallback for sessions with neither; the tray menu and `capscr --jump` work regardless
  - Wayland pixels come from the best source the compositor offers (KWin ScreenShot2, ext-image-copy-capture, wlr-screencopy, or the screenshot portal), including sessions running without XWayland; the recording bar and pins stay above other windows via layer-shell or plasma-shell
  - upload credentials live in the system keyring (freedesktop Secret Service) instead of on disk; OCR runs through tesseract; copying a capture as a file uses text/uri-list; capture and upload cues play through the system audio stack; the one-click ffmpeg download fetches a static build verified against a pinned sha256
  - the few remaining platform boundaries (HDR capture, GNOME keep-above and tray) are listed in `docs/platform-limits.md`, with `capscr --wayland-diag` reporting which apply to a given session
- linux CI: the test suite and clippy now run on ubuntu alongside the windows suite, with the capture path exercised against a real X server
- a **copy detected text (OCR)** post-action: bind it to a still-capture task and capscr runs OCR on the shot (the built-in Windows engine, tesseract on Linux) and drops the recognized text straight onto your clipboard — no file saved, no editor round-trip. (OCR was already reachable from the history grid; this makes it a one-step capture action.)
- an optional **per-task pre-capture delay**: each still-capture task can set its own delay (blank uses the global one), so a timed task for catching a menu or tooltip can sit alongside your instant hotkeys instead of the delay being all-or-nothing.

- **HDR capture on GNOME 50+**: fullscreen captures of a monitor running HDR now pull a 10-bit PQ frame off the screencast portal and go through the same tonemap (and optional HDR-preserved PNG) pipeline as Windows, instead of settling for the compositor's 8-bit screenshot; the hdr settings pane and history filter appear on sessions that support it
- a **GNOME companion extension** (installable from Settings → general on a GNOME session) that closes the three GNOME gaps at once: window-mode capture picks windows in capscr's own overlay instead of GNOME's portal dialog, the recording bar and pinned screenshots stay above fullscreen windows and land where they should, and a top-bar capture menu stands in for the missing system tray
- linux recordings show the same blinking red frame around the recorded region as Windows, sitting just outside the captured pixels and passing clicks through; it appears on X11, KDE, wlroots compositors, and GNOME with the companion extension
- on Plasma 6.7+, the recording control bar is excluded from capture the same way it is on Windows: KWin removes it from screenshots and recordings compositor-side, so it sits below or above the region and tucks inside it when there is no room, never appearing in the frames or fleeing to another monitor

### fixed
- a linux recording's first frame no longer catches the region selector mid-teardown (its dim layer and size label could land in frame one); the first grab now waits out the compositor's close animation
- the HDR-preserved PNG post-action actually writes an HDR PNG again: the encoder expected 16-bit pixel data while every capture source delivers packed 10-bit, so it silently fell back to SDR output every time
- pinned screenshots no longer vanish from captures on Plasma 6.6.1+ — KWin started hiding all of a screenshotting app's own windows by default, and capscr now asks it not to, keeping pins in your shots like every other window
- when fast GDI capture fails on Windows, the xcap fallback now resolves the monitor by position instead of an id that the two enumerators never shared

## [0.5.43] — 2026-07-11

a hardening and polish pass across the whole app. if you're coming from 0.5.40, this also carries the 0.5.41 and 0.5.42 changes (the recording overhaul — flat memory use, wall-clock playback timing, system-audio sync — plus the native installer and the earlier fixes).

### security
- a plugin's network fetch now connects to the exact address that was validated instead of re-resolving the hostname, closing a dns-rebinding window that could otherwise reach an internal or cloud-metadata address
- a plugin fetch capability wildcard has to sit on a path boundary (`https://host/*`), so a granted host can no longer silently match a look-alike domain like `host.attacker.tld`
- the webview's asset scope no longer includes the whole roaming appdata tree — only capscr's own data, so a mis-scoped image reference can't read another app's files
- a marketplace plugin that declares capabilities (image read, network fetch, clipboard, notifications) now installs **disabled**; it stays inert until you review those permissions in the plugins tab and enable it. capability-free plugins install ready to use
- plugin zips are now capped by the bytes actually written, not the archive's self-declared sizes, so a small download can't decompress to gigabytes and fill the disk
- the auto-downloaded ffmpeg is verified against its published sha256 before capscr trusts and runs it
- the ocr, trim, pin, and drag-drop-upload commands are held to the same output+history file allow-list as the other capture-file actions
- the wasm plugin host no longer leaks a small allocation on every plugin memory growth

### fixed
- a single missing or unknown field in `config.toml` no longer wipes every task and stored credential — missing fields fill from defaults and the file is repaired in place
- the global hotkey kill switch is no longer silently re-enabled by the next settings save
- switching the upload destination from the tray no longer discards unsaved edits open in the hub
- a full-height region recording no longer bakes the timer/stop control bar into the gif/mp4
- releasing the mouse while holding shift now commits the aspect-locked selection instead of leaving the overlay open, and the committed rectangle matches the last one drawn
- the selection overlay's keyboard (escape, arrow-nudge, enter) reaches the overlay reliably even when another window held focus
- the magnifier loupe flips against the monitor the cursor is on instead of straddling a bezel onto the next monitor
- a selection anchored at the top-left desktop pixel is no longer mistaken for no selection
- editor save/copy/upload stay disabled until an image is actually loaded, so a stale canvas can't overwrite a newly opened file
- the statusbar capture count updates after a screenshot instead of freezing at its startup value
- the update banner sits above the view instead of covering its title
- numeric settings clamp to their range when you commit a field, so what's shown always matches what's saved
- gdi and d3d device handles no longer leak on capture error paths, and a poisoned hdr device cache recovers instead of panicking

### changed
- the tasks view applies each edit immediately (its save button could never enable) and deleting a task now arms-to-confirm
- history shows a still first frame and plays a clip on hover, instead of autoplaying every video at once
- escape closes the trim modal and pinned windows
- the recording elapsed clock and the startup d3d11 prewarm run only when actually needed (recording in progress / an hdr display present), trimming idle work
- mp4 encoding no longer clones each frame on the common no-resize path
- capscr no longer adds itself to windows startup without consent — turn it on in settings
- the inert ftp "use tls" checkbox and the dead `minimize_to_tray` setting were removed

## [0.5.42] — 2026-07-11

### added
- a **region (last)** capture mode: bind it to a task and it re-fires your previous selection rectangle with no selector, so re-grabbing the same panel doesn't mean re-dragging. the first use with no stored rectangle falls back to a normal region drag and remembers it
- the plugins tab now shows what each installed plugin was granted (clipboard, image read/modify, notifications, and each allowed fetch host), so you can see what a plugin can reach before enabling it

### changed
- the ftp destination no longer advertises ftps in the dropdown, section title, or the tls hint — ftps isn't implemented, so the copy now points at sftp for an encrypted transfer
- an mp4 endpoint typed as `http://` for the s3 uploader is refused before the request, so the sigv4 credentials and image never go out in the clear

### fixed
- quitting capscr during an mp4 recording no longer loses the whole clip: it saved only gif recordings and dropped mp4s silently. it now saves the mp4 and, if the save fails, says so
- upload redirects and connections are hardened against SSRF: a server can no longer redirect an upload to an internal address (the redirect target is resolved and checked, not just string-matched), and ftp/sftp/http connections dial the exact address that was validated instead of re-resolving the hostname, closing a DNS-rebinding window. the marketplace registry and plugin downloads go through the same guard
- a plugin can no longer declare a catch-all `https://*` fetch permission — network access has to name concrete hosts
- the mouse cursor no longer has a dark halo in screenshots and recordings: it was composited with the wrong alpha math, darkening anti-aliased edges and drop shadows
- one bad value in `config.toml` no longer wipes every setting for the session — the file is repaired in place (out-of-range HDR values clamped, malformed or duplicate task hotkeys dropped/unbound) instead of falling back to defaults
- a saved capture is announced only after the file is actually written; a failed background save now removes the empty placeholder and shows an error instead of a false "capture saved"
- a recording cut short by a disk-write failure, and an mp4 saved without its system-audio track, now each surface a note instead of a silent truncation or a silent drop
- the audio temp file no longer lingers in `%TEMP%` when an mp4 save fails
- captures saved to the history folder can now be revealed in explorer and opened in the editor, not just deleted or copied
- the custom-http fields (post url, form field, response path) only appear when the Custom destination is selected, instead of under every destination
- binding a task to a hotkey another task already uses no longer leaves the list showing a phantom binding the app never saved
- s3 can be picked as a per-task upload target
- a pinned screenshot window always appears and stays closable even if its image can't be loaded, and lowering its opacity no longer fades its own controls out of reach
- on a multi-HDR-monitor desktop, a capture on the secondary monitor is tonemapped against that monitor's SDR-content brightness rather than the primary's, and a transient panic in the HDR path no longer bricks HDR capture until restart
- a non-ASCII upload URL no longer crashes the tray menu rebuild
- the empty-history hint shows the screenshot/gif hotkeys you actually bound rather than stale defaults
- refreshed dependencies to clear outstanding security advisories

## [0.5.41] — 2026-07-11

### added
- a native installer: `capscr-x.x.x-setup.exe` replaces the bare MSI as the download — a single small window in capscr's own greyscale style (no wizard), wrapping the same signed MSI the in-app updater consumes, so the update chain is untouched. supports `/S` for silent installs and `/uninstall`; the MSI stays attached for scripted deployment

### fixed
- resolved recordings stopping long before the configured max duration on large or busy regions: captured frames no longer pile up in a fixed 1 GB memory buffer (about 8 seconds of moving 1080p content). GIF frames now spool to a temp file on disk and MP4 frames are encoded by ffmpeg live while the recording runs, so the limit in settings → capture is what actually ends a recording
- resolved the remaining playback speed drift in GIF and MP4 recordings: rounding each frame's delay separately accumulated error over the clip (15fps GIFs ~11% fast, 60fps GIFs 20% slow, MP4s up to a third too long when capture ran behind); frame timings are now scheduled against the cumulative wall clock so total drift stays under one frame regardless of length
- resolved system audio in MP4 recordings drifting out of sync after quiet stretches and sometimes cutting the video short: loopback capture delivers nothing while the system is silent, so those gaps are now zero-filled to keep the track wall-clock aligned
- console windows no longer flash while capscr runs ffmpeg (mp4 save, trim, availability check) or opens a capture with its default app

### changed
- stopping an MP4 recording now saves near-instantly: encoding happens during capture instead of all at once after stop
- a recording that stops by itself now says why: hitting the configured max duration, the frame-count safety limit, or low disk space each surface a toast instead of ending silently
- gif recordings no longer engage the system-audio loopback tap; it only runs for MP4, the format that can carry the track

## [0.5.40] — 2026-06-28

### fixed
- resolved monochrome and non-alpha cursors (such as the text select I-beam) being completely invisible in screenshots and video recordings: capscr now queries the cursor's monochrome AND mask directly to reconstruct the alpha channel, instead of using a color-based heuristic that incorrectly treated black pixels as transparent

## [0.5.39] — 2026-06-25

### added
- offline windows-native ocr: extract text from captured screenshots locally via WinRT
- floating desktop pinning: pin screenshots to the screen as borderless transparent sticky notes with customizable opacity controls
- jxl and avif output: added support for native jpeg xl and avif file formats
- s3 compatible cloud uploader: configure and test custom s3/compatible buckets for image storage with signature v4 and dpapi key encryption
- wasapi loopback audio recording: record system audio with screen recording and merge into mp4 outputs using ffmpeg

### changed
- screenshot overlay magnifier: adjust zoom factor with mouse wheel scroll
- select region handles: precision border adjustments using arrow keys (shift, ctrl, and ctrl+shift combinations)

## [0.5.38] — 2026-06-18

### added
- trim recordings in place: mp4 entries in history now have a trim action that opens the clip with in/out sliders (and set-to-playhead) and exports either a frame-accurate cut (precise, re-encoded) or an instant lossless cut (fast, stream-copied), so a quick trim no longer means opening a video editor

### changed
- capscr no longer keeps system-wide keyboard and mouse hooks running when they aren't needed: the keyboard hook is installed only while a keyboard shortcut is bound (or one is being recorded) and the mouse hook only while a mouse-button shortcut is bound, cutting capscr's background input overhead — by default no global mouse hook is installed at all
- history tile actions (copy, reveal, re-upload, delete) now show a brief confirmation on success instead of silently completing, so a working button no longer looks like it did nothing

## [0.5.37] — 2026-06-13

### fixed
- resolved capscr showing a generic icon in Windows Explorer and the taskbar: the embedded icon now carries BMP-format entries for the small sizes (16–128px) that Windows renders for executables, instead of PNG-only entries it could not decode at those sizes

## [0.5.36] — 2026-06-13

### added
- the mouse cursor is now drawn into GIF and MP4 recordings when "show cursor" is enabled, so the pointer is visible and follows the mouse through the clip; the toggle now applies to both screenshots and recordings

### fixed
- resolved the cursor being stamped into the corner of region and window screenshots: it is now captured at the instant the screen freezes and only appears if it was inside the selected area, instead of landing wherever the mouse came to rest after the drag

## [0.5.35] — 2026-06-11

### fixed
- resolved GIF and MP4 recordings playing back much faster than real time: frames are now timestamped at capture and encoded with their true durations (per-frame GIF delays; held frames in MP4), so duplicate-frame skipping and slow captures no longer compress the timeline
- resolved the flashing red recording border faintly bleeding into captured frames: the border stroke is now clipped to the outside of the recorded region
- resolved "captures folder unreachable" for output directories on secondary drives: any writable local folder is accepted now instead of only home/pictures/temp

## [0.5.34] — 2026-06-11

### fixed
- resolved the editor window opening as a blank white page with no way to close it: dynamically created webviews could land on about:blank instead of the app url; the editor now detects this and re-navigates until the page boots

### changed
- editing is disabled for GIF/MP4 recordings across the app: the history edit button is hidden for recordings, clicking a recording tile reveals the file in explorer instead, recording tasks no longer offer the "open in editor" post-action, and existing tasks configured that way reveal the saved file instead

## [0.5.33] — 2026-06-10

### added
- recording overlay control bar with a live elapsed/limit timer and a clickable stop button, placed outside the captured region so it never appears in the recording

### fixed
- resolved the image editor window opening as a blank white page that could not be closed
- copying a recorded GIF/MP4 now places the actual file on the clipboard (pasteable into explorer and chat apps) instead of the file path as text

## [0.5.32] — 2026-06-10

### performance
- optimized HDR PNG encoding to utilize uninitialized vectors and fast pointer byte copies, bypassing vector push and copy overhead
- optimized tonemapping buffer allocations to avoid zero-initializing large vectors, eliminating redundant memory writes

## [0.5.31] — 2026-06-10

### performance
- optimized regional recording loop to resolve target monitor once at startup and capture the screen directly, bypassing redundant monitor enumerations
- cached `is_hdr_at_point` queries for 2 seconds to reduce heavy DXGI and display configuration queries
- enabled high-precision 1ms timer resolution on Windows during active recording using `timeBeginPeriod` and `timeEndPeriod` to guarantee smooth frame pacing and eliminate stutter

## [0.5.30] — 2026-06-10

### added
- automatic download and configuration for FFmpeg when a user initiates a video recording (MP4) but does not have FFmpeg installed on their system

## [0.5.29] — 2026-06-10

### fixed
- resolved video recording (MP4) failing if ffmpeg is not present in the system PATH by automatically checking next to the application executable and in the app data directory

## [0.5.28] — 2026-06-09

### added
- first-launch onboarding overlay prompting users to configure their screenshot keybind shortcut if none is bound

### fixed
- resolved task name text input losing focus on every typed character in the tasks editor panel
- resolved missing application window icon on Windows taskbar for frameless windows

## [0.5.27] — 2026-06-09

### added
- regional video recording (MP4) support: allows recording high-performance video clips directly from a selected region and outputting H.264 MP4 format files
- screenshot and recording region selector overlay enhancements:
  - active selection cancelling: pressing the screenshot/recording hotkey again while the selector overlay is active/open immediately cancels it
  - aspect-ratio snap locking: holding shift snaps drag selection to standard aspect ratios (1:1, 16:9, 16:10, 4:3, 21:9)
  - recursive child window and control detection: holding ctrl traverses down from top-level windows to detect individual buttons, text boxes, and inner panels
  - forced even-pixel dimensions (width/height) on cropped regions to prevent H.264/FFmpeg encoding failures

## [0.5.26] — 2026-06-08

### fixed
- resolved clippy warnings in regional gif encoder

## [0.5.25] — 2026-06-05

### changed
- transitioned windows installer from custom dark-skinned NSIS (.exe) to standard native WiX (.msi) to resolve layout, line rendering, and icon transparency bugs

## [0.5.24] — 2026-06-05

### fixed
- low-level mouse hook now intercepts and consumes WM_XBUTTONUP/WM_NCXBUTTONUP release events for bound or newly captured mouse side buttons to block default browser/system navigation (e.g. moving back/forward a page in browsers)

## [0.5.23] — 2026-06-05

### changed
- plugins/marketplace view is code-split out of the hub's initial bundle (a 7.8 kB chunk loaded on demand when the tab opens, like the editor), trimming hub first-paint weight
- installed plugins are indexed by id so the registry list does O(1) lookups instead of a scan per entry, and the installed list no longer flashes "none installed" while the initial scan is still loading

## [0.5.22] — 2026-06-04

### fixed
- clippy under rust 1.91: give the capture serial-convert thread-local a `const` initializer, and keep the HDR tonemap's `min/max` verbatim behind `#[allow(clippy::manual_clamp)]` (clamp differs from min/max on NaN and the tonemap output is byte-exact). unblocks the CI lint job

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
