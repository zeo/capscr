#!/usr/bin/env bash
# builds a release binary with bundled frontend assets and installs it for the
# current user. plain `cargo build --release` produces a cfg(dev) binary whose
# webviews point at the vite dev url — the custom-protocol feature below is what
# makes tauri embed frontend/dist instead, so don't drop it.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
bin_dir="${HOME}/.local/bin"
app_dir="${HOME}/.local/share/applications"
icon_dir="${HOME}/.local/share/icons/hicolor/512x512/apps"

cd "$repo"

[ -d frontend/node_modules ] || npm --prefix frontend ci
npm --prefix frontend run build

cargo build --locked --release --features custom-protocol

mkdir -p "$bin_dir" "$app_dir" "$icon_dir"

# the app may be running as the transient unit from a previous install run or
# as the login autostart unit; stop both so the new binary owns the session
for unit in capscr.service app-capscr@autostart.service; do
    if systemctl --user is-active --quiet "$unit"; then
        systemctl --user stop "$unit"
    fi
done

install -m 755 target/release/capscr "$bin_dir/capscr"
install -m 644 icons/icon.png "$icon_dir/capscr.png"

# the desktop file name and the ScreenShot2 grant are what authorize the app
# against kwin's screenshot interface — keep both stable
sed -e "s|{{exec}}|$bin_dir/capscr|g" \
    -e "s|{{name}}|capscr|" \
    -e "s|{{icon}}|capscr|" \
    -e "s|{{comment}}|Screen capture|" \
    -e "s|{{categories}}|Graphics;Utility;|" \
    linux/capscr.desktop > "$app_dir/capscr.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database "$app_dir" || true

systemd-run --user --collect --unit=capscr "$bin_dir/capscr"
sleep 1
systemctl --user status capscr.service --no-pager -n 5 || true
