//! Pure directional tiling state machine and monitor geometry calculations.
//!
//! Monitor indices are only temporary handles into the current RandR snapshot;
//! navigation is based on physical monitor rectangles, never work areas, discovery,
//! or sort order. Work-area rectangles are used only for tile inference and target
//! placement. X11 supplies matching physical/work-area snapshots and applies the
//! resulting target geometry.

use anyhow::{bail, Result};

use crate::state::{Column, Geometry, Row, TileState};

#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Monitor {
    fn contains_point(self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

pub fn monitor_for_window(monitors: &[Monitor], geometry: Geometry) -> usize {
    // Compute the window center once; the nearest-monitor fallback reuses it.
    let (cx, cy) = geometry.center();
    monitors
        .iter()
        .position(|monitor| monitor.contains_point(cx, cy))
        .unwrap_or_else(|| {
            monitors
                .iter()
                .enumerate()
                .min_by_key(|(_, monitor)| {
                    let mx = monitor.x as i64 + monitor.width as i64 / 2;
                    let my = monitor.y as i64 + monitor.height as i64 / 2;
                    (cx as i64 - mx).abs() + (cy as i64 - my).abs()
                })
                .map_or(0, |(index, _)| index)
        })
}

/// Recover the logical tile position from the window's current geometry.
/// Used when we have no valid saved state so consecutive presses still
/// cycle correctly even if the WM adjusted the size slightly.
fn infer_state(monitors: &[Monitor], monitor: usize, actual: Geometry) -> TileState {
    let m = monitors[monitor];

    let (center_x, center_y) = actual.center();

    // Treat anything covering most of the monitor axis as "Full".
    // Integer cross-multiplication (10*w >= 7*h): exact pixel math, no float
    // precision concerns (and no i32 overflow at X11-sized values).
    let column = if 10 * actual.width >= 7 * m.width {
        Column::Full
    } else if center_x < m.x + m.width / 2 {
        Column::Left
    } else {
        Column::Right
    };

    let row = if 10 * actual.height >= 7 * m.height {
        Row::Full
    } else if center_y < m.y + m.height / 2 {
        Row::Top
    } else {
        Row::Bottom
    };

    TileState {
        monitor,
        column,
        row,
        expected: actual,
        previous: None,
    }
}

/// Advance a window through its local tile states, crossing to the geometrically
/// nearest monitor when it reaches an edge in the requested direction.
///
/// Uses work-area rectangles for tile inference and physical monitor
/// rectangles for monitor selection and adjacency. Both slices must describe
/// the same RandR snapshot in the same stable order.
///
/// KDE/Plasma-style behaviour:
/// - When the window occupies a pure half (the perpendicular axis is Full),
///   pressing the opposite direction flips it to the other half instead of
///   expanding to full-screen.
/// - When the window occupies a quadrant (both axes half), pressing a
///   direction expands that axis to Full (the only case that "extends").
/// - From Full on an axis, the direction moves to the corresponding half.
/// - Crossing a monitor boundary lands on the opposite half of the destination
///   monitor, preserving the tile on the perpendicular axis.
///
/// At an outer edge with no neighboring monitor in the requested direction,
/// all four directions return `Ok(None)` ("already at the desktop boundary").
pub fn advance_with_topology(
    direction: Direction,
    work_areas: &[Monitor],
    physical_monitors: &[Monitor],
    actual: Geometry,
    existing: Option<TileState>,
) -> Result<Option<TileState>> {
    if physical_monitors.is_empty() {
        bail!("no active monitors found");
    }
    if work_areas.len() != physical_monitors.len() {
        bail!("work-area and physical monitor snapshots do not match");
    }

    // Indices belong to one RandR snapshot only. Resolve the live window's
    // monitor against physical geometry; use its work area only for tile math.
    let monitor = monitor_for_window(physical_monitors, actual);
    let mut state = existing.unwrap_or_else(|| infer_state(work_areas, monitor, actual));
    state.monitor = monitor;

    match direction {
        Direction::Left => match state.column {
            Column::Right => {
                // Pure right half → flip to left half; quadrant → expand to full width.
                if state.row == Row::Full {
                    state.column = Column::Left;
                } else {
                    state.column = Column::Full;
                }
            }
            Column::Full => state.column = Column::Left,
            Column::Left => {
                let Some(monitor) = neighbor_monitor(physical_monitors, state.monitor, direction)
                else {
                    return Ok(None);
                };
                state.monitor = monitor;
                state.column = Column::Right;
            }
        },
        Direction::Right => match state.column {
            Column::Left => {
                // Pure left half → flip to right half; quadrant → expand to full width.
                if state.row == Row::Full {
                    state.column = Column::Right;
                } else {
                    state.column = Column::Full;
                }
            }
            Column::Full => state.column = Column::Right,
            Column::Right => {
                let Some(monitor) = neighbor_monitor(physical_monitors, state.monitor, direction)
                else {
                    return Ok(None);
                };
                state.monitor = monitor;
                state.column = Column::Left;
            }
        },
        Direction::Up => match state.row {
            Row::Bottom => {
                // Pure bottom half → flip to top half; quadrant → expand to full height.
                if state.column == Column::Full {
                    state.row = Row::Top;
                } else {
                    state.row = Row::Full;
                }
            }
            Row::Full => state.row = Row::Top,
            Row::Top => {
                let Some(monitor) = neighbor_monitor(physical_monitors, state.monitor, direction)
                else {
                    return Ok(None);
                };
                state.monitor = monitor;
                state.row = Row::Bottom;
            }
        },
        Direction::Down => match state.row {
            Row::Top => {
                // Pure top half → flip to bottom half; quadrant → expand to full height.
                if state.column == Column::Full {
                    state.row = Row::Bottom;
                } else {
                    state.row = Row::Full;
                }
            }
            Row::Full => state.row = Row::Bottom,
            Row::Bottom => {
                let Some(monitor) = neighbor_monitor(physical_monitors, state.monitor, direction)
                else {
                    return Ok(None);
                };
                state.monitor = monitor;
                state.row = Row::Top;
            }
        },
    }

    Ok(Some(state))
}

/// Find the best monitor in `direction` from `source`.
///
/// Candidates must have their center in the requested direction and have a
/// non-zero physical overlap on the perpendicular axis. The ranking prefers
/// the greatest overlap, then the smallest edge-to-edge gap, then the closest
/// perpendicular center. This handles staggered and mixed-resolution layouts
/// without assuming a linear ordering.
fn neighbor_monitor(monitors: &[Monitor], source: usize, direction: Direction) -> Option<usize> {
    let current = *monitors.get(source)?;
    let current_center_x = i64::from(current.x) + i64::from(current.width) / 2;
    let current_center_y = i64::from(current.y) + i64::from(current.height) / 2;
    let current_right = i64::from(current.x) + i64::from(current.width);
    let current_bottom = i64::from(current.y) + i64::from(current.height);

    monitors
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            if *index == source {
                return false;
            }
            let center_x = i64::from(candidate.x) + i64::from(candidate.width) / 2;
            let center_y = i64::from(candidate.y) + i64::from(candidate.height) / 2;
            let is_in_direction = match direction {
                Direction::Left => center_x < current_center_x,
                Direction::Right => center_x > current_center_x,
                Direction::Up => center_y < current_center_y,
                Direction::Down => center_y > current_center_y,
            };
            if !is_in_direction {
                return false;
            }

            // A monitor is only a directional neighbor when the physical
            // rectangles share a non-zero span on the perpendicular axis.
            let candidate_right = i64::from(candidate.x) + i64::from(candidate.width);
            let candidate_bottom = i64::from(candidate.y) + i64::from(candidate.height);
            match direction {
                Direction::Left | Direction::Right => {
                    interval_overlap(
                        i64::from(current.y),
                        current_bottom,
                        i64::from(candidate.y),
                        candidate_bottom,
                    ) > 0
                }
                Direction::Up | Direction::Down => {
                    interval_overlap(
                        i64::from(current.x),
                        current_right,
                        i64::from(candidate.x),
                        candidate_right,
                    ) > 0
                }
            }
        })
        .min_by_key(|(_, candidate)| {
            let x0 = i64::from(candidate.x);
            let y0 = i64::from(candidate.y);
            let x1 = x0 + i64::from(candidate.width);
            let y1 = y0 + i64::from(candidate.height);
            let center_x = x0 + i64::from(candidate.width) / 2;
            let center_y = y0 + i64::from(candidate.height) / 2;

            let (overlap, gap, perpendicular_distance) = match direction {
                Direction::Left | Direction::Right => {
                    let overlap = interval_overlap(i64::from(current.y), current_bottom, y0, y1);
                    let gap = match direction {
                        Direction::Left => (i64::from(current.x) - x1).max(0),
                        Direction::Right => (x0 - current_right).max(0),
                        Direction::Up | Direction::Down => unreachable!(),
                    };
                    (overlap, gap, (center_y - current_center_y).abs())
                }
                Direction::Up | Direction::Down => {
                    let overlap = interval_overlap(i64::from(current.x), current_right, x0, x1);
                    let gap = match direction {
                        Direction::Up => (i64::from(current.y) - y1).max(0),
                        Direction::Down => (y0 - current_bottom).max(0),
                        Direction::Left | Direction::Right => unreachable!(),
                    };
                    (overlap, gap, (center_x - current_center_x).abs())
                }
            };

            // `min_by_key`: negate overlap so larger shared edges win.
            (-overlap, gap, perpendicular_distance)
        })
        .map(|(index, _)| index)
}

fn interval_overlap(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0)
}

pub fn target_geometry(state: TileState, monitors: &[Monitor]) -> Geometry {
    let monitor = monitors[state.monitor];
    let left_width = monitor.width / 2;

    let (x, width) = match state.column {
        Column::Full => (monitor.x, monitor.width),
        Column::Left => (monitor.x, left_width),
        Column::Right => (monitor.x + left_width, monitor.width - left_width),
    };

    let (y, height) = match state.row {
        Row::Full => (monitor.y, monitor.height),
        Row::Top => (monitor.y, monitor.height / 2),
        Row::Bottom => {
            let top_height = monitor.height / 2;
            (monitor.y + top_height, monitor.height - top_height)
        }
    };

    Geometry {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i32, y: i32, width: i32, height: i32) -> Monitor {
        Monitor {
            x,
            y,
            width,
            height,
        }
    }

    fn geo(x: i32, y: i32, width: i32, height: i32) -> Geometry {
        Geometry {
            x,
            y,
            width,
            height,
        }
    }

    fn tile_state(index: usize, column: Column, row: Row) -> TileState {
        TileState {
            monitor: index,
            column,
            row,
            expected: geo(0, 0, 100, 100),
            previous: None,
        }
    }

    const M0: Monitor = Monitor {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    const M1: Monitor = Monitor {
        x: 1920,
        y: 0,
        width: 1920,
        height: 1080,
    };
    const SINGLE: [Monitor; 1] = [M0];
    const PAIR: [Monitor; 2] = [M0, M1];

    // `infer_state` is private, so it is pinned through
    // `advance_with_topology` with `existing = None`. Up and Down leave `monitor` and `column` untouched
    // and map the row (Up: Bottom -> Full, Full -> Top, Top -> boundary;
    // Down: Top -> Full, Full -> Bottom, Bottom -> boundary), so the pair of
    // results determines the inferred (monitor, column, row) unambiguously.
    // It unwraps both results, so only use it when the inferred row is Full;
    // Top/Bottom inferences hit the boundary in one direction and are pinned
    // by tests that call `advance_with_topology` directly.
    fn inferred_up_down(monitors: &[Monitor], actual: Geometry) -> (TileState, TileState) {
        let up = advance_with_topology(Direction::Up, monitors, monitors, actual, None)
            .unwrap()
            .unwrap();
        let down = advance_with_topology(Direction::Down, monitors, monitors, actual, None)
            .unwrap()
            .unwrap();
        (up, down)
    }

    #[test]
    fn pure_half_right_flips_to_other_half() {
        // KDE-style: pure left half + Right → pure right half (does not go full).
        let actual = geo(0, 0, 1, 1);
        let next = advance_with_topology(
            Direction::Right,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Left, Row::Full)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Right, Row::Full)
        );
    }

    #[test]
    fn pure_half_left_flips_to_other_half() {
        // KDE-style: pure right half + Left → pure left half (does not go full).
        let actual = geo(0, 0, 1, 1);
        let next = advance_with_topology(
            Direction::Left,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Right, Row::Full)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Left, Row::Full)
        );
    }

    #[test]
    fn pure_half_down_flips_to_other_half() {
        // KDE-style: pure top half + Down → pure bottom half (does not go full).
        let actual = geo(0, 0, 1, 1);
        let next = advance_with_topology(
            Direction::Down,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Full, Row::Top)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Full, Row::Bottom)
        );
    }

    #[test]
    fn pure_half_up_flips_to_other_half() {
        // KDE-style: pure bottom half + Up → pure top half (does not go full).
        let actual = geo(0, 0, 1, 1);
        let next = advance_with_topology(
            Direction::Up,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Full, Row::Bottom)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Full, Row::Top)
        );
    }

    #[test]
    fn quadrant_still_extends_on_horizontal() {
        // Quadrants still expand (the only case that extends rather than flips).
        let actual = geo(0, 0, 1, 1);
        let next = advance_with_topology(
            Direction::Right,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Left, Row::Top)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Full, Row::Top)
        );
    }

    #[test]
    fn advance_right_single_monitor() {
        // `actual` is ignored while a saved state is supplied.
        let actual = geo(0, 0, 1, 1);

        let from_left = advance_with_topology(
            Direction::Right,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Left, Row::Bottom)),
        )
        .unwrap()
        .unwrap();
        // Horizontal movement must keep the row.
        assert_eq!(
            (from_left.monitor, from_left.column, from_left.row),
            (0, Column::Full, Row::Bottom)
        );

        let from_full = advance_with_topology(
            Direction::Right,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Full, Row::Top)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_full.monitor, from_full.column, from_full.row),
            (0, Column::Right, Row::Top)
        );

        // Right on a single monitor is the desktop boundary.
        assert!(advance_with_topology(
            Direction::Right,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Right, Row::Full))
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advance_left_single_monitor() {
        let actual = geo(0, 0, 1, 1);

        let from_right = advance_with_topology(
            Direction::Left,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Right, Row::Top)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_right.monitor, from_right.column, from_right.row),
            (0, Column::Full, Row::Top)
        );

        let from_full = advance_with_topology(
            Direction::Left,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Full, Row::Bottom)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_full.monitor, from_full.column, from_full.row),
            (0, Column::Left, Row::Bottom)
        );

        // Left on a single monitor is the desktop boundary.
        assert!(advance_with_topology(
            Direction::Left,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Left, Row::Top))
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advance_up_single_monitor() {
        let actual = geo(0, 0, 1, 1);

        // Vertical advances must keep column and monitor.
        let from_bottom = advance_with_topology(
            Direction::Up,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Right, Row::Bottom)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_bottom.monitor, from_bottom.column, from_bottom.row),
            (0, Column::Right, Row::Full)
        );

        let from_full = advance_with_topology(
            Direction::Up,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Left, Row::Full)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_full.monitor, from_full.column, from_full.row),
            (0, Column::Left, Row::Top)
        );

        // Already at the top with no monitor above: the desktop boundary,
        // mirroring the horizontal outer-edge stop.
        assert!(advance_with_topology(
            Direction::Up,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Full, Row::Top)),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advance_down_single_monitor() {
        let actual = geo(0, 0, 1, 1);

        let from_top = advance_with_topology(
            Direction::Down,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Left, Row::Top)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_top.monitor, from_top.column, from_top.row),
            (0, Column::Left, Row::Full)
        );

        let from_full = advance_with_topology(
            Direction::Down,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Right, Row::Full)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (from_full.monitor, from_full.column, from_full.row),
            (0, Column::Right, Row::Bottom)
        );

        // Already at the bottom with no monitor below: the desktop boundary,
        // mirroring the horizontal outer-edge stop.
        assert!(advance_with_topology(
            Direction::Down,
            &SINGLE,
            &SINGLE,
            actual,
            Some(tile_state(0, Column::Full, Row::Bottom)),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advance_left_crosses_to_previous_monitor() {
        let expected = geo(0, 0, 960, 540);
        let previous = geo(0, 0, 1920, 1080);
        let existing = TileState {
            monitor: 1,
            column: Column::Left,
            row: Row::Top,
            expected,
            previous: Some(previous),
        };

        let next = advance_with_topology(
            Direction::Left,
            &PAIR,
            &PAIR,
            geo(1920, 0, 1, 1),
            Some(existing),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Right, Row::Top)
        );
        // advance_with_topology only changes monitor/column/row; expected and
        // previous carry over.
        assert_eq!(next.expected, expected);
        assert_eq!(next.previous, Some(previous));
    }

    #[test]
    fn advance_right_crosses_to_next_monitor() {
        let next = advance_with_topology(
            Direction::Right,
            &PAIR,
            &PAIR,
            geo(0, 0, 1, 1),
            Some(tile_state(0, Column::Right, Row::Bottom)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (1, Column::Left, Row::Bottom)
        );
    }

    #[test]
    fn advance_left_at_first_monitor_returns_none() {
        assert!(advance_with_topology(
            Direction::Left,
            &PAIR,
            &PAIR,
            geo(0, 0, 1, 1),
            Some(tile_state(0, Column::Left, Row::Top))
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advance_right_at_last_monitor_returns_none() {
        assert!(advance_with_topology(
            Direction::Right,
            &PAIR,
            &PAIR,
            geo(1920, 0, 1, 1),
            Some(tile_state(1, Column::Right, Row::Top))
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advance_infers_state_from_actual_when_no_saved_state() {
        // (960, 0, 960, 540) is the right/top quadrant of a 1920x1080 monitor,
        // so the inferred state is (monitor 0, Right, Top).
        let actual = geo(960, 0, 960, 540);

        let left = advance_with_topology(Direction::Left, &SINGLE, &SINGLE, actual, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            (left.monitor, left.column, left.row),
            (0, Column::Full, Row::Top)
        );

        // Right from the inferred Right column on a single monitor: boundary.
        assert!(
            advance_with_topology(Direction::Right, &SINGLE, &SINGLE, actual, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn advance_empty_monitors_errors() {
        let err =
            advance_with_topology(Direction::Left, &[], &[], geo(0, 0, 1, 1), None).unwrap_err();
        assert!(err.to_string().contains("no active monitors"));
    }

    #[test]
    fn advance_preserves_expected_and_previous() {
        let expected = geo(0, 0, 960, 540);
        let previous = geo(0, 0, 1920, 1080);
        let existing = TileState {
            monitor: 0,
            column: Column::Left,
            row: Row::Top,
            expected,
            previous: Some(previous),
        };

        let next = advance_with_topology(
            Direction::Right,
            &SINGLE,
            &SINGLE,
            geo(0, 0, 1, 1),
            Some(existing),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Full, Row::Top)
        );
        assert_eq!(next.expected, expected);
        assert_eq!(next.previous, Some(previous));
    }

    #[test]
    fn adjacency_uses_physical_geometry_not_strut_adjusted_work_areas() {
        let physical = [monitor(0, 0, 1920, 1080), monitor(1920, 0, 1920, 1080)];
        // Simulate different panel struts shifting the usable rectangles
        // vertically. They must not make the two side-by-side displays
        // appear to be vertical neighbors.
        let work_areas = [monitor(0, 40, 1920, 1040), monitor(1920, 0, 1920, 1080)];
        let actual = geo(0, 40, 960, 520);
        let existing = Some(tile_state(0, Column::Left, Row::Top));

        assert!(
            advance_with_topology(Direction::Up, &work_areas, &physical, actual, existing,)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn adjacency_rejects_monitors_without_perpendicular_physical_overlap() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 1080, 1920, 1080)];
        let next = advance_from(Direction::Right, &monitors, 0, Column::Right, Row::Full);
        assert!(next.is_none());
    }

    #[test]
    fn target_geometry_full_on_even_monitor() {
        assert_eq!(
            target_geometry(tile_state(0, Column::Full, Row::Full), &SINGLE),
            geo(0, 0, 1920, 1080)
        );
    }

    #[test]
    fn target_geometry_quadrants_on_even_monitor() {
        assert_eq!(
            target_geometry(tile_state(0, Column::Left, Row::Top), &SINGLE),
            geo(0, 0, 960, 540)
        );
        assert_eq!(
            target_geometry(tile_state(0, Column::Right, Row::Bottom), &SINGLE),
            geo(960, 540, 960, 540)
        );
        assert_eq!(
            target_geometry(tile_state(0, Column::Left, Row::Bottom), &SINGLE),
            geo(0, 540, 960, 540)
        );
        assert_eq!(
            target_geometry(tile_state(0, Column::Right, Row::Top), &SINGLE),
            geo(960, 0, 960, 540)
        );
    }

    #[test]
    fn target_geometry_odd_size_monitor_gives_right_and_bottom_the_extra_pixel() {
        let monitors = [monitor(0, 0, 1001, 1001)];
        assert_eq!(
            target_geometry(tile_state(0, Column::Left, Row::Full), &monitors),
            geo(0, 0, 500, 1001)
        );
        assert_eq!(
            target_geometry(tile_state(0, Column::Right, Row::Full), &monitors),
            geo(500, 0, 501, 1001)
        );
        assert_eq!(
            target_geometry(tile_state(0, Column::Full, Row::Top), &monitors),
            geo(0, 0, 1001, 500)
        );
        assert_eq!(
            target_geometry(tile_state(0, Column::Full, Row::Bottom), &monitors),
            geo(0, 500, 1001, 501)
        );
    }

    #[test]
    fn target_geometry_respects_monitor_offset() {
        let monitors = [monitor(1920, 100, 1920, 1080)];
        assert_eq!(
            target_geometry(tile_state(0, Column::Left, Row::Full), &monitors),
            geo(1920, 100, 960, 1080)
        );
    }

    #[test]
    fn monitor_for_window_picks_monitor_containing_center() {
        // Center (50, 50) lies inside monitor 0.
        assert_eq!(monitor_for_window(&PAIR, geo(0, 0, 100, 100)), 0);
        // Center (2450, 50) lies inside monitor 1.
        assert_eq!(monitor_for_window(&PAIR, geo(2400, 0, 100, 100)), 1);
    }

    #[test]
    fn monitor_for_window_falls_back_to_nearest_monitor_center() {
        // Center (1200, 50) is inside monitor 0.
        assert_eq!(monitor_for_window(&PAIR, geo(1000, 0, 400, 100)), 0);
        // Center (-400, 50) is off both monitors; monitor 0's center (960, 540)
        // is closer (Manhattan) than monitor 1's (2880, 540).
        assert_eq!(monitor_for_window(&PAIR, geo(-500, 0, 200, 100)), 0);
        // Center (4000, 50) is off both monitors; monitor 1's center is closer.
        assert_eq!(monitor_for_window(&PAIR, geo(3900, 0, 200, 100)), 1);
    }

    #[test]
    fn infer_state_full_when_actual_covers_monitor() {
        let monitors = [monitor(0, 0, 1000, 1000)];
        let actual = geo(0, 0, 1000, 1000);
        let (up, down) = inferred_up_down(&monitors, actual);
        assert_eq!((up.monitor, up.column, up.row), (0, Column::Full, Row::Top));
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Full, Row::Bottom)
        );
        // infer_state seeds `expected` with the actual geometry, no previous.
        assert_eq!(up.expected, actual);
        assert_eq!(up.previous, None);
    }

    #[test]
    fn infer_state_left_half_width() {
        let monitors = [monitor(0, 0, 1000, 1000)];
        let (up, down) = inferred_up_down(&monitors, geo(0, 0, 500, 1000));
        assert_eq!((up.monitor, up.column, up.row), (0, Column::Left, Row::Top));
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Left, Row::Bottom)
        );
    }

    #[test]
    fn infer_state_right_half_width() {
        let monitors = [monitor(0, 0, 1000, 1000)];
        let (up, down) = inferred_up_down(&monitors, geo(500, 0, 500, 1000));
        assert_eq!(
            (up.monitor, up.column, up.row),
            (0, Column::Right, Row::Top)
        );
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Right, Row::Bottom)
        );
    }

    #[test]
    fn infer_state_top_half_height() {
        let monitors = [monitor(0, 0, 1000, 1000)];
        let actual = geo(0, 0, 1000, 500);
        // A window in the top half is inferred as pure Top (full width):
        // Up from Top is the desktop boundary on a single monitor, Down
        // flips to Bottom (KDE-style). Together they fix the inferred
        // state as (0, Full, Top).
        let up = advance_with_topology(Direction::Up, &monitors, &monitors, actual, None).unwrap();
        assert!(up.is_none());
        let down = advance_with_topology(Direction::Down, &monitors, &monitors, actual, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Full, Row::Bottom)
        );
    }

    #[test]
    fn infer_state_bottom_half_height() {
        let monitors = [monitor(0, 0, 1000, 1000)];
        let actual = geo(0, 500, 1000, 500);
        // A window in the bottom half is inferred as pure Bottom (full width):
        // Down from Bottom is the desktop boundary, Up flips to Top
        // (KDE-style). Together they fix the inferred state as (0, Full, Bottom).
        let up = advance_with_topology(Direction::Up, &monitors, &monitors, actual, None)
            .unwrap()
            .unwrap();
        assert_eq!((up.monitor, up.column, up.row), (0, Column::Full, Row::Top));
        let down =
            advance_with_topology(Direction::Down, &monitors, &monitors, actual, None).unwrap();
        assert!(down.is_none());
    }

    #[test]
    fn infer_state_width_700_of_1000_is_full() {
        // 10*700 = 7000 >= 7*1000 = 7000, so the cross-multiplication
        // classifies it as Full.
        let monitors = [monitor(0, 0, 1000, 1000)];
        let (up, down) = inferred_up_down(&monitors, geo(0, 0, 700, 1000));
        assert_eq!((up.monitor, up.column, up.row), (0, Column::Full, Row::Top));
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Full, Row::Bottom)
        );
    }

    #[test]
    fn infer_state_width_699_of_1000_is_left_not_full() {
        // 10*699 = 6990 < 7*1000 = 7000; center_x (349) < 500, so Left,
        // NOT Full.
        let monitors = [monitor(0, 0, 1000, 1000)];
        let (up, down) = inferred_up_down(&monitors, geo(0, 0, 699, 1000));
        assert_eq!((up.monitor, up.column, up.row), (0, Column::Left, Row::Top));
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Left, Row::Bottom)
        );
    }

    #[test]
    fn infer_state_height_700_of_1000_is_full() {
        // 10*700 = 7000 >= 7*1000 = 7000, so the cross-multiplication
        // classifies it as Full.
        let monitors = [monitor(0, 0, 1000, 1000)];
        let (up, down) = inferred_up_down(&monitors, geo(0, 0, 1000, 700));
        assert_eq!((up.monitor, up.column, up.row), (0, Column::Full, Row::Top));
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Full, Row::Bottom)
        );
    }

    #[test]
    fn infer_state_height_699_of_1000_is_top_not_full() {
        // 10*699 = 6990 < 7*1000 = 7000; center_y (349) < 500, so Top,
        // NOT Full. Inferred as pure Top half (full width): Up is the
        // desktop boundary and Down flips to Bottom (KDE-style; a quadrant
        // Top would instead expand to Full).
        let monitors = [monitor(0, 0, 1000, 1000)];
        let actual = geo(0, 0, 1000, 699);
        let up = advance_with_topology(Direction::Up, &monitors, &monitors, actual, None).unwrap();
        assert!(up.is_none());
        let down = advance_with_topology(Direction::Down, &monitors, &monitors, actual, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            (down.monitor, down.column, down.row),
            (0, Column::Full, Row::Bottom)
        );
    }

    #[test]
    fn advance_resolves_stale_monitor_index_from_live_geometry() {
        // Monitor 7 does not exist in this snapshot: the index is re-resolved
        // from the live window position, while the saved column/row (which
        // match the live geometry) carry over.
        let stale = tile_state(7, Column::Right, Row::Bottom);
        // Bottom-right quadrant of monitor 0: consistent with (Right, Bottom).
        let actual = geo(960, 540, 960, 540);

        let next = advance_with_topology(Direction::Left, &SINGLE, &SINGLE, actual, Some(stale))
            .unwrap()
            .unwrap();

        assert_eq!(
            (next.monitor, next.column, next.row),
            (0, Column::Full, Row::Bottom)
        );
    }

    fn advance_from(
        direction: Direction,
        monitors: &[Monitor],
        monitor: usize,
        column: Column,
        row: Row,
    ) -> Option<TileState> {
        advance_with_topology(
            direction,
            monitors,
            monitors,
            geo(
                monitors[monitor].x,
                monitors[monitor].y,
                monitors[monitor].width,
                monitors[monitor].height,
            ),
            Some(tile_state(monitor, column, row)),
        )
        .unwrap()
    }

    #[test]
    fn right_navigation_uses_geometry_not_monitor_sort_order() {
        let monitors = [
            monitor(0, 0, 1920, 1080),
            monitor(1920, 500, 1280, 1024),
            monitor(-1200, 0, 1200, 1920),
        ];
        let next = advance_from(Direction::Right, &monitors, 0, Column::Right, Row::Full).unwrap();
        assert_eq!((next.monitor, next.column), (1, Column::Left));
    }

    #[test]
    fn left_navigation_handles_negative_coordinates() {
        let monitors = [monitor(-1200, 0, 1200, 1920), monitor(0, 0, 1920, 1080)];
        let next = advance_from(Direction::Left, &monitors, 1, Column::Left, Row::Full).unwrap();
        assert_eq!((next.monitor, next.column), (0, Column::Right));
    }

    #[test]
    fn vertical_navigation_crosses_to_monitor_above() {
        let monitors = [monitor(0, 0, 2560, 1440), monitor(400, -1200, 1920, 1200)];
        let next = advance_from(Direction::Up, &monitors, 0, Column::Right, Row::Top).unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (1, Column::Right, Row::Bottom)
        );
    }

    #[test]
    fn vertical_navigation_crosses_to_monitor_below() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(-300, 1080, 2560, 1440)];
        let next = advance_from(Direction::Down, &monitors, 0, Column::Left, Row::Bottom).unwrap();
        assert_eq!(
            (next.monitor, next.column, next.row),
            (1, Column::Left, Row::Top)
        );
    }

    #[test]
    fn neighbor_prefers_monitor_with_greatest_perpendicular_overlap() {
        let monitors = [
            monitor(0, 0, 1920, 1080),
            monitor(1920, 900, 1920, 1080),
            monitor(1920, 100, 1280, 700),
        ];
        let next = advance_from(Direction::Right, &monitors, 0, Column::Right, Row::Full).unwrap();
        assert_eq!(next.monitor, 2);
    }

    #[test]
    fn no_geometric_neighbor_is_a_boundary() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(0, 1200, 1920, 1080)];
        assert!(advance_from(Direction::Right, &monitors, 0, Column::Right, Row::Full).is_none());
    }

    #[test]
    fn saved_monitor_index_is_reconciled_with_live_window_geometry() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 0, 1920, 1080)];
        // Saved index points to monitor 0, but the window is actually in the
        // pure right half of monitor 1: the index is re-resolved from the live
        // geometry and Left flips it to the left half on monitor 1 (KDE-style).
        let next = advance_with_topology(
            Direction::Left,
            &monitors,
            &monitors,
            geo(2880, 0, 960, 1080),
            Some(tile_state(0, Column::Right, Row::Full)),
        )
        .unwrap()
        .unwrap();
        assert_eq!((next.monitor, next.column), (1, Column::Left));
    }
}
