//! The data↔screen coordinate transform — the single source of truth.
//!
//! Both consumers derive their mapping from one [`Transform`] built from the
//! same (limits, area): the wgpu shader gets an orthographic transformed→NDC
//! matrix ([`Transform::ortho_matrix`]), and the egui chrome gets
//! [`Transform::data_to_pixel`] / [`Transform::pixel_to_data`] (`doc/design.md`
//! §4). Computing the mapping in two places is what makes the image and the
//! axes drift apart by a pixel, so it lives here once.
//!
//! Each axis is an [`Axis`] with a [`Scale`] (linear or log10) and an
//! `inverted` flag. Everything funnels through one normalized coordinate
//! `t ∈ [0, 1]` ([`Axis::norm`] / [`Axis::denorm`]), so linear, inverted, and
//! log axes share a single code path (`doc/design.md` §13 Wave A).
//!
//! Scope note: [`Transform::ortho_matrix`] is affine, so it expresses linear and
//! inverted axes exactly. For a log axis the GPU producers upload `log10`-mapped
//! coordinates and the matrix maps the log-space limits linearly; the pixel
//! mapping ([`Axis::norm`]) is exact for all scales (`doc/design.md` §12·§13 A3).

use egui::{Pos2, Rect, emath::RectTransform, pos2};

/// Per-axis scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    /// `t = (v - min) / (max - min)`.
    Linear,
    /// `t = (log10 v - log10 min) / (log10 max - log10 min)`; requires `min > 0`.
    Log10,
}

/// Which Y axis a curve is plotted against (silx `YAxis`): the main left axis,
/// the secondary right axis, or one of the additional stacked axes
/// (`doc/design.md` §13 A5 + multi-axis extension).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum YAxis {
    /// The main left Y axis (`Plot::limits`).
    #[default]
    Left,
    /// The secondary right Y axis (`Plot::y2`).
    Right,
    /// One of the additional Y axes beyond the built-in left/right pair, indexed
    /// into `Plot::extra`. Each carries its own range, scale, side, and label;
    /// curves bound here plot against `Plot::extra[index]` and its ticks/label
    /// are drawn stacked outward in the configured [`AxisSide`] gutter. A curve
    /// bound to an unknown index (or an axis with no range yet) falls back to the
    /// left transform, mirroring the right axis' `y2 == None` fallback.
    Extra(usize),
}

/// Which gutter a Y axis' ticks and label are drawn in. The built-in axes are
/// fixed (`Left` for [`YAxis::Left`], the right gutter for [`YAxis::Right`]);
/// each [`YAxis::Extra`] axis chooses its side, and same-side extra axes stack
/// outward in creation order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisSide {
    /// Ticks and label in the left gutter.
    Left,
    /// Ticks and label in the right gutter (the default secondary placement).
    #[default]
    Right,
}

/// One axis: a data range, a scale, and a direction flag.
///
/// Preconditions: `max > min`; for [`Scale::Log10`], also `min > 0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub min: f64,
    pub max: f64,
    pub scale: Scale,
    /// When true, the normalized coordinate is flipped (`t → 1 - t`), reversing
    /// the on-screen direction of the axis.
    pub inverted: bool,
}

impl Axis {
    /// A linear, non-inverted axis over `[min, max]`.
    pub fn linear(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            scale: Scale::Linear,
            inverted: false,
        }
    }

    /// Map a data value to its normalized coordinate `t ∈ [0, 1]` (0 at the low
    /// screen edge, 1 at the high edge), applying scale and inversion.
    pub fn norm(&self, v: f64) -> f64 {
        let t = match self.scale {
            Scale::Linear => (v - self.min) / (self.max - self.min),
            Scale::Log10 => (v.log10() - self.min.log10()) / (self.max.log10() - self.min.log10()),
        };
        if self.inverted { 1.0 - t } else { t }
    }

    /// Inverse of [`Axis::norm`]: map a normalized coordinate back to data.
    pub fn denorm(&self, t: f64) -> f64 {
        let t = if self.inverted { 1.0 - t } else { t };
        match self.scale {
            Scale::Linear => self.min + t * (self.max - self.min),
            Scale::Log10 => {
                let lmin = self.min.log10();
                let lmax = self.max.log10();
                10f64.powf(lmin + t * (lmax - lmin))
            }
        }
    }

    /// The axis range in transformed (post-scale) space, as `(value at t = 0,
    /// value at t = 1)`. Inversion swaps the endpoints. This is the affine
    /// coordinate the orthographic matrix maps to NDC.
    fn effective_range(&self) -> (f64, f64) {
        let (lo, hi) = match self.scale {
            Scale::Linear => (self.min, self.max),
            Scale::Log10 => (self.min.log10(), self.max.log10()),
        };
        if self.inverted { (hi, lo) } else { (lo, hi) }
    }
}

/// Expand limits so both axes share the same data-units-per-pixel, keeping
/// data square on screen (silx `setKeepDataAspectRatio`). The axis that is too
/// tight is widened symmetrically about its center; nothing is ever shrunk, so
/// all originally-visible data stays visible. Linear-only — the caller must not
/// apply this when either axis is log10. A no-op for a degenerate `area` or
/// span.
///
/// This is a pure function of `(limits, area)`: the widget derives the display
/// view from the stable requested limits every frame, so resizing never
/// compounds the expansion (`doc/design.md` §13 A4).
pub fn keep_aspect_limits(
    (x_min, x_max, y_min, y_max): (f64, f64, f64, f64),
    area: Rect,
) -> (f64, f64, f64, f64) {
    let (w, h) = (area.width() as f64, area.height() as f64);
    let (x_span, y_span) = (x_max - x_min, y_max - y_min);
    if w <= 0.0 || h <= 0.0 || x_span <= 0.0 || y_span <= 0.0 {
        return (x_min, x_max, y_min, y_max);
    }
    // Data units per pixel on each axis; equalize to the larger (zoom out the
    // tighter axis) so the common scale shows all data.
    let scale_x = x_span / w;
    let scale_y = y_span / h;
    if scale_x > scale_y {
        let new_span = scale_x * h;
        let cy = 0.5 * (y_min + y_max);
        (x_min, x_max, cy - new_span / 2.0, cy + new_span / 2.0)
    } else {
        let new_span = scale_y * w;
        let cx = 0.5 * (x_min + x_max);
        (cx - new_span / 2.0, cx + new_span / 2.0, y_min, y_max)
    }
}

/// Plot margins as fractions of the full widget rect, matching silx
/// `setAxesMargins`. Insetting the full widget rect by these yields the data
/// area that a [`Transform`] maps into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Margins {
    /// No margins: the data area is the whole widget rect.
    pub const ZERO: Margins = Margins {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    /// Inset `full` by these fractions to get the data area.
    pub fn data_area(&self, full: Rect) -> Rect {
        let w = full.width();
        let h = full.height();
        Rect::from_min_max(
            pos2(full.left() + self.left * w, full.top() + self.top * h),
            pos2(
                full.right() - self.right * w,
                full.bottom() - self.bottom * h,
            ),
        )
    }
}

/// Linear mapping between data space and the data area's screen pixels.
///
/// Preconditions: each axis is non-degenerate (`max > min`; `min > 0` for log).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub x: Axis,
    pub y: Axis,
    /// Data-area rectangle in egui points (screen space).
    pub area: Rect,
}

impl Transform {
    /// Build a linear, non-inverted transform mapping the given limits into
    /// `area` (back-compatible constructor).
    pub fn new(x_min: f64, x_max: f64, y_min: f64, y_max: f64, area: Rect) -> Self {
        Self {
            x: Axis::linear(x_min, x_max),
            y: Axis::linear(y_min, y_max),
            area,
        }
    }

    /// Build a transform from explicit axes.
    pub fn with_axes(x: Axis, y: Axis, area: Rect) -> Self {
        Self { x, y, area }
    }

    /// Map data coordinates to screen pixels (egui points). The Y axis points up
    /// in data space, so its low value sits at the bottom of the area.
    pub fn data_to_pixel(&self, x: f64, y: f64) -> Pos2 {
        let (px, py) = self.data_to_pixel_f64(x, y);
        pos2(px as f32, py as f32)
    }

    /// Map screen pixels (egui points) back to data coordinates.
    pub fn pixel_to_data(&self, p: Pos2) -> (f64, f64) {
        self.pixel_to_data_f64(p.x as f64, p.y as f64)
    }

    fn data_to_pixel_f64(&self, x: f64, y: f64) -> (f64, f64) {
        let (left, right) = (self.area.left() as f64, self.area.right() as f64);
        let (top, bottom) = (self.area.top() as f64, self.area.bottom() as f64);
        let tx = self.x.norm(x);
        let ty = self.y.norm(y);
        let px = left + tx * (right - left);
        // ty = 0 (low) -> bottom, ty = 1 (high) -> top.
        let py = bottom + ty * (top - bottom);
        (px, py)
    }

    fn pixel_to_data_f64(&self, px: f64, py: f64) -> (f64, f64) {
        let (left, right) = (self.area.left() as f64, self.area.right() as f64);
        let (top, bottom) = (self.area.top() as f64, self.area.bottom() as f64);
        let tx = (px - left) / (right - left);
        let ty = (py - bottom) / (top - bottom);
        (self.x.denorm(tx), self.y.denorm(ty))
    }

    /// data→pixel [`RectTransform`] for the linear, non-inverted case (a
    /// convenience for affine chrome work). Not meaningful for log/inverted
    /// axes; use [`Transform::data_to_pixel`] there.
    pub fn rect_transform(&self) -> RectTransform {
        let from = Rect::from_min_max(
            pos2(self.x.min as f32, self.y.max as f32),
            pos2(self.x.max as f32, self.y.min as f32),
        );
        RectTransform::from_to(from, self.area)
    }

    /// Column-major transformed→NDC orthographic matrix for the wgpu shader.
    /// Maps each axis's `Axis::effective_range` to NDC `[-1, 1]` at `z = 0`,
    /// with the high edge of Y at `+1` (top) to match egui-wgpu's viewport.
    /// Affine, so it expresses linear and inverted axes exactly; for a log axis
    /// the producers must upload `log10`-mapped coordinates (`doc/design.md`
    /// §4·§13 A3). Equivalent to pygfx's `OrthographicCamera::show_rect`.
    pub fn ortho_matrix(&self) -> [[f32; 4]; 4] {
        let (ex0, ex1) = self.x.effective_range();
        let (ey0, ey1) = self.y.effective_range();
        let sx = (2.0 / (ex1 - ex0)) as f32;
        let sy = (2.0 / (ey1 - ey0)) as f32;
        let tx = (-(ex1 + ex0) / (ex1 - ex0)) as f32;
        let ty = (-(ey1 + ey0) / (ey1 - ey0)) as f32;
        [
            [sx, 0.0, 0.0, 0.0],
            [0.0, sy, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [tx, ty, 0.0, 1.0],
        ]
    }
}

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

/// Clamp one axis range into the float32-safe window and repair degenerate
/// ranges, mirroring silx `_utils/panzoom.checkAxisLimits` (panzoom.py:49-75).
/// This is the single repair every limits adoption funnels through — gesture
/// commits and reset-zoom refits alike — matching silx `setLimits`, whose
/// first step is per-axis `_checkLimits` (`PlotWidget.py:2705-2712`).
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

#[cfg(test)]
mod tests {
    use super::*;

    // A deliberately asymmetric, off-origin setup to catch sign/offset bugs.
    fn sample() -> Transform {
        let area = Rect::from_min_max(pos2(100.0, 20.0), pos2(420.0, 260.0));
        Transform::new(-3.0, 5.0, 10.0, 50.0, area)
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn corners_map_with_y_flip() {
        let t = sample();
        // (x_min, y_min) -> (left, bottom)
        let (px, py) = t.data_to_pixel_f64(-3.0, 10.0);
        assert!(
            close(px, 100.0, 1e-9) && close(py, 260.0, 1e-9),
            "{px},{py}"
        );
        // (x_max, y_max) -> (right, top)
        let (px, py) = t.data_to_pixel_f64(5.0, 50.0);
        assert!(close(px, 420.0, 1e-9) && close(py, 20.0, 1e-9), "{px},{py}");
        // center -> area center
        let (px, py) = t.data_to_pixel_f64(1.0, 30.0);
        assert!(
            close(px, 260.0, 1e-9) && close(py, 140.0, 1e-9),
            "{px},{py}"
        );
    }

    #[test]
    fn round_trip_is_identity_f64() {
        let t = sample();
        for &(x, y) in &[
            (-3.0, 10.0),
            (5.0, 50.0),
            (1.0, 30.0),
            (-1.25, 42.0),
            (4.9, 11.1),
        ] {
            let (px, py) = t.data_to_pixel_f64(x, y);
            let (rx, ry) = t.pixel_to_data_f64(px, py);
            assert!(
                close(rx, x, 1e-9) && close(ry, y, 1e-9),
                "{x},{y} -> {rx},{ry}"
            );
        }
    }

    #[test]
    fn round_trip_through_pos2_is_close() {
        // f32 Pos2 loses precision; tolerance is small relative to the data span.
        let t = sample();
        for &(x, y) in &[(-1.25, 42.0), (4.9, 11.1), (0.0, 25.0)] {
            let (rx, ry) = t.pixel_to_data(t.data_to_pixel(x, y));
            assert!(
                close(rx, x, 1e-3) && close(ry, y, 1e-3),
                "{x},{y} -> {rx},{ry}"
            );
        }
    }

    #[test]
    fn rect_transform_agrees_with_data_to_pixel() {
        let t = sample();
        let rt = t.rect_transform();
        for &(x, y) in &[(-3.0, 10.0), (5.0, 50.0), (1.0, 30.0)] {
            let a = t.data_to_pixel(x, y);
            let b = rt.transform_pos(pos2(x as f32, y as f32));
            assert!(
                (a.x - b.x).abs() <= 1e-3 && (a.y - b.y).abs() <= 1e-3,
                "{a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn ortho_matrix_maps_limits_to_ndc() {
        let t = sample();
        let m = t.ortho_matrix();
        // Apply column-major matrix to (x, y, 0, 1): clip = m * v.
        let apply = |x: f32, y: f32| -> (f32, f32) {
            let cx = m[0][0] * x + m[1][0] * y + m[3][0];
            let cy = m[0][1] * x + m[1][1] * y + m[3][1];
            (cx, cy)
        };
        let approx = |a: f32, b: f32| (a - b).abs() <= 1e-5;
        let (cx, cy) = apply(-3.0, 10.0);
        assert!(approx(cx, -1.0) && approx(cy, -1.0), "{cx},{cy}");
        let (cx, cy) = apply(5.0, 50.0);
        assert!(approx(cx, 1.0) && approx(cy, 1.0), "{cx},{cy}");
        let (cx, cy) = apply(1.0, 30.0);
        assert!(approx(cx, 0.0) && approx(cy, 0.0), "{cx},{cy}");
    }

    #[test]
    fn margins_inset_full_rect() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0));
        let m = Margins {
            left: 0.1,
            top: 0.2,
            right: 0.05,
            bottom: 0.0,
        };
        let area = m.data_area(full);
        assert_eq!(area.left(), 20.0);
        assert_eq!(area.top(), 20.0);
        assert_eq!(area.right(), 190.0);
        assert_eq!(area.bottom(), 100.0);
    }

    #[test]
    fn axis_norm_denorm_round_trip_all_scales() {
        let axes = [
            Axis::linear(-3.0, 5.0),
            Axis {
                min: -3.0,
                max: 5.0,
                scale: Scale::Linear,
                inverted: true,
            },
            Axis {
                min: 1.0,
                max: 1000.0,
                scale: Scale::Log10,
                inverted: false,
            },
            Axis {
                min: 1.0,
                max: 1000.0,
                scale: Scale::Log10,
                inverted: true,
            },
        ];
        for axis in axes {
            for &v in &[axis.min, axis.max, axis.denorm(0.5), axis.denorm(0.27)] {
                let t = axis.norm(v);
                let back = axis.denorm(t);
                assert!((back - v).abs() <= 1e-9 * v.abs().max(1.0), "{axis:?}: {v}");
            }
        }
    }

    #[test]
    fn inverted_axis_flips_endpoints() {
        let a = Axis::linear(0.0, 10.0);
        let b = Axis {
            inverted: true,
            ..a
        };
        assert!(close(a.norm(0.0), 0.0, 1e-12) && close(a.norm(10.0), 1.0, 1e-12));
        assert!(close(b.norm(0.0), 1.0, 1e-12) && close(b.norm(10.0), 0.0, 1e-12));
    }

    #[test]
    fn log_axis_maps_decades_evenly() {
        let a = Axis {
            min: 1.0,
            max: 1000.0,
            scale: Scale::Log10,
            inverted: false,
        };
        // Three decades -> 10 at 1/3, 100 at 2/3.
        assert!(close(a.norm(1.0), 0.0, 1e-12));
        assert!(close(a.norm(10.0), 1.0 / 3.0, 1e-12));
        assert!(close(a.norm(100.0), 2.0 / 3.0, 1e-12));
        assert!(close(a.norm(1000.0), 1.0, 1e-12));
    }

    #[test]
    fn keep_aspect_equalizes_units_per_pixel() {
        // Wide area (2:1), square data limits: x must widen so both axes share
        // the same data-units-per-pixel; the y range is untouched.
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0));
        let (x0, x1, y0, y1) = keep_aspect_limits((0.0, 10.0, 0.0, 10.0), area);
        // y unchanged.
        assert!(close(y0, 0.0, 1e-12) && close(y1, 10.0, 1e-12));
        // x widened to 20 units (0.1 units/px * 200 px), centered on 5.
        assert!(close(x0, -5.0, 1e-9) && close(x1, 15.0, 1e-9), "{x0},{x1}");
        // Units-per-pixel now equal on both axes.
        let sx = (x1 - x0) / area.width() as f64;
        let sy = (y1 - y0) / area.height() as f64;
        assert!(close(sx, sy, 1e-9), "{sx} vs {sy}");
    }

    #[test]
    fn keep_aspect_is_idempotent_for_fixed_area() {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(150.0, 300.0));
        let once = keep_aspect_limits((0.0, 4.0, -2.0, 2.0), area);
        let twice = keep_aspect_limits(once, area);
        assert!(
            close(once.0, twice.0, 1e-9)
                && close(once.1, twice.1, 1e-9)
                && close(once.2, twice.2, 1e-9)
                && close(once.3, twice.3, 1e-9),
            "{once:?} vs {twice:?}"
        );
    }

    #[test]
    fn keep_aspect_noop_on_degenerate_inputs() {
        let zero = Rect::from_min_max(pos2(0.0, 0.0), pos2(0.0, 0.0));
        assert_eq!(
            keep_aspect_limits((0.0, 1.0, 0.0, 1.0), zero),
            (0.0, 1.0, 0.0, 1.0)
        );
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert_eq!(
            keep_aspect_limits((5.0, 5.0, 0.0, 1.0), area),
            (5.0, 5.0, 0.0, 1.0)
        );
    }

    #[test]
    fn inverted_ortho_flips_ndc() {
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        let t = Transform::with_axes(
            Axis {
                inverted: true,
                ..Axis::linear(0.0, 10.0)
            },
            Axis::linear(0.0, 10.0),
            area,
        );
        let m = t.ortho_matrix();
        let ndc_x = |x: f32| m[0][0] * x + m[3][0];
        // x_min now maps to +1 and x_max to -1 (flipped).
        assert!((ndc_x(0.0) - 1.0).abs() <= 1e-5);
        assert!((ndc_x(10.0) + 1.0).abs() <= 1e-5);
    }
}
