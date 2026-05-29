//! Interaction math: pure functions mapping pointer input to new data limits.
//!
//! The widget reads egui input, converts it through the *current* on-screen
//! [`Transform`](crate::core::transform::Transform), and applies one of these to
//! produce the next limits. Because everything downstream (the wgpu ortho matrix
//! and the egui chrome) derives from those limits, the image, curve, and axes
//! move together with no extra bookkeeping (`doc/design.md` §4·§8·§11.6).
//!
//! Mouse mapping (silx default): left-drag = box zoom, right-drag = pan,
//! wheel = cursor-anchored zoom, double-click = reset. The mapping lives in the
//! widget; this module is just the geometry, kept pure so it is unit-testable.

use egui::{Pos2, Rect, Vec2};

use crate::core::transform::Transform;

/// Data limits `(x_min, x_max, y_min, y_max)`.
pub type Limits = (f64, f64, f64, f64);

/// Translate `limits` by a screen-space drag delta (pixels) so the data point
/// under the pointer stays under the pointer (the content follows the cursor).
///
/// Screen `+x` is right and `+y` is down; the Y axis is flipped (data `y_max` at
/// the top), so a downward drag increases the data Y limits.
pub fn pan(limits: Limits, area: Rect, delta_px: Vec2) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    let w = area.width().max(1.0) as f64;
    let h = area.height().max(1.0) as f64;
    let dx = delta_px.x as f64 * (x_max - x_min) / w;
    let dy = delta_px.y as f64 * (y_max - y_min) / h;
    (x_min - dx, x_max - dx, y_min + dy, y_max + dy)
}

/// Scale `limits` about a fixed data point `(cx, cy)` by `factor`. `factor < 1`
/// zooms in (shrinks the span); `factor > 1` zooms out. The point `(cx, cy)`
/// keeps its screen position.
pub fn zoom_about(limits: Limits, factor: f64, cx: f64, cy: f64) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    (
        cx + (x_min - cx) * factor,
        cx + (x_max - cx) * factor,
        cy + (y_min - cy) * factor,
        cy + (y_max - cy) * factor,
    )
}

/// Limits covering the data-space box defined by two corners, in any order.
pub fn box_zoom(ax: f64, ay: f64, bx: f64, by: f64) -> Limits {
    (ax.min(bx), ax.max(bx), ay.min(by), ay.max(by))
}

/// Convert an egui wheel delta (`smooth_scroll_delta.y`, pixels) to a zoom
/// factor for [`zoom_about`]. Scrolling up (`> 0`) zooms in (`factor < 1`).
pub fn wheel_zoom_factor(scroll_y: f32) -> f64 {
    // Exponential so repeated notches compose multiplicatively and symmetrically.
    (-(scroll_y as f64) * 0.0015).exp()
}

/// Whether `limits` are non-degenerate (both spans strictly positive). The
/// widget keeps the previous limits when a candidate fails this.
pub fn is_valid(limits: Limits) -> bool {
    let (x_min, x_max, y_min, y_max) = limits;
    x_max > x_min && y_max > y_min
}

/// A picked polyline vertex: its index and data coordinates, plus the pixel
/// distance from the cursor (`doc/design.md` §13 C2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointPick {
    pub index: usize,
    pub x: f64,
    pub y: f64,
    pub dist_px: f32,
}

/// Nearest polyline vertex to `cursor` (screen pixels) within `threshold_px`.
/// `points` are data coordinates, projected through `transform` to pixels for
/// the distance test. `None` if no vertex is within the threshold.
pub fn nearest_point(
    points: &[(f64, f64)],
    transform: &Transform,
    cursor: Pos2,
    threshold_px: f32,
) -> Option<PointPick> {
    let mut best: Option<PointPick> = None;
    for (index, &(x, y)) in points.iter().enumerate() {
        let dist_px = transform.data_to_pixel(x, y).distance(cursor);
        if dist_px <= threshold_px && best.is_none_or(|b| dist_px < b.dist_px) {
            best = Some(PointPick {
                index,
                x,
                y,
                dist_px,
            });
        }
    }
    best
}

/// Image pixel `(col, row)` under `cursor` (screen pixels), or `None` if the
/// cursor maps outside the image. `origin` is the data coordinate of pixel
/// `(0, 0)`'s lower-left corner and `scale` is data units per pixel (matching
/// [`crate::ImageData`]); row 0 is at the bottom.
pub fn image_index(
    transform: &Transform,
    origin: (f64, f64),
    scale: (f64, f64),
    dims: (u32, u32),
    cursor: Pos2,
) -> Option<(u32, u32)> {
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return None;
    }
    let (x, y) = transform.pixel_to_data(cursor);
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let col = ((x - origin.0) / scale.0).floor();
    let row = ((y - origin.1) / scale.1).floor();
    if col < 0.0 || row < 0.0 {
        return None;
    }
    // Saturating f64->u32 cast handles huge values; the bounds check rejects them.
    let (col, row) = (col as u32, row as u32);
    (col < dims.0 && row < dims.1).then_some((col, row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn area_100() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0))
    }

    fn close(a: Limits, b: Limits) -> bool {
        let t = 1e-9;
        (a.0 - b.0).abs() <= t
            && (a.1 - b.1).abs() <= t
            && (a.2 - b.2).abs() <= t
            && (a.3 - b.3).abs() <= t
    }

    #[test]
    fn pan_right_shifts_view_left() {
        // Drag 10px right (10% of width, span 10) -> x limits shift -1.
        let out = pan((0.0, 10.0, 0.0, 10.0), area_100(), vec2(10.0, 0.0));
        assert!(close(out, (-1.0, 9.0, 0.0, 10.0)), "{out:?}");
    }

    #[test]
    fn pan_down_increases_y_limits() {
        // Y is flipped: dragging down raises the data Y window.
        let out = pan((0.0, 10.0, 0.0, 10.0), area_100(), vec2(0.0, 10.0));
        assert!(close(out, (0.0, 10.0, 1.0, 11.0)), "{out:?}");
    }

    #[test]
    fn zoom_about_center_halves_span_keeping_center() {
        let out = zoom_about((0.0, 10.0, 0.0, 10.0), 0.5, 5.0, 5.0);
        assert!(close(out, (2.5, 7.5, 2.5, 7.5)), "{out:?}");
    }

    #[test]
    fn zoom_about_keeps_anchor_fixed() {
        // The anchor's fractional position within the limits is unchanged.
        let limits = (0.0, 10.0, 0.0, 10.0);
        let (cx, cy) = (8.0, 2.0);
        let out = zoom_about(limits, 0.3, cx, cy);
        let frac_before = (cx - limits.0) / (limits.1 - limits.0);
        let frac_after = (cx - out.0) / (out.1 - out.0);
        assert!((frac_before - frac_after).abs() <= 1e-9);
        let _ = cy;
    }

    #[test]
    fn box_zoom_orders_corners() {
        let out = box_zoom(8.0, 1.0, 2.0, 9.0);
        assert!(close(out, (2.0, 8.0, 1.0, 9.0)), "{out:?}");
    }

    #[test]
    fn wheel_factor_direction_and_neutral() {
        assert!(wheel_zoom_factor(100.0) < 1.0);
        assert!(wheel_zoom_factor(-100.0) > 1.0);
        assert!((wheel_zoom_factor(0.0) - 1.0).abs() <= 1e-12);
    }

    #[test]
    fn validity_rejects_collapsed_or_inverted() {
        assert!(is_valid((0.0, 1.0, 0.0, 1.0)));
        assert!(!is_valid((1.0, 1.0, 0.0, 1.0)));
        assert!(!is_valid((0.0, 1.0, 2.0, 1.0)));
    }

    use crate::core::transform::Transform;

    // 100×100 px area mapping data [0,10]×[0,10]; 1 data unit = 10 px.
    fn pick_transform() -> Transform {
        Transform::new(0.0, 10.0, 0.0, 10.0, area_100())
    }

    #[test]
    fn nearest_point_picks_closest_within_threshold() {
        let t = pick_transform();
        let pts = [(0.0, 0.0), (5.0, 5.0), (10.0, 10.0)];
        // (5,5) -> pixel (50, 50). Cursor a few px away picks index 1.
        let pick = nearest_point(&pts, &t, pos2(52.0, 47.0), 6.0).expect("a pick");
        assert_eq!(pick.index, 1);
        assert_eq!((pick.x, pick.y), (5.0, 5.0));
        // Nothing within threshold -> None.
        assert!(nearest_point(&pts, &t, pos2(52.0, 47.0), 2.0).is_none());
        assert!(nearest_point(&[], &t, pos2(0.0, 0.0), 100.0).is_none());
    }

    #[test]
    fn image_index_maps_cursor_to_pixel() {
        // 10×10 image, origin (0,0), unit scale, over data [0,10] in a 100px area.
        let t = pick_transform();
        // Data (0,0) is bottom-left -> pixel (0, 100). Pixel (5,95) -> data ~(0.5, 0.5)
        // -> col 0, row 0.
        assert_eq!(
            image_index(&t, (0.0, 0.0), (1.0, 1.0), (10, 10), pos2(5.0, 95.0)),
            Some((0, 0))
        );
        // Center pixel (55, 45) -> data (5.5, 5.5) -> col 5, row 5.
        assert_eq!(
            image_index(&t, (0.0, 0.0), (1.0, 1.0), (10, 10), pos2(55.0, 45.0)),
            Some((5, 5))
        );
        // Outside the data area maps outside the image.
        assert!(image_index(&t, (0.0, 0.0), (1.0, 1.0), (10, 10), pos2(-5.0, 50.0)).is_none());
    }
}
