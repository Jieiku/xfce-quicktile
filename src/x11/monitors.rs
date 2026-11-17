//! RandR monitor discovery and pure work-area calculations from EWMH struts.

use std::ops::Range;

use anyhow::Result;
use x11rb::{
    connection::Connection,
    protocol::{randr::ConnectionExt as RandrConnectionExt, xproto::Window},
    rust_connection::RustConnection,
};

use crate::tiling::Monitor;

/// A panel/dock strut as advertised via `_NET_WM_STRUT_PARTIAL` (or the
/// 4-value `_NET_WM_STRUT` fallback). Spans are half-open `Range<i32>` in
/// root coordinates; `None` means "no span advertised" and `apply_struts`
/// defaults that span to the monitor's full range on the axis.
///
/// Margins are `u32` because that is the wire CARDINAL type: the per-edge
/// margin sum is computed as wrapping u32 and cast to i32, and the
/// wrap-then-cast reject-by-zero semantics in `apply_struts` is deliberate.
/// Fields are `pub(super)` because the parent module (and later sibling
/// modules) parse the strut properties and construct `Strut` here.
pub(super) struct Strut {
    pub(super) left: u32,
    pub(super) right: u32,
    pub(super) top: u32,
    pub(super) bottom: u32,
    pub(super) left_y: Option<Range<i32>>,
    pub(super) right_y: Option<Range<i32>>,
    pub(super) top_x: Option<Range<i32>>,
    pub(super) bottom_x: Option<Range<i32>>,
}

/// Fetch active monitor rectangles via RANDR and sort them by `(x, y)` for
/// deterministic status output. Tiling navigation does not depend on this
/// order; it selects neighbors from the actual rectangle geometry.
///
/// When RANDR is unavailable or reports no monitors, fall back to one
/// monitor covering screen 0's pixel size. The fallback deliberately uses
/// `roots[0]` regardless of the connected screen number — that quirk is
/// preserved from the original implementation.
pub(super) fn fetch_monitors(conn: &RustConnection, root: Window) -> Result<Vec<Monitor>> {
    let reply = conn.randr_get_monitors(root, true)?.reply();
    let mut monitors: Vec<Monitor> = match reply {
        Ok(reply) if !reply.monitors.is_empty() => reply
            .monitors
            .into_iter()
            .map(|m| Monitor {
                x: m.x as i32,
                y: m.y as i32,
                width: m.width as i32,
                height: m.height as i32,
            })
            .collect(),
        _ => {
            let screen = &conn.setup().roots[0];
            vec![Monitor {
                x: 0,
                y: 0,
                width: screen.width_in_pixels as i32,
                height: screen.height_in_pixels as i32,
            }]
        }
    };

    // Keep monitor numbering deterministic for status messages. Directional
    // navigation uses geometry and remains correct for arbitrary layouts.
    monitors.sort_by_key(|m| (m.x, m.y));

    Ok(monitors)
}

/// Shrink each monitor to its work area using the same approach as
/// Yakuake: build strut band rectangles in root coordinates, keep only
/// those that intersect the monitor, then apply the max margin per edge.
///
/// Pure: no connection access — the caller has already read the client
/// list and parsed the struts. All rectangles are half-open `Range<i32>`.
pub(super) fn apply_struts(monitors: &mut [Monitor], struts: &[Strut]) {
    if struts.is_empty() {
        return;
    }

    for m in monitors.iter_mut() {
        let mx0 = m.x;
        let my0 = m.y;
        let mx1 = mx0 + m.width;
        let my1 = my0 + m.height;
        let mx = mx0..mx1;
        let my = my0..my1;

        let mut left = 0u32;
        let mut right = 0u32;
        let mut top = 0u32;
        let mut bottom = 0u32;

        for s in struts {
            // Top strut: band along global y=0..top spanning top_x. The top
            // edge is anchored to y=0 in root coordinates (where the panel
            // actually is), not to this monitor's top edge.
            if s.top > 0 {
                let band_x = span_or(&s.top_x, &mx);
                let band_y = 0..s.top as i32;
                if ranges_overlap(mx0..mx1, band_x) && ranges_overlap(my0..my1, band_y) {
                    top = top.max(s.top);
                }
            }

            // Bottom strut: anchored to THIS monitor's bottom edge — the
            // margin is measured inward from the monitor while the span
            // coordinates stay absolute root coordinates.
            if s.bottom > 0 {
                let band_x = span_or(&s.bottom_x, &mx);
                let band_y = (my1 - s.bottom as i32)..my1;
                if ranges_overlap(mx0..mx1, band_x) && ranges_overlap(my0..my1, band_y) {
                    bottom = bottom.max(s.bottom);
                }
            }

            // Left strut: anchored to the global left edge (x=0).
            if s.left > 0 {
                let band_y = span_or(&s.left_y, &my);
                let band_x = 0..s.left as i32;
                if ranges_overlap(my0..my1, band_y) && ranges_overlap(mx0..mx1, band_x) {
                    left = left.max(s.left);
                }
            }

            // Right strut: anchored to THIS monitor's right edge.
            if s.right > 0 {
                let band_y = span_or(&s.right_y, &my);
                let band_x = (mx1 - s.right as i32)..mx1;
                if ranges_overlap(my0..my1, band_y) && ranges_overlap(mx0..mx1, band_x) {
                    right = right.max(s.right);
                }
            }
        }

        // screenGeometry.adjusted(left, top, -right, -bottom)
        let new_x = mx0 + left as i32;
        let new_y = my0 + top as i32;
        // A negative cast means the (wrapping) u32 margin sum exceeded i32's positive
        // range, i.e. a malformed strut. The old u32 code saturated such sums to 0,
        // rejecting the update via the guard below, so keep that reject-by-zero semantics.
        let margin_w = (left + right) as i32;
        let new_w = if margin_w >= 0 {
            m.width.saturating_sub(margin_w)
        } else {
            0
        };
        let margin_h = (top + bottom) as i32;
        let new_h = if margin_h >= 0 {
            m.height.saturating_sub(margin_h)
        } else {
            0
        };
        if new_w > 0 && new_h > 0 {
            m.x = new_x;
            m.y = new_y;
            m.width = new_w;
            m.height = new_h;
        }
    }
}

/// Strict half-open intersection: touching edges do not overlap.
fn ranges_overlap(a: Range<i32>, b: Range<i32>) -> bool {
    a.start < b.end && b.start < a.end
}

/// The strut's span for an axis, or the monitor's full range if the strut
/// advertised none (`None`).
fn span_or(span: &Option<Range<i32>>, full: &Range<i32>) -> Range<i32> {
    span.clone().unwrap_or_else(|| full.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const M0: Monitor = Monitor {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    fn monitor(x: i32, y: i32, width: i32, height: i32) -> Monitor {
        Monitor {
            x,
            y,
            width,
            height,
        }
    }

    /// Monitor as a plain tuple; `Monitor` does not derive `PartialEq`
    /// and this package must not change `tiling.rs`.
    fn rect(m: &Monitor) -> (i32, i32, i32, i32) {
        (m.x, m.y, m.width, m.height)
    }

    /// A strut with the given margins and no advertised spans; tests set
    /// the span fields explicitly where a span matters.
    fn strut(left: u32, right: u32, top: u32, bottom: u32) -> Strut {
        Strut {
            left,
            right,
            top,
            bottom,
            left_y: None,
            right_y: None,
            top_x: None,
            bottom_x: None,
        }
    }

    #[test]
    fn empty_struts_leave_monitors_unchanged() {
        let mut monitors = vec![M0];
        apply_struts(&mut monitors, &[]);
        assert_eq!(rect(&monitors[0]), (0, 0, 1920, 1080));
    }

    #[test]
    fn top_panel_shrinks_work_area() {
        let mut monitors = vec![M0];
        let mut s = strut(0, 0, 27, 0);
        s.top_x = Some(0..1920);
        apply_struts(&mut monitors, &[s]);
        assert_eq!(rect(&monitors[0]), (0, 27, 1920, 1053));
    }

    #[test]
    fn bottom_panel_shrinks_work_area() {
        let mut monitors = vec![M0];
        let struts = [strut(0, 0, 0, 40)];
        apply_struts(&mut monitors, &struts);
        assert_eq!(rect(&monitors[0]), (0, 0, 1920, 1040));
    }

    #[test]
    fn left_dock_shrinks_work_area() {
        let mut monitors = vec![M0];
        let struts = [strut(60, 0, 0, 0)];
        apply_struts(&mut monitors, &struts);
        assert_eq!(rect(&monitors[0]), (60, 0, 1860, 1080));
    }

    #[test]
    fn right_dock_shrinks_work_area() {
        let mut monitors = vec![M0];
        let struts = [strut(0, 60, 0, 0)];
        apply_struts(&mut monitors, &struts);
        assert_eq!(rect(&monitors[0]), (0, 0, 1860, 1080));
    }

    #[test]
    fn partial_top_x_span_applies_only_where_it_overlaps() {
        let mut monitors = vec![M0, monitor(2000, 0, 1920, 1080)];
        let mut s = strut(0, 0, 27, 0);
        s.top_x = Some(1000..1400);
        apply_struts(&mut monitors, &[s]);
        assert_eq!(rect(&monitors[0]), (0, 27, 1920, 1053));
        assert_eq!(rect(&monitors[1]), (2000, 0, 1920, 1080));
    }

    #[test]
    fn plain_strut_defaults_spans_to_full_range() {
        // A plain _NET_WM_STRUT advertises no spans: every axis must fall
        // back to the monitor's full range, so the margin still applies.
        let mut monitors = vec![M0];
        let struts = [strut(0, 0, 27, 0)];
        apply_struts(&mut monitors, &struts);
        assert_eq!(rect(&monitors[0]), (0, 27, 1920, 1053));
    }

    #[test]
    fn multiple_struts_on_same_edge_take_max_margin() {
        let mut monitors = vec![M0];
        let mut small = strut(0, 0, 20, 0);
        small.top_x = Some(0..1920);
        let mut large = strut(0, 0, 30, 0);
        large.top_x = Some(0..1920);
        apply_struts(&mut monitors, &[small, large]);
        assert_eq!(rect(&monitors[0]), (0, 30, 1920, 1050));
    }

    #[test]
    fn combined_struts_apply_all_four_margins() {
        let mut monitors = vec![M0];
        let mut top = strut(0, 0, 27, 0);
        top.top_x = Some(0..1920);
        let mut bottom = strut(0, 0, 0, 40);
        bottom.bottom_x = Some(0..1920);
        let mut left = strut(60, 0, 0, 0);
        left.left_y = Some(0..1080);
        let mut right = strut(0, 60, 0, 0);
        right.right_y = Some(0..1080);
        apply_struts(&mut monitors, &[top, bottom, left, right]);
        assert_eq!(rect(&monitors[0]), (60, 27, 1800, 1013));
    }

    #[test]
    fn oversize_margin_rejects_whole_update() {
        // top=2000 on a 1080-tall monitor: the guard rejects the whole
        // x/y/w/h update, so the monitor is left untouched.
        let mut monitors = vec![M0];
        let mut s = strut(0, 0, 2000, 0);
        s.top_x = Some(0..1920);
        apply_struts(&mut monitors, &[s]);
        assert_eq!(rect(&monitors[0]), (0, 0, 1920, 1080));
    }

    #[test]
    fn malformed_u32_margin_rejects_update() {
        // left=0x80000000 is a malformed CARDINAL: whatever path rejects
        // it (inverted band or wrap-then-cast margin), the monitor must
        // come out unchanged. Pins the P2-repair semantics.
        let mut monitors = vec![M0];
        let struts = [strut(0x8000_0000, 0, 0, 0)];
        apply_struts(&mut monitors, &struts);
        assert_eq!(rect(&monitors[0]), (0, 0, 1920, 1080));
    }

    #[test]
    fn wrapping_u32_margin_sum_rejects_update() {
        // Both margins are individually legal (i32::MAX fits in i32) but
        // their wrapping u32 sum casts to a negative i32: the
        // reject-by-zero path must reject the whole update.
        let mut monitors = vec![M0];
        let max = i32::MAX as u32;
        let struts = [strut(max, max, 0, 0)];
        apply_struts(&mut monitors, &struts);
        assert_eq!(rect(&monitors[0]), (0, 0, 1920, 1080));
    }

    #[test]
    fn two_monitors_only_the_spanned_one_changes() {
        let mut monitors = vec![M0, monitor(1920, 0, 1920, 1080)];
        // Bottom panel whose x span covers only the first monitor.
        let mut s = strut(0, 0, 0, 40);
        s.bottom_x = Some(0..1920);
        apply_struts(&mut monitors, &[s]);
        assert_eq!(rect(&monitors[0]), (0, 0, 1920, 1040));
        assert_eq!(rect(&monitors[1]), (1920, 0, 1920, 1080));
    }

    #[test]
    fn ranges_overlap_is_strict_half_open() {
        assert!(ranges_overlap(0..10, 5..15));
        assert!(ranges_overlap(0..10, -5..5));
        assert!(ranges_overlap(0..1, 0..1));
        assert!(!ranges_overlap(0..10, 10..20)); // touching edges
        assert!(!ranges_overlap(10..20, 0..10));
        assert!(!ranges_overlap(0..10, -5..0));
        assert!(!ranges_overlap(0..10, 100..200));
        assert!(!ranges_overlap(0..0, 0..10)); // empty range
    }
}
