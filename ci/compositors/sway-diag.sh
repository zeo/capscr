#!/usr/bin/env bash
# smoke-test the wayland capture stack under a headless sway (wlroots) session.
# launches sway with the pixman software renderer and no input devices, then
# runs `capscr --wayland-diag` and asserts it detects a wlroots wayland
# session and drives its source chain without crashing. capture may return
# black under headless pixman, so the bar is "the diagnostic runs and
# classifies the desktop", not "a frame came back lit".
set -euo pipefail

CAPSCR_BIN="${1:-target/debug/capscr}"
if [ ! -x "$CAPSCR_BIN" ]; then
    echo "capscr binary not found at $CAPSCR_BIN" >&2
    exit 1
fi
CAPSCR_BIN="$(readlink -f "$CAPSCR_BIN")"

runtime="$(mktemp -d)"
export XDG_RUNTIME_DIR="$runtime"
export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_LIBINPUT_NO_DEVICES=1
export XDG_CURRENT_DESKTOP=sway

config="$runtime/sway.cfg"
cat > "$config" <<'EOF'
exec_always true
EOF

out="$runtime/diag.txt"
# run the diagnostic inside the sway session; sway exits when the command does
timeout 90 dbus-run-session -- sway --config "$config" -d 2> "$runtime/sway.log" &
sway_pid=$!

# wait for the compositor socket to appear
for _ in $(seq 1 50); do
    sock=$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' 2>/dev/null | head -1)
    [ -n "$sock" ] && break
    sleep 0.2
done
if [ -z "${sock:-}" ]; then
    echo "sway never created a wayland socket" >&2
    cat "$runtime/sway.log" >&2 || true
    kill "$sway_pid" 2>/dev/null || true
    exit 1
fi
export WAYLAND_DISPLAY="$(basename "$sock")"

timeout 30 "$CAPSCR_BIN" --wayland-diag | tee "$out" || true
swaymsg exit 2>/dev/null || kill "$sway_pid" 2>/dev/null || true

echo "--- assertions ---"
grep -q "wayland session: true" "$out" || { echo "FAIL: not detected as a wayland session"; exit 1; }
grep -qi "desktop: Wlroots" "$out" || { echo "FAIL: sway not classified as wlroots"; exit 1; }
# the chain must at least reach the portal fallback row for every output
grep -q "portal-screenshot on" "$out" || { echo "FAIL: source chain did not run"; exit 1; }
echo "PASS: sway diagnostic ran and classified the session"
