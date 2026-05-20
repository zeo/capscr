# capscr

Fast HDR-aware Windows screen capture, designed to replace ShareX without the bloat.

- **Homepage**: [rot.lt/work/capscr](https://rot.lt/work/capscr)
- **Plugins**: [rot.lt/work/capscr/plugins](https://rot.lt/work/capscr/plugins) · publishing contract: [`docs/marketplace.md`](docs/marketplace.md) · source-of-truth registry: [`lintowe/capscr-plugins`](https://github.com/lintowe/capscr-plugins)
- **Downloads**: [GitHub Releases](https://github.com/lintowe/capscr/releases) (MSI + NSIS, signed updater)
- **License**: MIT

## Features

- **HDR captures that look right.** Windows.Graphics.Capture FP16 → SKIV ICtCp luminance-only tonemap (per-frame MaxCLL via P99, chroma preservation) → SDR PNG. Per-monitor SDR-white detection.
- **Per-hotkey task model.** Bind any hotkey to a capture mode + post-action: `PrintScreen` → region → save + clipboard, `Ctrl+Shift+G` → region GIF → save, etc. No central "default action" — each hotkey is its own task.
- **Selection overlay.** Drag for region, click for window, Enter for fullscreen, **`Alt+click` for color picker** (pixel `#RRGGBB` → clipboard). Live `WxH @ X,Y` dimensions, 8× magnifier loupe, window-snap highlight.
- **Upload destinations.** Imgur (anonymous), custom HTTPS POST, FTP. HTTP and FTP both go through SSRF protection (DNS double-resolve, private-IP / cloud-metadata rejection).
- **Tray-first.** ~14 MB working set when idle. The hub window only allocates a webview when you open it.
- **Signed auto-updates.** ed25519-signed update bundles via `tauri-plugin-updater`, embedded pubkey, no separate channel.
- **No telemetry, no phone-home.**

## Install

Download from the [releases page](https://github.com/lintowe/capscr/releases/latest):

| File | Use |
|---|---|
| `capscr_x.x.x_x64-setup.exe` | NSIS installer (recommended) |
| `capscr_x.x.x_x64_en-US.msi` | MSI installer |
| `*.sig` | Updater signatures — leave alongside the installer if you run the auto-updater manually |
| `latest.json` | Auto-updater manifest, not for manual install |

Windows 10 1903+ for HDR capture; older builds and Linux X11 can still take SDR shots from source.

## Default hotkeys

Configurable in **hub → Tasks**.

| Hotkey | Action |
|---|---|
| `PrintScreen` | Region capture → save + clipboard |
| `Ctrl+Shift+G` | Region GIF → save to file |

Hold `Alt` while the selection overlay is up and click any pixel to copy its `#RRGGBB` to clipboard.

## Configuration

Settings live at `%APPDATA%\capscr\config.toml` and are also editable in **hub → Settings**. Notable fields:

```toml
[capture.hdr]
mode = "map-cll-to-display"  # or "normalize-to-cll" for HDR display output
brightness_nits = 80.0       # SDR-white target in nits
user_brightness_scale = 1.0
use_p99_max_cll = true

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

## Build from source

Requirements:

- Rust 1.75+
- Node 20+
- MSVC build tools (Windows)

```powershell
git clone https://github.com/lintowe/capscr.git
cd capscr
npm --prefix frontend install
cargo install tauri-cli --version "^2" --locked
cargo tauri build
```

For signed bundles set `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` before `cargo tauri build`. Generate a keypair with `cargo tauri signer generate -w ./signing/key.priv` and paste the public key into `tauri.conf.json` → `plugins.updater.pubkey`.

## Plugins

capscr ships with a built-in marketplace. Open the hub (tray → click capscr), switch to the **plugins** tab, and the in-app browser fetches [`rot.lt/capscr/registry.json`](https://rot.lt/capscr/registry.json) to show available plugins. Click `[install]` and capscr downloads the plugin zip, verifies its sha256, and extracts it to `%APPDATA%/com.capscr.capscr/data/plugins/<id>/`.

The marketplace contract — what `registry.json` must look like, what goes in a plugin zip, how publishing works — is documented in [`docs/marketplace.md`](docs/marketplace.md). The source-of-truth registry lives at [`lintowe/capscr-plugins`](https://github.com/lintowe/capscr-plugins).

**Status:** the plugin runtime (event hooks, WASM host) arrives in v0.4. Today's plugins install as metadata-only; they appear under "installed" but don't yet execute any logic.

## Roadmap

Work that did not make 0.3.1:

- In-app canvas editor (arrows, text, blur, step numbers, crop) — _shipped 0.3.10+_.
- WASM plugin host with manifest-declared permissions + marketplace fed by github.com/lintowe/capscr-plugins — _marketplace client shipped 0.3.29; runtime host in v0.4_.
- HDR-preserved output (JPEG-XL, AVIF with PQ, PNG+cICP) — _PNG+cICP shipped 0.3.28 for HDR10 source; scRGB and HLG in v0.4. JXL/AVIF deferred._
- SFTP destination (planned behind a `sftp` feature flag once the russh API stabilises).
- DPAPI / Windows credential vault for stored FTP passwords (currently plaintext in `config.toml`).

## Credits

- HDR tonemap in `src/capture/tonemapping.rs` is a Rust port of the SKIV (Special K Image Viewer) ICtCp luminance-only tonemap by Andon "Kaldaien" Coleman, MIT-licensed: https://github.com/SpecialKO/SKIV
- Per-frame MaxCLL / P99 logic follows the pattern from ShareX-HDR by GotoFinal (MIT): https://github.com/GotoFinal/ShareX-HDR

## License

MIT — see [LICENSE](LICENSE).
