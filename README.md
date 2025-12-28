# capscr

A fast, cross-platform screen capture tool with HDR support, GIF recording, and cloud upload.

## Features

- **Screen Capture** - Full screen, window, or region selection
- **HDR Support** - Capture HDR content with automatic tone mapping (ACES Filmic, Reinhard, Hable)
- **GIF Recording** - Record screen activity as animated GIFs with configurable FPS and quality
- **Cloud Upload** - Upload to Imgur or custom endpoints with HTTPS enforcement
- **Global Hotkeys** - Trigger captures from anywhere without switching windows
- **Clipboard Integration** - Copy screenshots directly to clipboard
- **Multi-Monitor** - Support for multiple displays including mixed DPI setups

## Installation

### Windows

Download the latest release from [Releases](https://github.com/lintowe/capscr/releases):

| File | Description |
|------|-------------|
| `capscr-x.x.x-x86_64.msi` | Windows installer (recommended) |
| `capscr-x.x.x-x86_64.zip` | Portable version, no installation required |

**Requirements:** Windows 10 version 1903 or later (for HDR capture support)

### Linux

```bash
# Download and extract
tar -xzf capscr-x.x.x-x86_64-linux.tar.gz
cd capscr

# Run
./capscr
```

**Requirements:** X11 display server, libxcb

### Building from Source

See [Build](#build) section below.

## Build

### Prerequisites

- [Rust](https://rustup.rs/) 1.75 or later
- Platform-specific dependencies:
  - **Windows:** Visual Studio Build Tools with C++ workload
  - **Linux:** `libxcb-dev`, `libxkbcommon-dev`, `libssl-dev`
  - **macOS:** Xcode Command Line Tools

### Quick Build

```bash
git clone https://github.com/lintowe/capscr.git
cd capscr
cargo build --release
```

The binary will be at `target/release/capscr` (or `capscr.exe` on Windows).

## Hotkeys

Default keyboard shortcuts (configurable in Settings):

| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+S` | Capture entire screen |
| `Ctrl+Shift+W` | Capture active window |
| `Ctrl+Shift+R` | Capture selected region |
| `Ctrl+Shift+G` | Start/stop GIF recording |

Hotkeys can be customized in the Settings panel. Supported modifiers: `Ctrl`, `Alt`, `Shift`, `Win`/`Super`/`Cmd`

## Configuration

Settings are automatically saved to:

| Platform | Location |
|----------|----------|
| Windows | `%APPDATA%\capscr\config.toml` |
| Linux | `~/.config/capscr/config.toml` |

### Configuration Options

```toml
[output]
directory = "~/Pictures/Screenshots"
format = "png"              # png, jpg, webp, bmp
quality = 95                # 1-100, for lossy formats

[capture]
show_cursor = true
delay_ms = 0                # Capture delay
gif_fps = 15                # GIF frame rate (1-60)
gif_max_duration_secs = 60  # Max GIF recording time
hdr_enabled = true
hdr_tonemap = "AcesFilmic"  # Reinhard, ReinhardExtended, AcesFilmic, Hable, Exposure
hdr_exposure = 1.0          # 0.1-10.0

[hotkeys]
capture_screen = "Ctrl+Shift+S"
capture_window = "Ctrl+Shift+W"
capture_region = "Ctrl+Shift+R"
record_gif = "Ctrl+Shift+G"

[upload]
destination = "Imgur"       # Imgur or Custom
copy_url_to_clipboard = true
# For custom endpoints:
# custom_url = "https://your-server.com/upload"
# custom_form_name = "file"
# custom_response_path = "url"
```

## License

MIT License - see [LICENSE](LICENSE) for details.
