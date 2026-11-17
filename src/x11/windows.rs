//! Focused-window resolution and EWMH window-state manipulation.

use anyhow::{bail, Result};
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        ClientMessageData, ClientMessageEvent, ConnectionExt as XprotoConnectionExt, EventMask,
        Window,
    },
    rust_connection::RustConnection,
};

use super::props::{self, Atoms};

/// Prefer EWMH _NET_ACTIVE_WINDOW (the real toplevel the WM considers
/// active). Fall back to input-focus + climb to the nearest managed
/// toplevel. This is required for GTK/Mozilla/LibreOffice windows where
/// the keyboard focus is often on a child, not the client window the
/// window manager owns.
pub(super) fn resolve_target_window(
    conn: &RustConnection,
    root: Window,
    atoms: &Atoms,
) -> Result<Window> {
    // A failed active-window read falls back to input focus on purpose;
    // the `?` must not be added here.
    if let Ok(Some(win)) = props::active_window(conn, root, atoms.active_window) {
        if win != 0 && win != root {
            return Ok(win);
        }
    }

    let focus = conn.get_input_focus()?.reply()?.focus;
    if focus == 0 || focus == root {
        bail!("there is no focused application window");
    }
    find_toplevel(conn, root, focus, atoms)
}

/// Climb the parent chain until we reach a window that is a direct child
/// of the root, or that has a WM_STATE property (i.e. is managed).
///
/// Capped at 32 climbs; if the parent chain reaches 0 the function
/// returns 0, so the caller must re-check the result against 0 and the
/// root before acting.
pub(super) fn find_toplevel(
    conn: &RustConnection,
    root: Window,
    mut window: Window,
    atoms: &Atoms,
) -> Result<Window> {
    for _ in 0..32 {
        if window == 0 || window == root {
            break;
        }

        // Prefer a window that the WM is managing.
        if props::has_wm_state(conn, window, atoms.wm_state) {
            return Ok(window);
        }

        let tree = conn.query_tree(window)?.reply()?;
        if tree.parent == root || tree.parent == 0 {
            return Ok(window);
        }
        window = tree.parent;
    }
    Ok(window)
}

/// Ask the WM to leave maximized / fullscreen state. While a window is
/// maximized, the WM owns its geometry and ignores or clamps
/// `_NET_MOVERESIZE_WINDOW` requests, so the maximization must be
/// released before our geometry request is honored.
pub(super) fn unmaximize(
    conn: &RustConnection,
    root: Window,
    window: Window,
    atoms: &Atoms,
) -> Result<()> {
    // _NET_WM_STATE client message: action=0 (remove), then up to two atoms.
    // Source indication 2 = pager / external tool.
    let data = ClientMessageData::from([
        0u32, // _NET_WM_STATE_REMOVE
        atoms.net_wm_state_maximized_horz,
        atoms.net_wm_state_maximized_vert,
        2, // source: pager
        0,
    ]);
    let event = ClientMessageEvent::new(32, window, atoms.net_wm_state, data);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )?;

    let data_fs = ClientMessageData::from([0u32, atoms.net_wm_state_fullscreen, 0, 2, 0]);
    let event_fs = ClientMessageEvent::new(32, window, atoms.net_wm_state, data_fs);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event_fs,
    )?;
    conn.flush()?;
    // Brief settle delay so the WM applies the state change before the
    // next step issues the geometry request. 30 ms is frozen behavior,
    // not a tunable — do not "optimize" it.
    std::thread::sleep(std::time::Duration::from_millis(30));
    Ok(())
}
