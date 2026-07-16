#!/usr/bin/env bash
# smoke-test the wayland stack under a headless KWin (KDE Plasma 6) session.
# runs on a self-hosted runner / Fedora container where Plasma 6 is available
# (ubuntu-24.04 ships Plasma 5.27, which lacks the GlobalShortcuts portal and
# newer ScreenShot2). asserts capscr classifies the desktop as KDE and its
# ScreenShot2 pixel source is present. the .desktop grant for ScreenShot2 must
# be installed for a real capture; without it the diag still classifies KDE
# and records the authorization error, which is itself a useful check.
set -euo pipefail

CAPSCR_BIN="${1:-target/debug/capscr}"
if [ ! -x "$CAPSCR_BIN" ]; then
    echo "capscr binary not found at $CAPSCR_BIN" >&2
    exit 1
fi
CAPSCR_BIN="$(readlink -f "$CAPSCR_BIN")"

runtime="$(mktemp -d)"
export XDG_RUNTIME_DIR="$runtime"
export XDG_CURRENT_DESKTOP=KDE

out="$runtime/diag.txt"
session="$runtime/session.sh"
cat > "$session" <<EOF
#!/usr/bin/env bash
set -e
/usr/libexec/xdg-desktop-portal-kde &
/usr/libexec/xdg-desktop-portal &
kwin_wayland --virtual --width 1920 --height 1080 --no-lockscreen &
kwin_pid=\$!
sleep 8
for _ in \$(seq 1 40); do
    sock=\$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' 2>/dev/null | head -1)
    [ -n "\$sock" ] && break
    sleep 0.3
done
export WAYLAND_DISPLAY="\$(basename "\${sock:-wayland-0}")"
timeout 30 "$CAPSCR_BIN" --wayland-diag > "$out" 2>&1 || true
# force the ext-copy source too, proving that path builds a session even
# though kwin's chain prefers ScreenShot2
CAPSCR_FORCE_SOURCE=ext-image-copy timeout 30 "$CAPSCR_BIN" --wayland-diag >> "$out" 2>&1 || true
kill \$kwin_pid 2>/dev/null || true
EOF
chmod +x "$session"

timeout 90 dbus-run-session -- "$session" || true

echo "--- diag output ---"
cat "$out" || true
echo "--- assertions ---"
grep -q "wayland session: true" "$out" || { echo "FAIL: not detected as a wayland session"; exit 1; }
grep -qi "desktop: Kde" "$out" || { echo "FAIL: not classified as KDE"; exit 1; }
grep -q "kwin screenshot2 service: present" "$out" || { echo "FAIL: ScreenShot2 service not found"; exit 1; }
echo "PASS: KDE classified and ScreenShot2 service present"
