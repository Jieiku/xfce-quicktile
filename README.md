# XFCE QuickTile

A lightweight Rust daemon for KDE-style directional quick tiling on X11. Uses XRandR for monitor geometry and EWMH to request window positioning from the active window manager.

The daemon is not XFCE-dependent, but the included installer configures XFCE autostart and keyboard shortcuts.

![quicktile](quicktile.gif)

## Behavior

Each monitor is divided into left/right and top/bottom halves. Directional shortcuts cycle through three states:

- **Left:** Right → Full → Left
- **Right:** Left → Full → Right
- **Up:** Bottom → Full → Top
- **Down:** Top → Full → Bottom

At monitor boundaries, navigation continues onto the neighboring monitor's opposite half. The perpendicular tile position is preserved. Navigation uses monitor geometry rather than enumeration order, supporting mixed resolutions, negative coordinates, gaps, and staggered layouts.

Tiling state is kept in memory and cleared when the daemon exits.

## Build and install (EndeavourOS)

```bash
sudo pacman -S --needed rust
chmod +x install.sh
./install.sh
```

The installer builds the release binary in `~/.local/bin`, configures XFCE autostart, and registers Super+Arrow shortcuts.

Start manually:

```bash
~/.local/bin/xfce-quicktile daemon
```

Check daemon status:

```bash
~/.local/bin/xfce-quicktile ping
```

## Other X11 desktop environments

The daemon uses standard X11 interfaces and can be used with other EWMH-compatible window managers. Configure autostart and keyboard shortcuts through your desktop environment's settings. The included installer is XFCE-specific.

## Limitations

- X11 only; Wayland is not supported.
- Dialogs, transient windows, and windows with fixed size hints may not resize correctly.
- Window-manager behavior and window decorations can affect positioning.
- The `stop` command does not currently stop the daemon; terminate the process manually or log out.

## Troubleshooting

Run the daemon in a terminal to see startup errors:

```bash
pkill -f 'xfce-quicktile daemon' || true
~/.local/bin/xfce-quicktile daemon
```

Verify your session and monitor layout:

```bash
echo "$XDG_SESSION_TYPE"
xrandr --listmonitors
```
