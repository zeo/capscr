# capscr

Fast HDR-aware screen capture for Windows and Linux — tray-first, signed updates, no telemetry.

- homepage: [rot.lt/work/capscr](https://rot.lt/work/capscr)
- plugins: [rot.lt/work/capscr/plugins](https://rot.lt/work/capscr/plugins) — publishing contract in [`docs/marketplace.md`](docs/marketplace.md), registry at [`zeo/capscr-plugins`](https://github.com/zeo/capscr-plugins)
- downloads: [GitHub Releases](https://github.com/zeo/capscr/releases) (signed MSI / deb / rpm / AppImage + auto-updater)
- license: MIT

## features

HDR captures via Windows.Graphics.Capture FP16, ICtCp luminance-only tonemap (per-frame MaxCLL via P99), SDR PNG output. Per-monitor SDR-white detection. (HDR capture is Windows-only; Linux desktops don't expose an HDR capture surface yet, so captures there are SDR.)

Per-hotkey task model. Each hotkey binds a capture mode (region, region-last, window, fullscreen, active monitor, region GIF, region MP4) plus a post-action (save, clipboard, open in editor, upload). No central default — every hotkey is its own task. Default tasks: region → save + clipboard (unbound out of the box; a first-launch prompt asks you to pick a key), `Ctrl+Shift+G` for region GIF → save, `Ctrl+Shift+V` for region MP4 → save.

Selection overlay: drag for region, click for window, Enter for fullscreen, `Alt+click` for color picker (pixel `#RRGGBB` copied to clipboard). Live `WxH @ X,Y` readout, 8× magnifier loupe, window-snap highlight.

Recording: region GIF and H.264 MP4 (MP4 via ffmpeg, auto-downloaded and sha256-verified on first use; on Linux a distro ffmpeg on PATH is preferred) with a live timer + stop control drawn outside the captured area and frames timed to real wall-clock playback. The mouse cursor is composited into recordings and screenshots when **show cursor** is enabled.

In-app editor: arrows, text, blur, step numbers, and crop, reached via the "open in editor" post-action.

Uploads: Imgur (anonymous), custom HTTPS POST, and SFTP. Network destinations go through SSRF protection (DNS double-resolve, private-IP / cloud-metadata rejection); stored SFTP passwords are kept in the per-user credential vault (DPAPI on Windows, the freedesktop Secret Service on Linux), not cleartext. Plain FTP is disabled because it exposes credentials and captures in transit.

Tray-only at idle (~14 MB working set). The hub window allocates a webview only when opened.

Signed auto-updates via `tauri-plugin-updater` (ed25519, embedded pubkey).

No telemetry.

## install

Download from the [releases page](https://github.com/zeo/capscr/releases/latest):

| file | use |
|---|---|
| `capscr-x.x.x-setup.exe` | **the installer** — one small window in capscr's own style, wrapping the signed MSI below (`/S` for silent installs) |
| `capscr_x.x.x_x64_en-US.msi` | the raw MSI, for Group Policy / scripted deployment |
| `capscr_x.x.x_x64_en-US.msi.sig` | updater signature — keep alongside the MSI if running the updater manually |
| `capscr_x.x.x_amd64.deb` | Debian / Ubuntu / Mint package |
| `capscr-x.x.x-1.x86_64.rpm` | Fedora / openSUSE package |
| `capscr_x.x.x_amd64.AppImage` | any distro — `chmod +x` and run; this is the build the Linux auto-updater tracks |
| `latest.json` | auto-updater manifest, not for manual install |

Windows 10 1903+ or a Linux desktop with webkit2gtk 4.1 and glibc 2.39+ (Ubuntu 24.04+, Debian 13+, Fedora 40+, or equivalents). Recording MP4 wants `ffmpeg`, the OCR post-action wants `tesseract`, and file-clipboard on X11 wants `xclip` — the deb/rpm packages pull these in as recommends.

Both X11 and Wayland are supported. On Wayland the pixel source is chosen per compositor: KDE uses KWin's authorized ScreenShot2, wlroots compositors (sway, Hyprland, COSMIC, …) use the `ext-image-copy-capture` protocol, and GNOME uses the screenshot / screencast portals. Feature coverage by session:

| feature | X11 | KDE Wayland | wlroots Wayland | GNOME Wayland |
|---|---|---|---|---|
| region / fullscreen / monitor capture, editor, upload, GIF + MP4 recording | ✅ | ✅ | ✅ | ✅ |
| window picking | capscr overlay | KWin picker | capscr overlay | GNOME's portal picker |
| keyboard global hotkeys | ✅ (X grabs) | ✅ (GlobalShortcuts portal) | portal if present, else Advanced input | ✅ (GlobalShortcuts portal) |
| mouse side-button hotkeys | Advanced input | Advanced input | Advanced input | Advanced input |
| recording bar / pin kept above fullscreen | ✅ | ✅ | ✅ (layer-shell) | best-effort |
| tray icon | ✅ | ✅ | depends on host | needs the AppIndicator extension |
| HDR-preserved capture | — | — | — | — |

"Advanced input" is an opt-in in **hub → Settings → hotkeys** that reads `/dev/input` directly (needs membership in the `input` group); it powers mouse side-button hotkeys and keyboard hotkeys on Wayland compositors without the GlobalShortcuts portal. HDR-preserved capture is Windows-only: no Linux compositor exposes HDR pixels to a capture client yet (run `capscr --wayland-diag` for a per-output readout), so Linux captures are SDR. On a desktop with no system tray (vanilla GNOME), capscr opens its hub with guidance and stays reachable through global hotkeys, the desktop-file capture actions, and relaunching the app.

The `—` cells above are platform boundaries, not missing features: they need changes in the compositor itself, not in capscr. Each one, why it exists, and what would close it is written up in [`docs/platform-limits.md`](docs/platform-limits.md).

## default hotkeys

Configurable in **hub → Tasks**.

| hotkey | action |
|---|---|
| _(unbound — set on first launch or in **hub → Tasks**)_ | region capture → save + clipboard |
| `Ctrl+Shift+G` | region GIF → save to file |
| `Ctrl+Shift+V` | region MP4 (H.264) → save to file |

Hold `Alt` while the selection overlay is up and click any pixel to copy its `#RRGGBB` to clipboard.

## configuration

Settings live at `%APPDATA%\com.capscr.capscr\config\config.toml` on Windows and `~/.config/capscr/config.toml` on Linux, editable in **hub → Settings**. Notable fields:

```toml
[capture.hdr]
brightness_nits = 0.0        # SDR-white override in nits; 0 = auto-detect
user_brightness_scale = 1.0  # global pre-tonemap exposure multiplier
use_p99_max_cll = true       # ignore extreme outliers when picking source peak

[upload]
destination = "Imgur"        # or "Custom" / "Ftp" / "Sftp"
copy_url_to_clipboard = true

[upload.ftp]
host = "files.example.com"
port = 21
username = "user"
password = "secret"           # migrated to the per-user credential vault on first save
remote_dir = "/screenshots"
public_url_template = "https://files.example.com/{filename}"

[upload.sftp]
host = "files.example.com"
port = 22
username = "user"
password = "secret"           # or set private_key_path; migrated to the per-user credential vault on first save
remote_dir = "/screenshots"
public_url_template = "https://files.example.com/{filename}"
```

SFTP accepts Ed25519 and ECDSA keys through `private_key_path`. RSA key support is disabled because its Rust implementation has an unresolved timing side-channel (RUSTSEC-2023-0071).

### where capscr stores things

Two directory trees, by design, on both platforms:

- **config, plugins, history, sound cache, downloaded ffmpeg, thumbnails** — under the `com.capscr.capscr` app dirs (`%APPDATA%\com.capscr.capscr` and `%LOCALAPPDATA%\com.capscr.capscr` on Windows; `~/.config/capscr`, `~/.local/share/capscr`, `~/.cache/capscr` on Linux).
- **window-state, updater bookkeeping, notification / taskbar identity** — under the Tauri app identifier `io.rot.capscr`.

The split is intentional: the identifiers are load-bearing (the updater's continuity, KDE's ScreenShot2 desktop-file grant, and the Windows AppUserModelID all key off `io.rot.capscr`), so they are not unified.

## build from source

Requirements: Rust 1.75+, Node 20+, and MSVC build tools (Windows) or the webkit2gtk stack (Linux).

```powershell
git clone https://github.com/zeo/capscr.git
cd capscr
npm --prefix frontend install
cargo install tauri-cli --version "^2" --locked
cargo tauri build
```

On Debian/Ubuntu the Linux build needs these packages first:

```sh
sudo apt install build-essential curl wget file pkg-config libssl-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libclang-dev libxcb1-dev libxcb-shm0-dev \
  libxcb-randr0-dev libxrandr-dev libdbus-1-dev libpipewire-0.3-dev \
  libwayland-dev libegl-dev libgbm-dev libdrm-dev
```

For signed bundles set `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` before `cargo tauri build`. Generate a keypair with `cargo tauri signer generate -w ./signing/key.priv` and paste the public key into `tauri.conf.json` → `plugins.updater.pubkey`.

## plugins

capscr ships with a built-in marketplace. Open the hub (tray → click capscr), switch to the **plugins** tab, and the in-app browser fetches [`rot.lt/capscr/registry.json`](https://rot.lt/capscr/registry.json). Click `[install]` and capscr downloads the plugin zip, verifies its sha256, and extracts it to `%APPDATA%/com.capscr.capscr/data/plugins/<id>/`.

The marketplace contract — `registry.json` shape, plugin zip layout, publishing — is documented in [`docs/marketplace.md`](docs/marketplace.md). The source-of-truth registry lives at [`zeo/capscr-plugins`](https://github.com/zeo/capscr-plugins).

Status: the plugin runtime (event hooks, WASM host) ships in v0.4. WASM plugins now execute — the host dispatches `on_capture`, `on_capture_saved`, and `on_upload_success` to plugin exports, and grants capability-gated host imports (`log`, `clipboard_write_text`, `notify`, `fetch`). See [`docs/plugin-runtime.md`](docs/plugin-runtime.md). Plugins without a `[runtime]` section stay metadata-only — listed under "installed" but not executed.

## roadmap

Most of the original roadmap has shipped: the in-app editor, the WASM plugin host + marketplace, HDR-preserved PNG (PQ cICP and HLG), the SFTP destination, and DPAPI-encrypted upload credentials.

Still deferred:

- HDR-preserved output in more formats — scRGB, plus JPEG-XL and AVIF with PQ (PNG+cICP and HLG already ship)

## credits

HDR tonemap in `src/capture/tonemapping.rs` is a Rust port of the SKIV (Special K Image Viewer) ICtCp luminance-only tonemap by Andon "Kaldaien" Coleman, MIT-licensed: https://github.com/SpecialKO/SKIV

Per-frame MaxCLL / P99 logic adapted from GotoFinal's open-source HDR tonemap reference, MIT-licensed.

## license

MIT — see [LICENSE](LICENSE).
