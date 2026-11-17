//! X11 connection, window geometry, monitor discovery, and move/resize operations.

mod monitors;
mod props;
mod windows;

use anyhow::{bail, Context, Result};
use x11rb::{
    connection::Connection,
    protocol::xproto::{
        ClientMessageData, ClientMessageEvent, ConnectionExt as XprotoConnectionExt, EventMask,
        Window,
    },
    rust_connection::RustConnection,
};

use crate::{
    state::{Column, Geometry, Row, StateStore},
    tiling::{advance_with_topology, target_geometry, Direction, Monitor},
};

pub struct X11Manager {
    conn: RustConnection,
    root: Window,
    atoms: props::Atoms,
    states: StateStore,
}

impl X11Manager {
    pub fn connect() -> Result<Self> {
        let (conn, screen_num) = x11rb::connect(None).context("connecting to X11 display")?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let atoms = props::Atoms::new(&conn)?;

        Ok(Self {
            conn,
            root,
            atoms,
            states: StateStore::default(),
        })
    }

    pub fn handle(&mut self, direction: Direction) -> Result<String> {
        let focus = windows::resolve_target_window(&self.conn, self.root, &self.atoms)?;
        // `find_toplevel` can return 0 if the parent chain hits 0, so the
        // resolved window is re-checked here before acting on it.
        if focus == 0 || focus == self.root {
            bail!("there is no focused application window");
        }

        let mut actual = self.window_outer_geometry(focus)?;
        let (physical_monitors, work_areas) = self.monitors()?;
        let existing = self.states.get_valid(focus, actual);

        // Only unmaximize when we have no valid tile state and the window is
        // actually maximized/fullscreen. Sending unmaximize on every keypress
        // causes visible flicker even for already-tiled windows.
        if existing.is_none() && props::is_maximized_or_fullscreen(&self.conn, focus, &self.atoms)?
        {
            windows::unmaximize(&self.conn, self.root, focus, &self.atoms)?;
            actual = self.window_outer_geometry(focus)?;
        }

        let Some(mut next) =
            advance_with_topology(direction, &work_areas, &physical_monitors, actual, existing)?
        else {
            return Ok("already at the desktop boundary".into());
        };

        let target = target_geometry(next, &work_areas);
        self.move_resize(focus, target)?;
        // Settle delay: let the WM apply the configure, then record the
        // geometry it actually produced. This keeps state valid across
        // monitors even when XFWM4 adjusts for panels, struts, or odd
        // sizes. 40 ms is frozen behavior, not a tunable — do not
        // "optimize" it.
        std::thread::sleep(std::time::Duration::from_millis(40));
        let applied = self.window_outer_geometry(focus).unwrap_or(target);
        next.previous = existing.map(|old| old.expected).or(Some(actual));
        next.expected = applied;
        self.states.insert(focus, next);

        let col = match next.column {
            Column::Full => "full-width",
            Column::Left => "left",
            Column::Right => "right",
        };
        let row = match next.row {
            Row::Full => "full-height",
            Row::Top => "top",
            Row::Bottom => "bottom",
        };
        Ok(format!("monitor {} / {} / {}", next.monitor + 1, col, row))
    }

    /// Fetch one physical RandR snapshot and derive a parallel work-area
    /// snapshot. Keep their ordering identical so tile-state monitor indices
    /// remain valid in both coordinate spaces.
    fn monitors(&self) -> Result<(Vec<Monitor>, Vec<Monitor>)> {
        let physical = monitors::fetch_monitors(&self.conn, self.root)?;
        let mut work_areas = physical.clone();

        // A failed client list means "no struts", not an error.
        let Ok(clients) = props::client_list(&self.conn, self.root, self.atoms.client_list) else {
            return Ok((physical, work_areas));
        };
        let struts: Vec<monitors::Strut> = clients
            .iter()
            .filter_map(|&w| props::read_strut(&self.conn, w, &self.atoms))
            .collect();
        monitors::apply_struts(&mut work_areas, &struts);
        Ok((physical, work_areas))
    }

    fn window_outer_geometry(&self, window: Window) -> Result<Geometry> {
        let geometry = self
            .conn
            .get_geometry(window)?
            .reply()
            .with_context(|| format!("reading geometry for window 0x{window:x}"))?;
        let translated = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)?
            .reply()?;
        let extents = props::frame_extents(&self.conn, window, self.atoms.frame_extents);
        Ok(Geometry {
            x: translated.dst_x as i32 - extents.left,
            y: translated.dst_y as i32 - extents.top,
            width: geometry.width as i32 + (extents.left + extents.right),
            height: geometry.height as i32 + (extents.top + extents.bottom),
        })
    }

    fn move_resize(&self, window: Window, outer: Geometry) -> Result<()> {
        let extents = props::frame_extents(&self.conn, window, self.atoms.frame_extents);
        // Client-space geometry in i32; only the wire format is u32.
        let client_x = outer.x + extents.left;
        let client_y = outer.y + extents.top;
        let client_width = outer
            .width
            .saturating_sub((extents.left + extents.right).max(0))
            .max(1);
        let client_height = outer
            .height
            .saturating_sub((extents.top + extents.bottom).max(0))
            .max(1);

        // EWMH _NET_MOVERESIZE_WINDOW flags for a pager-style move+resize
        // specifying all of x, y, width, height. Per EWMH 4.2/9.11 the
        // low byte is the gravity (10 = Static, so the WM places the
        // window including its decorations at exactly the given geometry),
        // bits 8-11 mark which of x/y/width/height are present (all four),
        // and bits 12-15 are the source indication (2 = pager / direct
        // user action, not an application). The exact expression is
        // frozen for behavior preservation — do not "clean it up". The
        // WM performs the actual configure.
        let flags = 10u32 | (1u32 << 8) | (1u32 << 9) | (1u32 << 10) | (1u32 << 11) | (2u32 << 12);
        let event = ClientMessageEvent::new(
            32,
            window,
            self.atoms.moveresize,
            ClientMessageData::from([
                flags,
                client_x as u32,
                client_y as u32,
                client_width as u32,
                client_height as u32,
            ]),
        );
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?;
        self.conn.flush()?;
        Ok(())
    }
}
