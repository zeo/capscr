# capscr

A screen capture tool for Windows and Linux that lives in the tray. It handles HDR monitors, records GIF and MP4, and updates itself from signed releases. It sends no telemetry.

![the capscr hub, history view](shot-history.png)

![the capscr hub, tasks view](shot-tasks.png)

- homepage: [rot.lt/work/capscr](https://rot.lt/work/capscr)
- plugins: [rot.lt/work/capscr/plugins](https://rot.lt/work/capscr/plugins), registry at [`zeo/capscr-plugins`](https://github.com/zeo/capscr-plugins)
- downloads: [GitHub Releases](https://github.com/zeo/capscr/releases)

## features

Each hotkey runs a task, which pairs a capture mode with what happens afterwards. The modes are region, last region, window, fullscreen, active monitor, region GIF and region MP4. Afterwards capscr can save, copy, open the editor, annotate, upload or copy the text out with OCR. There's no limit on the number of tasks.

In the selection overlay you drag out a region, click a window, or press Enter for the whole screen. `Alt+click` copies the pixel under the cursor as `#RRGGBB`. The overlay shows the selection size as you drag and an 8x magnifier, and highlights the window it will snap to.

HDR capture (Windows) goes through Windows.Graphics.Capture FP16 with an ICtCp luminance-only tonemap and per-monitor SDR-white detection. Output is SDR PNG, or HDR-preserved PNG (PQ cICP / HLG). Linux compositors don't expose HDR pixels to capture clients yet, so Linux captures are SDR.

Region recording saves GIF or H.264 MP4. On Windows the selection stays on screen as a frame you can move, resize or snap to a preset size, before and during recording, and a toolbar beside it has record, pause and stop. Neither shows up in the output. On Linux recording starts as soon as you finish the selection, and the toolbar has pause and stop. GIF output can be scaled, cut to fewer colors, dithered, or run through ffmpeg's palette filters. MP4 can use NVENC, Quick Sync or AMF when ffmpeg supports them. On Windows capscr downloads ffmpeg and checks its sha256; on Linux it uses the system copy.

Uploads go to Imgur, custom HTTPS POST, SFTP, or S3. Destinations pass SSRF checks (DNS double-resolve, private-IP rejection), and stored passwords live in the platform credential vault (DPAPI / Secret Service), never on disk in cleartext. Plain FTP is rejected because it leaks credentials in transit.

Updates install in place through the rot installer and are signed with Minisign. deb and rpm installs update through the system package manager instead.

## install

Pick a file from the [releases page](https://github.com/zeo/capscr/releases/latest):

| file | use |
|---|---|
| `capscr-x.x.x-online-setup.exe` | signed Windows bootstrapper, downloads the verified package |
| `capscr-x.x.x-offline-setup.exe` | signed Windows installer, package embedded |
| `capscr-x.x.x-linux-x86_64-setup` | Linux bootstrapper |
| `capscr-x.x.x-linux-x86_64-offline-setup` | Linux offline installer |
| `capscr_x.x.x_amd64.deb` | Debian / Ubuntu / Mint |
| `capscr-x.x.x-1.x86_64.rpm` | Fedora / openSUSE |
| `capscr_x.x.x_amd64.AppImage` | any distro with glibc 2.39+; `chmod +x` and run it |

Needs Windows 10 1903+, or a Linux desktop with webkit2gtk 4.1 and glibc 2.39+. MP4 recording uses `ffmpeg`, OCR uses `tesseract`, X11 file-clipboard uses `xclip`; the deb and rpm pull these in as recommends.

X11 and Wayland both work. Wayland picks a pixel source per compositor (KWin ScreenShot2, `ext-image-copy-capture` on wlroots, portals on GNOME):

| feature | X11 | KDE Wayland | wlroots Wayland | GNOME Wayland |
|---|---|---|---|---|
| capture, editor, upload, GIF + MP4 | yes | yes | yes | yes |
| window picking | capscr overlay | KWin picker | capscr overlay | portal picker |
| keyboard global hotkeys | yes | yes | portal, else Advanced input | yes |
| mouse side-button hotkeys | Advanced input | Advanced input | Advanced input | Advanced input |
| recording bar above fullscreen | yes | yes | yes | best-effort |
| tray icon | yes | yes | depends on host | needs AppIndicator |
| HDR-preserved capture | no | no | no | no |

Advanced input (Settings → hotkeys) reads `/dev/input` directly and needs the `input` group; it covers mouse buttons everywhere and keyboards on portalless compositors. HDR-preserved capture on Linux needs changes in the compositors themselves; the details are in [`docs/platform-limits.md`](docs/platform-limits.md).

## default hotkeys

| hotkey | action |
|---|---|
| set on first launch | region → save + clipboard |
| `Ctrl+Shift+G` | region GIF → save |
| `Ctrl+Shift+V` | region MP4 → save |

The Tasks tab rebinds all of them.

## configuration

`%APPDATA%\capscr\capscr\config\config.toml` on Windows, `~/.config/capscr/config.toml` on Linux. Every setting is in the hub, and the file is plain TOML if you'd rather edit it by hand:

```toml
[capture.hdr]
brightness_nits = 0.0        # SDR-white override; 0 = auto-detect
user_brightness_scale = 1.0  # pre-tonemap exposure multiplier

[upload]
destination = "Imgur"        # or "Custom" / "Sftp" / "S3"
copy_url_to_clipboard = true

[upload.sftp]
host = "files.example.com"
port = 22
username = "user"
password = "secret"          # moved into the credential vault on first save
remote_dir = "/screenshots"
public_url_template = "https://files.example.com/{filename}"
```

SFTP accepts Ed25519 and ECDSA keys through `private_key_path`. RSA keys stay disabled until RUSTSEC-2023-0071 is fixed upstream.

## build from source

Rust 1.97+, Node 22.12+, MSVC build tools on Windows or the webkit2gtk stack on Linux.

```sh
git clone https://github.com/zeo/capscr.git
cd capscr
npm --prefix frontend ci
cargo install tauri-cli --version "^2" --locked
cargo tauri build
```

Debian/Ubuntu build dependencies:

```sh
sudo apt install build-essential curl wget file pkg-config libssl-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libclang-dev libxcb1-dev libxcb-shm0-dev \
  libxcb-randr0-dev libxrandr-dev libdbus-1-dev libpipewire-0.3-dev \
  libwayland-dev libegl-dev libgbm-dev libdrm-dev
```

Release builds sign packages and manifests with `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

## plugins

The plugins tab browses [`rot.lt/capscr/registry.json`](https://rot.lt/capscr/registry.json), verifies each zip's sha256, and installs into the app data dir. WASM plugins get `on_capture`, `on_capture_saved`, and `on_upload_success` hooks with capability-gated host imports; see [`docs/plugin-runtime.md`](docs/plugin-runtime.md) and the publishing contract in [`docs/marketplace.md`](docs/marketplace.md).

## credits

The HDR tonemap in `src/capture/tonemapping.rs` is a Rust port of the SKIV ICtCp luminance-only tonemap by Andon "Kaldaien" Coleman (MIT): https://github.com/SpecialKO/SKIV. Per-frame MaxCLL / P99 logic adapted from GotoFinal's HDR tonemap reference (MIT).

## license

MIT, see [LICENSE](LICENSE).
