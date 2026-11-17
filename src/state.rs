//! Window tile state and geometry types shared by the tiling and X11 layers.

use std::collections::HashMap;
use x11rb::protocol::xproto::Window;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Geometry {
    /// Center point of the outer rectangle (integer pixel math, floor
    /// division). Shared by monitor hit-testing and state inference so the
    /// "center" definition cannot drift between callers.
    pub fn center(self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// Full width of the monitor.
    Full,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// Full height of the monitor.
    Full,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileState {
    /// Index into the current monitor snapshot. Re-resolved from live window
    /// geometry before every transition; never treated as a persistent ID.
    pub monitor: usize,
    pub column: Column,
    pub row: Row,
    /// Last requested outer-frame geometry.
    pub expected: Geometry,
    /// Previous expected geometry, accepted while the WM is still applying
    /// the previous configure request.
    pub previous: Option<Geometry>,
}

#[derive(Default)]
pub struct StateStore {
    windows: HashMap<Window, TileState>,
}

impl StateStore {
    /// Record or replace the tile state for `window`.
    pub fn insert(&mut self, window: Window, state: TileState) {
        self.windows.insert(window, state);
    }

    /// Returns the saved state for `window` if `actual` is still "valid":
    /// within tolerance of `expected`, or of `previous` while the window
    /// manager is still applying the last configure request (the README
    /// documents this state-invalidation invariant).
    ///
    /// Takes `&mut self` because an invalid window is invalidated in place:
    /// its stale entry is removed here rather than kept around.
    pub fn get_valid(&mut self, window: Window, actual: Geometry) -> Option<TileState> {
        let state = self.windows.get(&window).copied()?;
        if close(actual, state.expected)
            || state
                .previous
                .is_some_and(|previous| close(actual, previous))
        {
            Some(state)
        } else {
            self.windows.remove(&window);
            None
        }
    }
}

/// How far (in pixels) positions may drift before a saved geometry no
/// longer counts as the same position. WMs round and adjust frame
/// geometry (borders, struts, odd sizes), so a saved state is still
/// trusted within this margin.
const POSITION_TOLERANCE: i32 = 16;
/// How far (in pixels) sizes may drift before a saved geometry no longer
/// counts as the same size. WMs round and adjust frame geometry, so a
/// saved state is still trusted within this margin.
const SIZE_TOLERANCE: i32 = 24;

fn close(a: Geometry, b: Geometry) -> bool {
    // Tolerant of WM border/frame adjustments and panel struts.
    a.x.abs_diff(b.x) <= POSITION_TOLERANCE as u32
        && a.y.abs_diff(b.y) <= POSITION_TOLERANCE as u32
        && a.width.abs_diff(b.width) <= SIZE_TOLERANCE as u32
        && a.height.abs_diff(b.height) <= SIZE_TOLERANCE as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(x: i32, y: i32, width: i32, height: i32) -> Geometry {
        Geometry {
            x,
            y,
            width,
            height,
        }
    }

    fn tile_state(expected: Geometry, previous: Option<Geometry>) -> TileState {
        TileState {
            monitor: 0,
            column: Column::Full,
            row: Row::Full,
            expected,
            previous,
        }
    }

    #[test]
    fn close_matches_identical_geometries() {
        let g = geo(100, 200, 800, 600);
        assert!(close(g, g));
    }

    #[test]
    fn close_tolerates_position_offsets_up_to_16() {
        let base = geo(100, 200, 800, 600);
        assert!(close(
            geo(base.x + 16, base.y, base.width, base.height),
            base
        ));
        assert!(!close(
            geo(base.x + 17, base.y, base.width, base.height),
            base
        ));
        assert!(close(
            geo(base.x, base.y + 16, base.width, base.height),
            base
        ));
        assert!(!close(
            geo(base.x, base.y + 17, base.width, base.height),
            base
        ));
    }

    #[test]
    fn close_tolerates_size_differences_up_to_24() {
        let base = geo(100, 200, 800, 600);
        assert!(close(
            geo(base.x, base.y, base.width + 24, base.height),
            base
        ));
        assert!(!close(
            geo(base.x, base.y, base.width + 25, base.height),
            base
        ));
        assert!(close(
            geo(base.x, base.y, base.width, base.height + 24),
            base
        ));
        assert!(!close(
            geo(base.x, base.y, base.width, base.height + 25),
            base
        ));
    }

    #[test]
    fn close_accepts_multiple_axes_within_tolerance() {
        let base = geo(100, 200, 800, 600);
        assert!(close(
            geo(base.x + 10, base.y - 5, base.width + 12, base.height - 8),
            base
        ));
    }

    #[test]
    fn get_valid_returns_state_when_actual_matches_expected() {
        let mut store = StateStore::default();
        let expected = geo(0, 0, 960, 540);
        store
            .windows
            .insert(1, tile_state(expected, Some(geo(0, 0, 1920, 1080))));

        let state = store
            .get_valid(1, expected)
            .expect("expected geometry must be accepted");
        assert_eq!(state.monitor, 0);
        assert_eq!(state.column, Column::Full);
        assert_eq!(state.row, Row::Full);
        assert_eq!(state.expected, expected);
    }

    #[test]
    fn get_valid_returns_state_when_actual_matches_previous() {
        let mut store = StateStore::default();
        let expected = geo(0, 0, 960, 540);
        let previous = geo(0, 0, 1920, 1080);
        store
            .windows
            .insert(1, tile_state(expected, Some(previous)));

        // The WM is still applying the previous configure: actual matches
        // previous even though it no longer matches expected.
        let state = store
            .get_valid(1, previous)
            .expect("previous geometry must be accepted");
        assert_eq!(state.expected, expected);
        assert_eq!(state.previous, Some(previous));
    }

    #[test]
    fn get_valid_removes_entry_when_actual_matches_neither() {
        let mut store = StateStore::default();
        store.windows.insert(
            1,
            tile_state(geo(0, 0, 960, 540), Some(geo(0, 0, 1920, 1080))),
        );
        let far = geo(5000, 5000, 10, 10);

        // Far from both expected and previous: entry is invalidated and removed.
        assert!(store.get_valid(1, far).is_none());
        assert!(store.windows.is_empty());
        // The removed entry cannot be resurrected.
        assert!(store.get_valid(1, far).is_none());
    }

    #[test]
    fn get_valid_returns_none_for_unknown_window() {
        let mut store = StateStore::default();
        assert!(store.get_valid(42, geo(0, 0, 10, 10)).is_none());
        assert!(store.windows.is_empty());
    }

    #[test]
    fn close_handles_extreme_coordinates_without_overflow() {
        let left = geo(i32::MIN, i32::MIN, i32::MIN, i32::MIN);
        let right = geo(i32::MAX, i32::MAX, i32::MAX, i32::MAX);
        assert!(!close(left, right));
    }
}
