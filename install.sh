#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
AUTO_DIR="${HOME}/.config/autostart"
BINARY="xfce-quicktile"
DESKTOP_FILE="$AUTO_DIR/xfce-quicktile-daemon.desktop"

# ---------------------------------------------------------------
# --uninstall path
# ---------------------------------------------------------------
if [[ "${1:-}" == "--uninstall" ]]; then
    if pgrep -x "$BINARY" >/dev/null 2>&1; then
        pkill -x "$BINARY" || true
        echo "Stopped running daemon."
    fi
    rm -f "$BIN_DIR/$BINARY" "$DESKTOP_FILE"
    echo "Uninstalled $BINARY and removed autostart entry."
    exit 0
fi

# ---------------------------------------------------------------
# Dependency check
# ---------------------------------------------------------------
command -v cargo >/dev/null || {
    echo "Rust/Cargo is required. On EndeavourOS: sudo pacman -S rust" >&2
    exit 1
}

# ---------------------------------------------------------------
# Build
# ---------------------------------------------------------------
cd "$ROOT"
cargo build --release

# ---------------------------------------------------------------
# Install binary
# ---------------------------------------------------------------
install -d "$BIN_DIR" "$AUTO_DIR"
install -m 0755 "target/release/$BINARY" "$BIN_DIR/$BINARY"

# ---------------------------------------------------------------
# Autostart entry
#
# The absolute path is baked in at install time (expanded by the
# unquoted heredoc). No `sh -c` wrapper needed: the .desktop spec
# does not expand $HOME, but it accepts a literal absolute path,
# which is what we write here.
# ---------------------------------------------------------------
cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=XFCE QuickTile daemon
Comment=KDE-style directional tiling for XFCE on X11
Exec=$BIN_DIR/$BINARY daemon
Terminal=false
Hidden=false
NoDisplay=false
X-GNOME-Autostart-enabled=true
EOF

# ---------------------------------------------------------------
# Restart a running daemon so it picks up the new binary
# ---------------------------------------------------------------
if pgrep -x "$BINARY" >/dev/null 2>&1; then
    pkill -x "$BINARY" || true
    sleep 0.2
    "$BIN_DIR/$BINARY" daemon &
    disown
    echo "Restarted running daemon with the new binary."
fi

# ---------------------------------------------------------------
# Idempotent xfconf keybinding registration
#
# Registers Super+arrow -> helper. Skips already-matching
# bindings, so re-running the installer is a no-op for them.
# ---------------------------------------------------------------
set_binding() {
    local key="$1" cmd="$2" path="/commands/custom/${1}"
    local current
    current="$(xfconf-query -c xfce4-keyboard-shortcuts -p "$path" 2>/dev/null || true)"
    if [[ "$current" == "$cmd" ]]; then
        echo "  $key: already set"
    else
        xfconf-query -c xfce4-keyboard-shortcuts -p "$path" -n -t string -s "$cmd"
        echo "  $key: set"
    fi
}

echo
echo "Registering keyboard shortcuts:"
set_binding "<Super>Left"  "$BIN_DIR/$BINARY left"
set_binding "<Super>Right" "$BIN_DIR/$BINARY right"
set_binding "<Super>Up"    "$BIN_DIR/$BINARY up"
set_binding "<Super>Down"  "$BIN_DIR/$BINARY down"

# ---------------------------------------------------------------
# Summary
# ---------------------------------------------------------------
echo
echo "Installed to: $BIN_DIR/$BINARY"
echo "Autostart entry: $DESKTOP_FILE"
echo
echo "Start now with:"
echo "  $BIN_DIR/$BINARY daemon"
echo
echo "Keybindings (Super+arrow) are registered in xfce4-keyboard-shortcuts."
echo "If you change them manually, re-running this installer will overwrite"
echo "only the four bindings above (it leaves everything else alone)."
