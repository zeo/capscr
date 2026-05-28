# capscr

Fast HDR-aware Windows screen capture — tray-first, signed updates, no telemetry.

- homepage: [rot.lt/work/capscr](https://rot.lt/work/capscr)
- plugins: [rot.lt/work/capscr/plugins](https://rot.lt/work/capscr/plugins) — publishing contract in [`docs/marketplace.md`](docs/marketplace.md), registry at [`lintowe/capscr-plugins`](https://github.com/lintowe/capscr-plugins)
- downloads: [GitHub Releases](https://github.com/lintowe/capscr/releases) (MSI + NSIS, signed updater)
- license: MIT

## features

HDR captures via Windows.Graphics.Capture FP16, ICtCp luminance-only tonemap (per-frame MaxCLL via P99), SDR PNG output. Per-monitor SDR-white detection.

Per-hotkey task model. Each hotkey binds a capture mode plus a post-action (save, clipboard, GIF, upload). No central default — every hotkey is its own task. Default bindings: `PrintScreen` for region → save + clipboard, `Ctrl+Shift+G` for region GIF → save.

Selection overlay: drag for region, click for window, Enter for fullscreen, `Alt+click` for color picker (pixel `#RRGGBB` copied to clipboard). Live `WxH @ X,Y` readout, 8× magnifier loupe, window-snap highlight.

Uploads: Imgur (anonymous), custom HTTPS POST, FTP. HTTP and FTP both go through SSRF protection (DNS double-resolve, private-IP / cloud-metadata rejection).

Tray-only at idle (~14 MB working set). The hub window allocates a webview only when opened.

Signed auto-updates via `tauri-plugin-updater` (ed25519, embedded pubkey).

No telemetry.

## install

Download from the [releases page](https://github.com/lintowe/capscr/releases/latest):

| file | use |
|---|---|
| `capscr_x.x.x_x64-setup.exe` | NSIS installer (recommended) |
| `capscr_x.x.x_x64_en-US.msi` | MSI installer |
| `*.sig` | updater signatures — keep alongside the installer if running the updater manually |
| `latest.json` | auto-updater manifest, not for manual install |

Windows 10 1903+ required. HDR capture goes through Windows.Graphics.Capture FP16, which is Windows-only — no macOS or Linux builds exist. The Cargo target hooks for those platforms are vestigial scaffolding from earlier prototyping.

## default hotkeys

Configurable in **hub → Tasks**.

| hotkey | action |
|---|---|
| `PrintScreen` | region capture → save + clipboard |
| `Ctrl+Shift+G` | region GIF → save to file |

Hold `Alt` while the selection overlay is up and click any pixel to copy its `#RRGGBB` to clipboard.

## configuration

Settings live at `%APPDATA%\capscr\config.toml` and are editable in **hub → Settings**. Notable fields:

```toml
[capture.hdr]
brightness_nits = 0.0        # SDR-white override in nits; 0 = auto-detect
user_brightness_scale = 1.0  # global pre-tonemap exposure multiplier
use_p99_max_cll = true       # ignore extreme outliers when picking source peak

[upload]
destination = "Imgur"        # or "Custom" / "Ftp"
copy_url_to_clipboard = true

[upload.ftp]
host = "files.example.com"
port = 21
username = "user"
password = "plaintext-for-now-see-roadmap"
remote_dir = "/screenshots"
public_url_template = "https://files.example.com/{filename}"
```

## build from source

Requirements: Rust 1.75+, Node 20+, MSVC build tools.

```powershell
git clone https://github.com/lintowe/capscr.git
cd capscr
npm --prefix frontend install
cargo install tauri-cli --version "^2" --locked
cargo tauri build
```

For signed bundles set `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` before `cargo tauri build`. Generate a keypair with `cargo tauri signer generate -w ./signing/key.priv` and paste the public key into `tauri.conf.json` → `plugins.updater.pubkey`.

## plugins

capscr ships with a built-in marketplace. Open the hub (tray → click capscr), switch to the **plugins** tab, and the in-app browser fetches [`rot.lt/capscr/registry.json`](https://rot.lt/capscr/registry.json). Click `[install]` and capscr downloads the plugin zip, verifies its sha256, and extracts it to `%APPDATA%/com.capscr.capscr/data/plugins/<id>/`.

The marketplace contract — `registry.json` shape, plugin zip layout, publishing — is documented in [`docs/marketplace.md`](docs/marketplace.md). The source-of-truth registry lives at [`lintowe/capscr-plugins`](https://github.com/lintowe/capscr-plugins).

Status: the plugin runtime (event hooks, WASM host) ships in v0.4. WASM plugins now execute — the host dispatches `on_capture`, `on_capture_saved`, and `on_upload_success` to plugin exports, and grants capability-gated host imports (`log`, `clipboard_write_text`, `notify`, `fetch`). See [`docs/plugin-runtime.md`](docs/plugin-runtime.md). Plugins without a `[runtime]` section stay metadata-only — listed under "installed" but not executed.

## roadmap

Work that did not make 0.3.1:

- in-app canvas editor (arrows, text, blur, step numbers, crop) — shipped 0.3.10+
- WASM plugin host with manifest-declared permissions + marketplace fed by github.com/lintowe/capscr-plugins — marketplace client shipped 0.3.29, runtime host shipped 0.4.0 (capability-gated clipboard/notify/fetch host imports)
- HDR-preserved output (JPEG-XL, AVIF with PQ, PNG+cICP) — PNG+cICP shipped 0.3.28 for HDR10 source; scRGB and HLG in v0.4. JXL/AVIF deferred
- SFTP destination (planned behind a `sftp` feature flag once the russh API stabilises)
- DPAPI / Windows credential vault for stored FTP passwords (currently plaintext in `config.toml`)

## credits

HDR tonemap in `src/capture/tonemapping.rs` is a Rust port of the SKIV (Special K Image Viewer) ICtCp luminance-only tonemap by Andon "Kaldaien" Coleman, MIT-licensed: https://github.com/SpecialKO/SKIV

Per-frame MaxCLL / P99 logic adapted from GotoFinal's open-source HDR tonemap reference, MIT-licensed.

## license

MIT — see [LICENSE](LICENSE).
