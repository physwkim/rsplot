//! Interaction math: pure functions mapping pointer input to new data limits.
//!
//! The widget reads egui input, converts it through the *current* on-screen
//! [`Transform`](crate::core::transform::Transform), and applies one of these to
//! produce the next limits. Because everything downstream (the wgpu ortho matrix
//! and the egui chrome) derives from those limits, the image, curve, and axes
//! move together with no extra bookkeeping (`doc/design.md` §4·§8·§11.6).
//!
//! Pointer-mode mapping lives in the widget; this module is just the geometry
//! for pan/zoom/pick math, kept pure so it is unit-testable.

use egui::{Pos2, Rect, Vec2};

use crate::core::transform::{Scale, Transform};

/// Data limits `(x_min, x_max, y_min, y_max)`.
pub type Limits = (f64, f64, f64, f64);

/// Float32 safe lower bound, mirroring silx `_utils/panzoom.py`
/// `FLOAT32_SAFE_MIN`. Linear-axis limits are kept inside `[FLOAT32_SAFE_MIN,
/// FLOAT32_SAFE_MAX]` so that span subtractions (`max - min`) do not overflow
/// float32 downstream in the shaders.
pub const FLOAT32_SAFE_MIN: f64 = -1e37;
/// Float32 safe upper bound, mirroring silx `FLOAT32_SAFE_MAX`.
pub const FLOAT32_SAFE_MAX: f64 = 1e37;
/// Smallest positive normal float32 (`numpy.finfo(numpy.float32).tiny`),
/// mirroring silx `FLOAT32_MINPOS`. The lower clamp bound on a log axis (where
/// the min must stay strictly positive).
pub const FLOAT32_MINPOS: f64 = 1.1754943508222875e-38;

/// Translate a single axis range by a screen-space drag of `delta_px` pixels
/// across an axis of `extent_px` pixels, mirroring silx `Pan.drag`
/// (`PlotInteraction.py`). For a [`Scale::Log10`] axis the shift is applied in
/// log10 space; for [`Scale::Linear`] it is a plain offset.
///
/// `delta_px` is the pixel delta that should be *subtracted* from the range (the
/// data point under the pointer follows the cursor). Returns the new
/// `(min, max)`; on a log axis with a non-positive `min` or an out-of-range
/// result the original range is kept (silx reverts in those cases).
fn pan_axis(min: f64, max: f64, delta_px: f64, extent_px: f64, scale: Scale) -> (f64, f64) {
    match scale {
        Scale::Log10 if min > 0.0 && max > 0.0 => {
            let log_min = min.log10();
            let log_max = max.log10();
            // Per-pixel log10 delta across the axis (the data-to-pixel mapping is
            // linear in log space), matching silx `dx = log10(xData) - log10(lastX)`.
            let d_log = delta_px * (log_max - log_min) / extent_px;
            let new_min = 10f64.powf(log_min - d_log);
            let new_max = 10f64.powf(log_max - d_log);
            // silx keeps the axis only while both bounds stay in positive float32.
            if new_min < FLOAT32_MINPOS || new_max > FLOAT32_SAFE_MAX {
                (min, max)
            } else {
                (new_min, new_max)
            }
        }
        _ => {
            let offset = delta_px * (max - min) / extent_px;
            let new_min = min - offset;
            let new_max = max - offset;
            if new_min < FLOAT32_SAFE_MIN || new_max > FLOAT32_SAFE_MAX {
                (min, max)
            } else {
                (new_min, new_max)
            }
        }
    }
}

/// Translate `limits` by a screen-space drag delta (pixels) so the data point
/// under the pointer stays under the pointer (the content follows the cursor),
/// mirroring silx `Pan.drag` (`PlotInteraction.py`).
///
/// Screen `+x` is right and `+y` is down; the Y axis is flipped (data `y_max` at
/// the top), so a downward drag increases the data Y limits. `x_scale` /
/// `y_scale` select linear vs. log10 translation per axis.
pub fn pan(limits: Limits, area: Rect, delta_px: Vec2, x_scale: Scale, y_scale: Scale) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    let w = area.width().max(1.0) as f64;
    let h = area.height().max(1.0) as f64;
    // X: a rightward drag (+delta_px.x) shifts the view left.
    let (new_x_min, new_x_max) = pan_axis(x_min, x_max, delta_px.x as f64, w, x_scale);
    // Y is flipped: a downward drag (+delta_px.y) shifts the view up, so the
    // subtracted pixel delta is negated relative to the X convention.
    let (new_y_min, new_y_max) = pan_axis(y_min, y_max, -(delta_px.y as f64), h, y_scale);
    (new_x_min, new_x_max, new_y_min, new_y_max)
}

/// Scale a 1D range about an invariant `center` by `scale`, mirroring silx
/// `scale1DRange` (`_utils/panzoom.py`). `scale < 1` zooms out (widens the
/// span); `scale > 1` zooms in. On a log axis the operation is performed in
/// log10 space and the result is clipped to the positive float32 range; on a
/// linear axis it is clipped to the float32 range. A degenerate (`min == max`)
/// range is returned unchanged.
///
/// Note silx's `scale` is the multiplicative *zoom factor* (`range / scale`),
/// the reciprocal of the per-axis shrink ratio used by [`zoom_about`].
fn scale1d_range(min: f64, max: f64, center: f64, scale: f64, is_log: bool) -> (f64, f64) {
    let (mut min, mut center, mut max) = (min, center, max);
    if is_log {
        // Min and center can be <= 0 when autoscale is off and the axis switched
        // to log; silx substitutes FLOAT32_MINPOS in that case.
        min = if min > 0.0 {
            min.log10()
        } else {
            FLOAT32_MINPOS
        };
        center = if center > 0.0 {
            center.log10()
        } else {
            FLOAT32_MINPOS
        };
        max = if max > 0.0 {
            max.log10()
        } else {
            FLOAT32_MINPOS
        };
    }

    if min == max {
        return (min, max);
    }

    let offset = (center - min) / (max - min);
    let range = (max - min) / scale;
    let mut new_min = center - offset * range;
    let mut new_max = center + (1.0 - offset) * range;

    if is_log {
        new_min = 10f64.powf(new_min).clamp(FLOAT32_MINPOS, FLOAT32_SAFE_MAX);
        new_max = 10f64.powf(new_max).clamp(FLOAT32_MINPOS, FLOAT32_SAFE_MAX);
    } else {
        new_min = new_min.clamp(FLOAT32_SAFE_MIN, FLOAT32_SAFE_MAX);
        new_max = new_max.clamp(FLOAT32_SAFE_MIN, FLOAT32_SAFE_MAX);
    }
    (new_min, new_max)
}

/// Scale `limits` about a fixed data point `(cx, cy)`, mirroring silx
/// `applyZoomToPlot` (`_utils/panzoom.py`). `factor < 1` zooms in (shrinks the
/// span); `factor > 1` zooms out. The point `(cx, cy)` keeps its screen
/// position. `x_scale` / `y_scale` select log10 vs. linear scaling per axis.
///
/// silx `scale1DRange` divides the span by its `scale`, so to shrink the span by
/// `factor` here the silx scale is `1 / factor`.
pub fn zoom_about(
    limits: Limits,
    factor: f64,
    cx: f64,
    cy: f64,
    x_scale: Scale,
    y_scale: Scale,
) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    // silx `scale` is the reciprocal of our span-shrink `factor`.
    let silx_scale = 1.0 / factor;
    let (new_x_min, new_x_max) =
        scale1d_range(x_min, x_max, cx, silx_scale, x_scale == Scale::Log10);
    let (new_y_min, new_y_max) =
        scale1d_range(y_min, y_max, cy, silx_scale, y_scale == Scale::Log10);
    (new_x_min, new_x_max, new_y_min, new_y_max)
}

/// Pan a single axis range by `pan_factor` (a signed proportion of the range),
/// mirroring silx `applyPan` (`_utils/panzoom.py`). This is the arrow-key /
/// programmatic pan path (distinct from the mouse-drag [`pan`]). For a log axis
/// with a positive `min` the offset is applied in log10 space; otherwise it is a
/// linear offset. Out-of-range results are discarded (the original range is
/// kept), matching silx.
pub fn apply_pan(min: f64, max: f64, pan_factor: f64, is_log10: bool) -> (f64, f64) {
    if is_log10 && min > 0.0 {
        // Negative range with log scale can happen via other backends; skip it.
        let log_min = min.log10();
        let log_max = max.log10();
        let log_offset = pan_factor * (log_max - log_min);
        let new_min = 10f64.powf(log_min + log_offset);
        let new_max = 10f64.powf(log_max + log_offset);
        if new_min > 0.0 && new_max.is_finite() {
            (new_min, new_max)
        } else {
            (min, max)
        }
    } else {
        let offset = pan_factor * (max - min);
        let new_min = min + offset;
        let new_max = max + offset;
        if new_min > f64::NEG_INFINITY && new_max < f64::INFINITY {
            (new_min, new_max)
        } else {
            (min, max)
        }
    }
}

/// A pan direction for [`apply_pan`]-based arrow-key panning, mirroring silx
/// `PlotWidget.pan` directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanDirection {
    Up,
    Down,
    Left,
    Right,
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

/// Clamp one axis range into the float32-safe window and repair degenerate
/// ranges, mirroring silx `_utils/panzoom.checkAxisLimits` (panzoom.py:51-77).
///
/// Both bounds are clamped to `[lower, FLOAT32_SAFE_MAX]`, where `lower` is
/// [`FLOAT32_MINPOS`] on a log axis (`is_log == true`) and [`FLOAT32_SAFE_MIN`]
/// otherwise. If the clamp leaves `max < min` the two are swapped; if it leaves
/// `max == min` the range is expanded the way silx does:
/// - `v == 0` → `(-0.1, 0.1)`
/// - `v < 0`  → `(max(v * 1.1, FLOAT32_SAFE_MIN), v * 0.9)`
/// - `v > 0`  → `(v * 0.9, min(v * 1.1, FLOAT32_SAFE_MAX))`
///
/// A `NaN` bound clamps to `lower` (matching `numpy.clip`'s NaN→bound on the
/// platforms silx targets), so the result is always finite and ordered.
pub fn clamp_axis_limits(min: f64, max: f64, is_log: bool) -> (f64, f64) {
    let lower = if is_log {
        FLOAT32_MINPOS
    } else {
        FLOAT32_SAFE_MIN
    };
    let clip = |v: f64| -> f64 {
        // numpy.clip with a NaN input yields the NaN, but silx's downstream
        // expects a finite ordered range; map NaN to the lower bound so the
        // window is always usable.
        if v.is_nan() {
            lower
        } else {
            v.clamp(lower, FLOAT32_SAFE_MAX)
        }
    };
    let mut vmin = clip(min);
    let mut vmax = clip(max);

    if vmax < vmin {
        std::mem::swap(&mut vmin, &mut vmax);
    } else if vmax == vmin {
        let v = vmin;
        if v == 0.0 {
            vmin = -0.1;
            vmax = 0.1;
        } else if v < 0.0 {
            vmax = v * 0.9;
            vmin = (v * 1.1).max(FLOAT32_SAFE_MIN);
        } else {
            vmax = (v * 1.1).min(FLOAT32_SAFE_MAX);
            vmin = v * 0.9;
        }
    }
    (vmin, vmax)
}

/// Clamp both axes of `limits` into the float32-safe window via
/// [`clamp_axis_limits`], mirroring silx applying `checkAxisLimits` per axis
/// after pan/zoom (`PlotInteraction.py:241-250`, panzoom.py). `x_log` / `y_log`
/// select the log lower bound per axis. Applied after every pan and zoom so an
/// extreme gesture cannot push a bound past the float32-safe range.
pub fn clamp_limits(limits: Limits, x_log: bool, y_log: bool) -> Limits {
    let (x_min, x_max, y_min, y_max) = limits;
    let (nx0, nx1) = clamp_axis_limits(x_min, x_max, x_log);
    let (ny0, ny1) = clamp_axis_limits(y_min, y_max, y_log);
    (nx0, nx1, ny0, ny1)
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
        let out = pan(
            (0.0, 10.0, 0.0, 10.0),
            area_100(),
            vec2(10.0, 0.0),
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (-1.0, 9.0, 0.0, 10.0)), "{out:?}");
    }

    #[test]
    fn pan_down_increases_y_limits() {
        // Y is flipped: dragging down raises the data Y window.
        let out = pan(
            (0.0, 10.0, 0.0, 10.0),
            area_100(),
            vec2(0.0, 10.0),
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (0.0, 10.0, 1.0, 11.0)), "{out:?}");
    }

    #[test]
    fn pan_log_round_trips_in_log_space() {
        // Boundary: a +d drag then a -d drag on a log axis returns to the start.
        let limits = (1.0, 100.0, 1.0, 100.0);
        let area = area_100();
        let forward = pan(limits, area, vec2(20.0, 13.0), Scale::Log10, Scale::Log10);
        let back = pan(
            forward,
            area,
            vec2(-20.0, -13.0),
            Scale::Log10,
            Scale::Log10,
        );
        assert!(close(back, limits), "{back:?}");
        // The intermediate state must have moved (otherwise the round-trip is trivial).
        assert!(!close(forward, limits), "{forward:?}");
    }

    #[test]
    fn pan_log_translates_in_log_space() {
        // A drag of half the width on a log decade [1, 100] shifts both bounds by
        // half a log decade in log10 space (the span is 2 decades over 100px, so
        // 50px == 1 decade).
        let out = pan(
            (1.0, 100.0, 1.0, 100.0),
            area_100(),
            vec2(50.0, 0.0),
            Scale::Log10,
            Scale::Linear,
        );
        // X limits shift left by one decade: 1 -> 0.1, 100 -> 10.
        assert!((out.0 - 0.1).abs() <= 1e-9, "{out:?}");
        assert!((out.1 - 10.0).abs() <= 1e-9, "{out:?}");
        // Y (linear) unchanged.
        assert!(
            (out.2 - 1.0).abs() <= 1e-9 && (out.3 - 100.0).abs() <= 1e-9,
            "{out:?}"
        );
    }

    #[test]
    fn zoom_about_center_halves_span_keeping_center() {
        let out = zoom_about(
            (0.0, 10.0, 0.0, 10.0),
            0.5,
            5.0,
            5.0,
            Scale::Linear,
            Scale::Linear,
        );
        assert!(close(out, (2.5, 7.5, 2.5, 7.5)), "{out:?}");
    }

    #[test]
    fn zoom_about_keeps_anchor_fixed() {
        // The anchor's fractional position within the limits is unchanged.
        let limits = (0.0, 10.0, 0.0, 10.0);
        let (cx, cy) = (8.0, 2.0);
        let out = zoom_about(limits, 0.3, cx, cy, Scale::Linear, Scale::Linear);
        let frac_before = (cx - limits.0) / (limits.1 - limits.0);
        let frac_after = (cx - out.0) / (out.1 - out.0);
        assert!((frac_before - frac_after).abs() <= 1e-9);
        let _ = cy;
    }

    #[test]
    fn zoom_about_log_keeps_anchor_data_coord_fixed() {
        // Boundary: on a log axis the cursor's data coordinate must stay fixed
        // across a zoom (its fractional position in log space is invariant).
        let limits = (1.0, 1000.0, 1.0, 1000.0);
        let (cx, cy) = (10.0, 100.0);
        let out = zoom_about(limits, 0.5, cx, cy, Scale::Log10, Scale::Log10);
        let frac_log =
            |v: f64, lo: f64, hi: f64| (v.log10() - lo.log10()) / (hi.log10() - lo.log10());
        let fx_before = frac_log(cx, limits.0, limits.1);
        let fx_after = frac_log(cx, out.0, out.1);
        assert!(
            (fx_before - fx_after).abs() <= 1e-9,
            "x {fx_before} {fx_after}"
        );
        let fy_before = frac_log(cy, limits.2, limits.3);
        let fy_after = frac_log(cy, out.2, out.3);
        assert!(
            (fy_before - fy_after).abs() <= 1e-9,
            "y {fy_before} {fy_after}"
        );
    }

    #[test]
    fn apply_pan_linear_offsets_by_fraction() {
        // Linear: pan 10% of the [0, 10] span to the right.
        let (lo, hi) = apply_pan(0.0, 10.0, 0.1, false);
        assert!(
            (lo - 1.0).abs() <= 1e-12 && (hi - 11.0).abs() <= 1e-12,
            "{lo} {hi}"
        );
    }

    #[test]
    fn apply_pan_log_round_trips() {
        // Boundary: log pan +f then -f returns to the start in log space.
        let (lo, hi) = apply_pan(1.0, 100.0, 0.25, true);
        let (lo2, hi2) = apply_pan(lo, hi, -0.25, true);
        assert!(
            (lo2 - 1.0).abs() <= 1e-9 && (hi2 - 100.0).abs() <= 1e-9,
            "{lo2} {hi2}"
        );
        // Forward step moved by 0.25 decade: 1 -> 10^0.5, 100 -> 10^2.5.
        assert!((lo - 10f64.powf(0.5)).abs() <= 1e-9, "{lo}");
        assert!((hi - 10f64.powf(2.5)).abs() <= 1e-9, "{hi}");
    }

    #[test]
    fn apply_pan_log_nonpositive_min_falls_back_to_linear() {
        // Boundary: a non-positive min on a log axis takes silx's linear branch.
        let (lo, hi) = apply_pan(-1.0, 10.0, 0.1, true);
        // Linear offset: 0.1 * (10 - -1) = 1.1.
        assert!(
            (lo - 0.1).abs() <= 1e-12 && (hi - 11.1).abs() <= 1e-12,
            "{lo} {hi}"
        );
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
    fn clamp_axis_leaves_normal_range_untouched() {
        // A normal in-range linear range is returned unchanged.
        assert_eq!(clamp_axis_limits(-3.0, 5.0, false), (-3.0, 5.0));
        // A normal in-range positive log range is returned unchanged.
        assert_eq!(clamp_axis_limits(1.0, 1000.0, true), (1.0, 1000.0));
    }

    #[test]
    fn clamp_axis_clamps_beyond_safe_values() {
        // Boundary: a max beyond FLOAT32_SAFE_MAX clamps to it.
        let (lo, hi) = clamp_axis_limits(0.0, 1e40, false);
        assert_eq!((lo, hi), (0.0, FLOAT32_SAFE_MAX));
        // Boundary: a min below FLOAT32_SAFE_MIN clamps to it (linear).
        let (lo, hi) = clamp_axis_limits(-1e40, 5.0, false);
        assert_eq!((lo, hi), (FLOAT32_SAFE_MIN, 5.0));
        // Boundary: a non-positive min on a log axis clamps up to FLOAT32_MINPOS.
        let (lo, hi) = clamp_axis_limits(-10.0, 1000.0, true);
        assert_eq!((lo, hi), (FLOAT32_MINPOS, 1000.0));
    }

    #[test]
    fn clamp_axis_swaps_inverted_bounds() {
        // Boundary: max < min after clamping is swapped to ordered.
        let (lo, hi) = clamp_axis_limits(5.0, -3.0, false);
        assert_eq!((lo, hi), (-3.0, 5.0));
    }

    #[test]
    fn clamp_axis_expands_equal_bounds() {
        // v == 0 -> (-0.1, 0.1).
        assert_eq!(clamp_axis_limits(0.0, 0.0, false), (-0.1, 0.1));
        // v > 0 -> (v*0.9, v*1.1).
        let (lo, hi) = clamp_axis_limits(10.0, 10.0, false);
        assert!(
            (lo - 9.0).abs() <= 1e-12 && (hi - 11.0).abs() <= 1e-12,
            "{lo},{hi}"
        );
        // v < 0 -> (v*1.1, v*0.9).
        let (lo, hi) = clamp_axis_limits(-10.0, -10.0, false);
        assert!(
            (lo - -11.0).abs() <= 1e-12 && (hi - -9.0).abs() <= 1e-12,
            "{lo},{hi}"
        );
    }

    #[test]
    fn clamp_axis_nan_falls_to_lower_bound() {
        // Boundary: a NaN bound maps to the lower bound, keeping the range finite.
        let (lo, hi) = clamp_axis_limits(f64::NAN, 5.0, false);
        assert!(lo.is_finite() && hi.is_finite());
        assert_eq!((lo, hi), (FLOAT32_SAFE_MIN, 5.0));
        // Both NaN -> both fall to lower, then equal-expansion kicks in.
        let (lo, hi) = clamp_axis_limits(f64::NAN, f64::NAN, true);
        assert!(lo.is_finite() && hi.is_finite() && hi > lo, "{lo},{hi}");
    }

    #[test]
    fn clamp_limits_clamps_both_axes() {
        let out = clamp_limits((-1e40, 1e40, 0.0, 0.0), false, false);
        assert_eq!(out.0, FLOAT32_SAFE_MIN);
        assert_eq!(out.1, FLOAT32_SAFE_MAX);
        // Degenerate y expands.
        assert_eq!((out.2, out.3), (-0.1, 0.1));
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
