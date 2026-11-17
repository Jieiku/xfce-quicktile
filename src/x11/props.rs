//! EWMH atom interning and X11 window-property readers.

use anyhow::Result;
use x11rb::{
    connection::Connection,
    protocol::xproto::{Atom, AtomEnum, ConnectionExt as XprotoConnectionExt, Window},
    rust_connection::RustConnection,
};

use super::monitors::Strut;

/// All EWMH atoms the daemon uses, interned together in a single round
/// trip.
///
/// Consolidating the atoms in one struct lets the parent module and the
/// `windows` sibling pass a single `&Atoms` around instead of a grab-bag
/// of individual `Atom` values.
pub(super) struct Atoms {
    pub(super) moveresize: Atom,
    pub(super) frame_extents: Atom,
    pub(super) active_window: Atom,
    pub(super) wm_state: Atom,
    pub(super) net_wm_state: Atom,
    pub(super) net_wm_state_maximized_horz: Atom,
    pub(super) net_wm_state_maximized_vert: Atom,
    pub(super) net_wm_state_fullscreen: Atom,
    pub(super) client_list: Atom,
    pub(super) strut_partial: Atom,
    pub(super) strut: Atom,
}

impl Atoms {
    /// Intern all 11 atoms in ONE round trip.
    ///
    /// `RustConnection` buffers outgoing requests until a reply is waited
    /// on: creating all 11 `intern_atom` cookies first and only then
    /// calling `.reply()` means the first `reply()` flushes the buffer and
    /// the server answers all 11 atoms in one packet. The replies must be
    /// read in the SAME order the cookies were created, because x11rb
    /// pairs each reply with the oldest outstanding request — reordering
    /// would silently misassign atom ids.
    pub(super) fn new(conn: &RustConnection) -> Result<Self> {
        // All cookies first: every request is still buffered at this point,
        // so the first `reply()` below sends the whole batch in one
        // round trip.
        let moveresize = conn.intern_atom(false, b"_NET_MOVERESIZE_WINDOW")?;
        let frame_extents = conn.intern_atom(false, b"_NET_FRAME_EXTENTS")?;
        let active_window = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?;
        let wm_state = conn.intern_atom(false, b"WM_STATE")?;
        let net_wm_state = conn.intern_atom(false, b"_NET_WM_STATE")?;
        let net_wm_state_maximized_horz =
            conn.intern_atom(false, b"_NET_WM_STATE_MAXIMIZED_HORZ")?;
        let net_wm_state_maximized_vert =
            conn.intern_atom(false, b"_NET_WM_STATE_MAXIMIZED_VERT")?;
        let net_wm_state_fullscreen = conn.intern_atom(false, b"_NET_WM_STATE_FULLSCREEN")?;
        let client_list = conn.intern_atom(false, b"_NET_CLIENT_LIST")?;
        let strut_partial = conn.intern_atom(false, b"_NET_WM_STRUT_PARTIAL")?;
        let strut = conn.intern_atom(false, b"_NET_WM_STRUT")?;

        // Replies in creation order (see the doc above).
        let atoms = Self {
            moveresize: moveresize.reply()?.atom,
            frame_extents: frame_extents.reply()?.atom,
            active_window: active_window.reply()?.atom,
            wm_state: wm_state.reply()?.atom,
            net_wm_state: net_wm_state.reply()?.atom,
            net_wm_state_maximized_horz: net_wm_state_maximized_horz.reply()?.atom,
            net_wm_state_maximized_vert: net_wm_state_maximized_vert.reply()?.atom,
            net_wm_state_fullscreen: net_wm_state_fullscreen.reply()?.atom,
            client_list: client_list.reply()?.atom,
            strut_partial: strut_partial.reply()?.atom,
            strut: strut.reply()?.atom,
        };
        conn.flush()?;
        Ok(atoms)
    }
}

/// WM frame extents (`_NET_FRAME_EXTENTS`) in pixels.
///
/// An absent or malformed property yields all-zero extents, which callers
/// treat as "no WM frame".
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FrameExtents {
    pub(super) left: i32,
    pub(super) right: i32,
    pub(super) top: i32,
    pub(super) bottom: i32,
}

/// Read a property's 32-bit values, propagating connection and protocol
/// errors.
///
/// Returns an empty vec when the property is absent, has a non-32 format,
/// or holds no items — each caller decides whether that is a failure
/// (propagating readers) or a fallback (swallowing readers), so the
/// per-reader error behavior stays explicit at the call site.
fn property_u32s(
    conn: &RustConnection,
    window: Window,
    atom: Atom,
    typ: AtomEnum,
    long_length: u32,
) -> Result<Vec<u32>> {
    let reply = conn
        .get_property(false, window, atom, typ, 0, long_length)?
        .reply()?;
    Ok(reply.value32().map(|v| v.collect()).unwrap_or_default())
}

/// EWMH `_NET_ACTIVE_WINDOW`: the toplevel the WM considers active.
/// Propagates errors.
pub(super) fn active_window(
    conn: &RustConnection,
    root: Window,
    atom: Atom,
) -> Result<Option<Window>> {
    let values = property_u32s(conn, root, atom, AtomEnum::WINDOW, 1)?;
    Ok(values.first().copied().map(|v| v as Window))
}

/// Whether `window` carries a non-empty `WM_STATE` property, i.e. is
/// managed by a window manager. Swallows every error and returns false —
/// this is a probe, not a required read.
///
/// Checks the raw property bytes (`reply.value`), not the 32-bit view: a
/// non-32-format WM_STATE must still count as managed.
pub(super) fn has_wm_state(conn: &RustConnection, window: Window, atom: Atom) -> bool {
    let Ok(cookie) = conn.get_property(false, window, atom, AtomEnum::ANY, 0, 2) else {
        return false;
    };
    let Ok(reply) = cookie.reply() else {
        return false;
    };
    !reply.value.is_empty()
}

/// Whether `window` is maximized (horz or vert) or fullscreen per its
/// `_NET_WM_STATE`. Propagates errors.
pub(super) fn is_maximized_or_fullscreen(
    conn: &RustConnection,
    window: Window,
    atoms: &Atoms,
) -> Result<bool> {
    let values = property_u32s(conn, window, atoms.net_wm_state, AtomEnum::ATOM, 16)?;
    Ok(values.into_iter().any(|a| {
        a == atoms.net_wm_state_maximized_horz
            || a == atoms.net_wm_state_maximized_vert
            || a == atoms.net_wm_state_fullscreen
    }))
}

/// EWMH `_NET_CLIENT_LIST`: every managed client window. Propagates
/// errors.
pub(super) fn client_list(conn: &RustConnection, root: Window, atom: Atom) -> Result<Vec<Window>> {
    let values = property_u32s(conn, root, atom, AtomEnum::WINDOW, 4096)?;
    Ok(values.into_iter().map(|id| id as Window).collect())
}

/// Parse the strut properties of one client window into a `Strut`.
///
/// Prefers `_NET_WM_STRUT_PARTIAL` (12 CARDINALs) and falls back to
/// `_NET_WM_STRUT` (4 CARDINALs). Swallows every error and returns `None`
/// when the window advertises no (all-zero) strut.
pub(super) fn read_strut(conn: &RustConnection, window: Window, atoms: &Atoms) -> Option<Strut> {
    // Prefer PARTIAL (12 CARDINALs), fall back to STRUT (4 CARDINALs).
    // Layout of _NET_WM_STRUT_PARTIAL:
    //  0 left, 1 right, 2 top, 3 bottom,
    //  4 left_start_y, 5 left_end_y,
    // 6 right_start_y, 7 right_end_y,
    // 8 top_start_x, 9 top_end_x,
    //10 bottom_start_x, 11 bottom_end_x
    if let Ok(v) = property_u32s(conn, window, atoms.strut_partial, AtomEnum::CARDINAL, 12) {
        if v.len() >= 12 && (v[0] | v[1] | v[2] | v[3]) != 0 {
            return Some(Strut {
                left: v[0],
                right: v[1],
                top: v[2],
                bottom: v[3],
                left_y: Some((v[4] as i32)..(v[5] as i32 + 1)),
                right_y: Some((v[6] as i32)..(v[7] as i32 + 1)),
                top_x: Some((v[8] as i32)..(v[9] as i32 + 1)),
                bottom_x: Some((v[10] as i32)..(v[11] as i32 + 1)),
            });
        }
    }

    if let Ok(v) = property_u32s(conn, window, atoms.strut, AtomEnum::CARDINAL, 4) {
        if v.len() >= 4 && (v[0] | v[1] | v[2] | v[3]) != 0 {
            return Some(Strut {
                left: v[0],
                right: v[1],
                top: v[2],
                bottom: v[3],
                left_y: None,
                right_y: None,
                top_x: None,
                bottom_x: None,
            });
        }
    }
    None
}

/// `_NET_FRAME_EXTENTS` as (left, right, top, bottom) in pixels.
///
/// Swallows every error and returns all-zero extents when the property is
/// absent, has the wrong type, or is too short — the window is then
/// treated as unframed.
pub(super) fn frame_extents(conn: &RustConnection, window: Window, atom: Atom) -> FrameExtents {
    let Ok(values) = property_u32s(conn, window, atom, AtomEnum::CARDINAL, 4) else {
        return FrameExtents::default();
    };
    if values.len() >= 4 {
        FrameExtents {
            left: values[0] as i32,
            right: values[1] as i32,
            top: values[2] as i32,
            bottom: values[3] as i32,
        }
    } else {
        FrameExtents::default()
    }
}
