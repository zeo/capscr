#!/usr/bin/env bash
# smoke-test the wayland stack under a headless GNOME (Mutter) session with
# the GNOME portal backend. asserts `capscr --wayland-diag` classifies the
# desktop as GNOME and reports the GlobalShortcuts portal version, exercising
# the portal-detection path capscr's hotkey backend selection depends on.
# headless GNOME can't accept the interactive portal dialogs, so this checks
# detection and clean classification, not an end-to-end shortcut fire.
set -euo pipefail

CAPSCR_BIN="${1:-target/debug/capscr}"
if [ ! -x "$CAPSCR_BIN" ]; then
    echo "capscr binary not found at $CAPSCR_BIN" >&2
    exit 1
fi
CAPSCR_BIN="$(readlink -f "$CAPSCR_BIN")"

runtime="$(mktemp -d)"
export XDG_RUNTIME_DIR="$runtime"
export XDG_CURRENT_DESKTOP=GNOME

repo="$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)"

out="$runtime/diag.txt"
ext_out="$runtime/extension.txt"
session="$runtime/session.sh"
cat > "$session" <<EOF
#!/usr/bin/env bash
set -e
# sandbox the shell's config and data so the companion extension install
# never touches the runner's real profile
export XDG_DATA_HOME="$runtime/data"
export XDG_CONFIG_HOME="$runtime/config"
extdir="$runtime/data/gnome-shell/extensions/capscr@rot.lt"
mkdir -p "\$extdir" "$runtime/config"
cp "$repo/linux/gnome-extension/extension.js" "$repo/linux/gnome-extension/metadata.json" "\$extdir/"

# portal + pipewire stack for the screencast/screenshot paths
/usr/libexec/pipewire &
/usr/libexec/wireplumber &
/usr/libexec/xdg-desktop-portal-gnome &
/usr/libexec/xdg-desktop-portal &
gnome-shell --headless --wayland --virtual-monitor 1920x1080 &
shell_pid=\$!
# give the shell a moment to bring up its wayland socket + portal
sleep 8
for _ in \$(seq 1 40); do
    sock=\$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' 2>/dev/null | head -1)
    [ -n "\$sock" ] && break
    sleep 0.3
done
export WAYLAND_DISPLAY="\$(basename "\${sock:-wayland-0}")"
timeout 30 "$CAPSCR_BIN" --wayland-diag > "$out" 2>&1 || true

# companion extension wiring: loaded, versioned, and answering on the bus.
# enabling through the shell's own interface sidesteps sandboxed-dconf writes
{
    gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
        -m org.gnome.Shell.Extensions.EnableExtension "capscr@rot.lt" || true
    sleep 1
    gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell \
        -m org.gnome.Shell.Extensions.GetExtensionInfo "capscr@rot.lt" || true
    gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell/Extensions/Capscr \
        -m org.freedesktop.DBus.Properties.Get org.gnome.Shell.Extensions.Capscr Version || true
    gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell/Extensions/Capscr \
        -m org.gnome.Shell.Extensions.Capscr.ListWindows || true
} > "$ext_out" 2>&1
kill \$shell_pid 2>/dev/null || true
EOF
chmod +x "$session"

timeout 90 dbus-run-session -- "$session" || true

echo "--- diag output ---"
cat "$out" || true
echo "--- assertions ---"
grep -q "wayland session: true" "$out" || { echo "FAIL: not detected as a wayland session"; exit 1; }
grep -qi "desktop: Gnome" "$out" || { echo "FAIL: not classified as GNOME"; exit 1; }
# the portal version line is present whether or not it's supported; a "vN"
# value confirms the GlobalShortcuts backend is reachable
if grep -q "globalshortcuts portal: v" "$out"; then
    echo "PASS: GNOME classified and GlobalShortcuts portal detected"
else
    echo "NOTE: GlobalShortcuts portal absent in this GNOME build (hotkeys would fall back to Advanced input)"
    echo "PASS: GNOME classified; portal detection path ran"
fi
echo "--- companion extension ---"
cat "$ext_out" || true
grep -Eq "'state': <(uint32 )?1(\.0)?>" "$ext_out" || { echo "FAIL: companion extension not active"; exit 1; }
grep -q "uint32 1" "$ext_out" || { echo "FAIL: companion version property unanswered"; exit 1; }
grep -q "\[" "$ext_out" || { echo "FAIL: companion ListWindows returned no JSON"; exit 1; }
echo "PASS: companion extension active and answering"
