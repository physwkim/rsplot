//! Interactive histogram colorbar (pyqtgraph `HistogramLUTItem`-style).
//!
//! [`HistogramColorBar`] pairs a vertical colormap gradient with the active
//! image's value-distribution histogram and two draggable handles marking the
//! colormap's `vmin`/`vmax` levels. Dragging a handle — or picking "Auto range"
//! from the right-click context menu (pyqtgraph `autoLevel`: the data extremes)
//! — returns the new levels via
//! [`HistogramColorBarResponse::dragged_levels`]; the owner (e.g. `ImageView`)
//! applies them to the colormap — the same single-owner drag pattern as
//! [`crate::widget::radar_view::RadarView`].
//!
//! This is an intentional pyqtgraph-style addition, not a silx widget: silx
//! adjusts levels through a separate `ColormapDialog`. The bar's value axis spans
//! the **data range** (padded for drag headroom), so the handles have room to
//! move within it — unlike the static [`crate::widget::colorbar::ColorBarWidget`]
//! whose axis is exactly `[vmin, vmax]`. The two never share an axis meaning, so
//! they stay distinct widgets.
//!
//! All value↔pixel mapping, hit-testing, and level clamping is factored into
//! pure free functions that are unit-tested without a GPU/device; the `ui`
//! rendering itself needs an [`egui::Painter`] only (no wgpu).

use egui::{Color32, FontId, Rect, Sense, Shape, Stroke, Vec2, pos2};

use crate::core::colormap::{Colormap, Normalization};
use crate::widget::colorbar::format_end_label;

/// Which level handle a hit-test / drag refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// The low level (`vmin`), drawn at the bottom.
    Min,
    /// The high level (`vmax`), drawn at the top.
    Max,
}

/// Fraction of the (transformed) data span added as headroom on each end of the
/// axis, so the handles can be dragged slightly past the data min/max — the
/// padding is fixed (data-derived), so it does not jitter while dragging.
const AXIS_PAD_FRAC: f64 = 0.05;
/// Gradient-strip thickness in points (matches silx `_ColorScale`, 25 px).
const BAR_THICKNESS: f32 = 25.0;
/// Minimum cross-axis room reserved on the right for value labels, in points.
/// The actual reserve is measured from the formatted labels each frame (so they
/// never clip); this is only the floor when the labels are empty/tiny.
const MIN_LABEL_WIDTH: f32 = 32.0;
/// Gap between widget sub-areas / label gap, in points.
const GAP: f32 = 4.0;
/// Half-size of the triangle handle marker, in points.
const HANDLE_TRI: f32 = 5.0;
/// Half-height of a handle's draggable grab band, in points.
const HANDLE_GRAB_HALF: f32 = 5.0;
/// Tick/label font size, in points.
const FONT_SIZE: f32 = 11.0;
/// Vertical inset so edge handles/labels are not clipped at the rect edges.
const V_INSET: f32 = 6.0;

// ── Pure mapping / hit-test / clamp helpers ─────────────────────────────────

/// The bar's value axis `(lo, hi)` for a `data_range` under `norm`: the data
/// range, sanitized (swapped if reversed, expanded if degenerate, floored
/// positive for log) and padded by `AXIS_PAD_FRAC` of the transformed span on
/// each end for drag headroom.
pub fn axis_range(data_range: (f64, f64), norm: Normalization) -> (f64, f64) {
    let (mut lo, mut hi) = data_range;
    if hi < lo {
        std::mem::swap(&mut lo, &mut hi);
    }
    // Log requires strictly-positive bounds before transforming.
    if norm == Normalization::Log {
        if hi <= 0.0 || !hi.is_finite() {
            return (1.0, 10.0);
        }
        if lo <= 0.0 || !lo.is_finite() {
            lo = hi * 1e-6;
        }
    }
    let tl = norm.transform(lo);
    let th = norm.transform(hi);
    if !tl.is_finite() || !th.is_finite() {
        return (lo, if hi > lo { hi } else { lo + 1.0 });
    }
    let span = th - tl;
    let pad = if span > 0.0 {
        span * AXIS_PAD_FRAC
    } else {
        // Degenerate (all-equal) data: open a small symmetric window.
        if tl.abs() > 0.0 { tl.abs() * 0.5 } else { 0.5 }
    };
    (
        norm.inverse_transform(tl - pad),
        norm.inverse_transform(th + pad),
    )
}

/// Map a data value to its `[0, 1]` fraction along the axis `[lo, hi]` under
/// `norm` (0 at `lo`/bottom, 1 at `hi`/top), clamped. Returns 0 for a degenerate
/// axis. Non-finite transforms (e.g. log of a non-positive value) clamp to the
/// near end.
pub fn value_to_frac(v: f64, lo: f64, hi: f64, norm: Normalization) -> f64 {
    let tl = norm.transform(lo);
    let th = norm.transform(hi);
    if !tl.is_finite() || !th.is_finite() || th <= tl {
        return 0.0;
    }
    let tv = norm.transform(v);
    ((tv - tl) / (th - tl)).clamp(0.0, 1.0)
}

/// Inverse of [`value_to_frac`]: map a `[0, 1]` fraction back to a data value on
/// the axis `[lo, hi]` under `norm`. Returns `lo` for a degenerate axis.
pub fn frac_to_value(frac: f64, lo: f64, hi: f64, norm: Normalization) -> f64 {
    let tl = norm.transform(lo);
    let th = norm.transform(hi);
    if !tl.is_finite() || !th.is_finite() || th <= tl {
        return lo;
    }
    let t = tl + frac.clamp(0.0, 1.0) * (th - tl);
    norm.inverse_transform(t)
}

/// Which handle (if any) a pointer at `pointer_frac` grabs, given the handle
/// fractions and a tolerance `tol` (all in `[0, 1]` axis fraction). When both
/// are within tolerance the nearer one wins.
pub fn hit_handle(pointer_frac: f64, vmin_frac: f64, vmax_frac: f64, tol: f64) -> Option<Handle> {
    let dmin = (pointer_frac - vmin_frac).abs();
    let dmax = (pointer_frac - vmax_frac).abs();
    match (dmin <= tol, dmax <= tol) {
        (true, true) => Some(if dmin <= dmax {
            Handle::Min
        } else {
            Handle::Max
        }),
        (true, false) => Some(Handle::Min),
        (false, true) => Some(Handle::Max),
        (false, false) => None,
    }
}

/// The level pair the context menu's "Auto range" resets to (pyqtgraph
/// `HistogramLUTItem.imageChanged(autoLevel=True)`: the data extremes): the
/// data range, swapped if reversed; `None` when degenerate or non-finite
/// (there is no sensible pair to set).
pub fn auto_range_levels(data_range: (f64, f64)) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = data_range;
    if hi < lo {
        std::mem::swap(&mut lo, &mut hi);
    }
    (lo.is_finite() && hi.is_finite() && hi > lo).then_some((lo, hi))
}

/// Apply a dragged `handle` to `value` and return the new `(vmin, vmax)` pair.
/// The dragged handle is clamped into `[lo, hi]` and may not cross the other
/// handle: `vmin + min_sep <= vmax` always holds, and the non-dragged level is
/// preserved. `min_sep` is a value-space minimum separation.
pub fn apply_handle_drag(
    handle: Handle,
    value: f64,
    vmin: f64,
    vmax: f64,
    lo: f64,
    hi: f64,
    min_sep: f64,
) -> (f64, f64) {
    match handle {
        Handle::Min => {
            // Upper bound never drops below `lo`, so `clamp` sees lo <= upper.
            let upper = (vmax - min_sep).max(lo);
            (value.clamp(lo, upper), vmax)
        }
        Handle::Max => {
            let lower = (vmin + min_sep).min(hi);
            (vmin, value.clamp(lower, hi))
        }
    }
}

// ── Widget ──────────────────────────────────────────────────────────────────

/// An interactive histogram colorbar. Built fresh each frame by its owner from
/// the active colormap, the image's value range, the value-distribution
/// histogram, and the current `vmin`/`vmax`.
#[derive(Clone, Debug)]
pub struct HistogramColorBar {
    /// The colormap whose gradient and LUT are shown.
    pub colormap: Colormap,
    /// The image's value range `(min, max)`; the axis is derived from this via
    /// [`axis_range`].
    pub data_range: (f64, f64),
    /// `(counts, edges)` from [`crate::core::histogram::compute_histogram`], or
    /// `None` to draw the gradient + handles without a histogram.
    pub histogram: Option<(Vec<u64>, Vec<f64>)>,
    /// Current low level (drawn as the bottom handle).
    pub vmin: f64,
    /// Current high level (drawn as the top handle).
    pub vmax: f64,
    /// Absolute screen `(top, bottom)` the gradient strip + histogram + handles
    /// should span, so the bar aligns with an external reference (the owning
    /// image's data-area guides). `None` falls back to the allocated box inset by
    /// `V_INSET`. Set by the owner from the image plot's data-area rect.
    pub bar_bounds: Option<(f32, f32)>,
}

/// The result of [`HistogramColorBar::ui`]: the allocated [`egui::Response`] plus
/// the new `(vmin, vmax)` levels when a handle was dragged (or the context
/// menu's "Auto range" was chosen) this frame.
pub struct HistogramColorBarResponse {
    /// The egui response of the allocated widget area.
    pub response: egui::Response,
    /// The new `(vmin, vmax)` when the levels changed this frame — a handle
    /// drag, or "Auto range" from the right-click menu (the data extremes) —
    /// ready to apply to the colormap; `None` when nothing changed.
    pub dragged_levels: Option<(f64, f64)>,
}

impl HistogramColorBar {
    /// A histogram colorbar for `colormap`, taking the levels from the colormap's
    /// `vmin`/`vmax` and the data range likewise (override with the builders).
    pub fn new(colormap: Colormap) -> Self {
        let (vmin, vmax) = (colormap.vmin, colormap.vmax);
        Self {
            colormap,
            data_range: (vmin, vmax),
            histogram: None,
            vmin,
            vmax,
            bar_bounds: None,
        }
    }

    /// Set the image's value range (builder form).
    pub fn with_data_range(mut self, range: (f64, f64)) -> Self {
        self.data_range = range;
        self
    }

    /// Set the value-distribution histogram (builder form).
    pub fn with_histogram(mut self, histogram: Option<(Vec<u64>, Vec<f64>)>) -> Self {
        self.histogram = histogram;
        self
    }

    /// Set the current levels (builder form).
    pub fn with_levels(mut self, vmin: f64, vmax: f64) -> Self {
        self.vmin = vmin;
        self.vmax = vmax;
        self
    }

    /// Pin the gradient strip (and histogram/handles) to absolute screen
    /// `(top, bottom)`, so the bar lines up with the owning image's data-area
    /// guides instead of filling its allocated box (builder form).
    pub fn with_bar_bounds(mut self, top: f32, bottom: f32) -> Self {
        self.bar_bounds = Some((top, bottom));
        self
    }

    /// Paint the histogram + gradient + draggable handles into a `desired`-sized
    /// region of `ui` and report any handle drag this frame. The column senses
    /// clicks so a right-click opens the Auto-range context menu.
    pub fn ui(&self, ui: &mut egui::Ui, desired: Vec2) -> HistogramColorBarResponse {
        let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
        self.show_in(ui, rect, response)
    }

    /// Render at an explicit `rect` instead of allocating from the layout cursor,
    /// for embedding in a fixed gutter — e.g. a plot's colorbar region in
    /// [`crate::widget::plot_widget::PlotView`]. Same interaction and paint as
    /// [`Self::ui`]; pair with [`Self::with_bar_bounds`] when the gutter is taller
    /// than the data area it should track.
    pub fn ui_at(&self, ui: &egui::Ui, rect: Rect) -> HistogramColorBarResponse {
        let response = ui.interact(rect, ui.id().with("histogram_colorbar"), Sense::click());
        self.show_in(ui, rect, response)
    }

    fn show_in(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        response: egui::Response,
    ) -> HistogramColorBarResponse {
        let norm = self.colormap.normalization;
        let (lo, hi) = axis_range(self.data_range, norm);

        // Vertical span of the strip: the caller-pinned bounds (the image's
        // data-area guides) when given, else the allocated box inset by V_INSET.
        // Independent of the (horizontal) label gutter, so resolved first.
        let (bar_top, bar_bottom) = match self.bar_bounds {
            Some((t, b)) => (t.max(rect.top()), b.min(rect.bottom())),
            None => (rect.top() + V_INSET, rect.bottom() - V_INSET),
        };
        let bar_height = (bar_bottom - bar_top).max(1.0);

        let vmin_frac = value_to_frac(self.vmin, lo, hi, norm);
        let vmax_frac = value_to_frac(self.vmax, lo, hi, norm);
        let y_of_frac = |f: f64| bar_bottom - (f as f32) * bar_height;

        // Per-handle drag (stable ids let egui track the grabbed handle across
        // frames; the widget itself stays stateless, like ColorBarWidget). The
        // grab band spans the full column width at the handle's y; this is also
        // why the drag is resolved before the (horizontal) label gutter below.
        let grab = |y: f32| {
            Rect::from_min_max(
                pos2(rect.left(), y - HANDLE_GRAB_HALF),
                pos2(rect.right(), y + HANDLE_GRAB_HALF),
            )
        };
        let min_resp = ui.interact(
            grab(y_of_frac(vmin_frac)),
            response.id.with("hcb_min"),
            Sense::drag(),
        );
        let max_resp = ui.interact(
            grab(y_of_frac(vmax_frac)),
            response.id.with("hcb_max"),
            Sense::drag(),
        );

        let min_sep = ((hi - lo).abs() * 0.005).max(f64::MIN_POSITIVE);
        let mut dragged_levels = None;
        let frac_at = |y: f32| ((bar_bottom - y) / bar_height).clamp(0.0, 1.0) as f64;
        if min_resp.dragged()
            && let Some(p) = min_resp.interact_pointer_pos()
        {
            let v = frac_to_value(frac_at(p.y), lo, hi, norm);
            dragged_levels = Some(apply_handle_drag(
                Handle::Min,
                v,
                self.vmin,
                self.vmax,
                lo,
                hi,
                min_sep,
            ));
        } else if max_resp.dragged()
            && let Some(p) = max_resp.interact_pointer_pos()
        {
            let v = frac_to_value(frac_at(p.y), lo, hi, norm);
            dragged_levels = Some(apply_handle_drag(
                Handle::Max,
                v,
                self.vmin,
                self.vmax,
                lo,
                hi,
                min_sep,
            ));
        }
        if min_resp.hovered() || max_resp.hovered() || min_resp.dragged() || max_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }

        // Right-click context menu (pyqtgraph `HistogramLUTItem` autoLevel):
        // "Auto range" resets the levels to the data extremes. The choice flows
        // through the same `dragged_levels` channel as a handle drag, so every
        // level change reaches the owner through one apply path.
        response.context_menu(|ui| {
            if ui.button("Auto range").clicked() {
                if let Some(levels) = auto_range_levels(self.data_range) {
                    dragged_levels = Some(levels);
                }
                ui.close();
            }
        });

        // Paint at the live (post-drag) levels so there is no one-frame lag.
        let (draw_vmin, draw_vmax) = dragged_levels.unwrap_or((self.vmin, self.vmax));

        // Reserve the label gutter from the *measured* width of the labels that
        // will actually be drawn (the post-drag values). This only places the
        // gradient strip so the labels normally sit clear of it — the no-clip
        // guarantee does NOT depend on it: labels are right-anchored at the
        // column's right edge (see `paint_handle`), so an unexpectedly wide
        // label overlaps leftward instead of clipping. A representative
        // scientific label is a floor so the strip is stable (no horizontal
        // jiggle) across drags.
        let font = FontId::proportional(FONT_SIZE);
        let measure = |s: String| {
            ui.painter()
                .layout_no_wrap(s, font.clone(), Color32::WHITE)
                .size()
                .x
        };
        let label_reserve = measure("-8.88e-88".to_owned())
            .max(measure(format_end_label(draw_vmin)))
            .max(measure(format_end_label(draw_vmax)))
            .max(MIN_LABEL_WIDTH)
            + GAP;

        // Layout: [ histogram | gradient strip | value labels ].
        let bar_left = (rect.right() - label_reserve - GAP - BAR_THICKNESS).max(rect.left());
        let bar_rect = Rect::from_min_max(
            pos2(bar_left, bar_top),
            pos2(bar_left + BAR_THICKNESS, bar_bottom),
        );
        let hist_rect = Rect::from_min_max(pos2(rect.left(), bar_top), pos2(bar_left, bar_bottom));

        if ui.is_rect_visible(rect) {
            self.paint(ui, rect, bar_rect, hist_rect, lo, hi, draw_vmin, draw_vmax);
        }

        HistogramColorBarResponse {
            response,
            dragged_levels,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        bar_rect: Rect,
        hist_rect: Rect,
        lo: f64,
        hi: f64,
        draw_vmin: f64,
        draw_vmax: f64,
    ) {
        let norm = self.colormap.normalization;
        let fg = ui.visuals().text_color();
        let painter = ui.painter_at(rect);
        let y_of = |v: f64| {
            bar_rect.bottom() - (value_to_frac(v, lo, hi, norm) as f32) * bar_rect.height()
        };

        // 1) Histogram bars, growing leftward from the gradient strip.
        if let Some((counts, edges)) = &self.histogram {
            let max_count = counts.iter().copied().max().unwrap_or(0);
            if max_count > 0 && hist_rect.width() > 0.0 {
                let hist_fill = fg.gamma_multiply(0.40);
                let w = hist_rect.width();
                for (i, &c) in counts.iter().enumerate() {
                    if c == 0 {
                        continue;
                    }
                    let (e0, e1) = (edges[i], edges[i + 1]);
                    let (ya, yb) = (y_of(e0), y_of(e1));
                    let (top, bot) = (ya.min(yb), ya.max(yb));
                    let len = (c as f32 / max_count as f32) * w;
                    let bar = Rect::from_min_max(
                        pos2(hist_rect.right() - len, top),
                        pos2(hist_rect.right(), bot),
                    );
                    painter.rect_filled(bar, egui::CornerRadius::ZERO, hist_fill);
                }
            }
        }

        // 2) Gradient strip: per-row color = colormap.normalize(value), which
        // saturates outside [vmin, vmax] and follows the normalization between.
        let height = bar_rect.height();
        let steps = height.ceil().max(1.0) as usize;
        for i in 0..steps {
            let y0 = bar_rect.top() + i as f32;
            let frac = ((height - i as f32 - 0.5) / height).clamp(0.0, 1.0) as f64;
            let value = frac_to_value(frac, lo, hi, norm);
            let idx =
                ((self.colormap.normalize(value) * 255.0).round() as i32).clamp(0, 255) as usize;
            let c = self.colormap.lut[idx];
            painter.rect_filled(
                Rect::from_min_max(pos2(bar_rect.left(), y0), pos2(bar_rect.right(), y0 + 1.0)),
                egui::CornerRadius::ZERO,
                Color32::from_rgb(c[0], c[1], c[2]),
            );
        }
        painter.rect_stroke(
            bar_rect,
            egui::CornerRadius::ZERO,
            Stroke::new(1.0_f32, fg),
            egui::StrokeKind::Inside,
        );

        // 3) Handles + value labels. Labels anchor to the column's right edge.
        let columns = HandleColumns {
            bar_rect,
            hist_left: hist_rect.left(),
            label_right: rect.right() - GAP,
        };
        self.paint_handle(&painter, &columns, y_of(draw_vmax), draw_vmax, fg);
        self.paint_handle(&painter, &columns, y_of(draw_vmin), draw_vmin, fg);
    }

    /// Draw one level handle: a line across the histogram + strip, a right-
    /// pointing triangle at the strip's left edge, and the level value label.
    fn paint_handle(
        &self,
        painter: &egui::Painter,
        columns: &HandleColumns,
        y: f32,
        value: f64,
        fg: Color32,
    ) {
        let bar_rect = columns.bar_rect;
        // High-contrast accent visible over any gradient color.
        let accent = Color32::from_rgb(20, 130, 240);
        let outline = Color32::WHITE;
        painter.line_segment(
            [pos2(columns.hist_left, y), pos2(bar_rect.right(), y)],
            Stroke::new(1.5_f32, accent),
        );
        let tri = vec![
            pos2(bar_rect.left() - 2.0 * HANDLE_TRI, y - HANDLE_TRI),
            pos2(bar_rect.left() - 2.0 * HANDLE_TRI, y + HANDLE_TRI),
            pos2(bar_rect.left(), y),
        ];
        painter.add(Shape::convex_polygon(
            tri,
            accent,
            Stroke::new(1.0_f32, outline),
        ));

        // Value label right of the strip, clamped inside the bar span. RIGHT-
        // anchored at the column's right edge: the text grows leftward, so no
        // formatted width can ever pass the edge (left-anchored growth was the
        // structural cause of drag-time clipping — e.g. "0" widening to
        // "5.74e-2" mid-drag).
        let font = FontId::proportional(FONT_SIZE);
        let galley = painter.layout_no_wrap(format_end_label(value), font, fg);
        let half_h = galley.size().y * 0.5;
        let cy =
            crate::widget::chrome::clamp_label_center(y, bar_rect.top(), bar_rect.bottom(), half_h);
        painter.galley(
            pos2(columns.label_right - galley.size().x, cy - half_h),
            galley,
            fg,
        );
    }
}

/// Horizontal geometry shared by both handle draws: the gradient strip, the
/// histogram's left edge (where the level line starts), and the right edge the
/// value labels anchor to.
struct HandleColumns {
    bar_rect: Rect,
    hist_left: f32,
    label_right: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── axis_range ──────────────────────────────────────────────────────

    #[test]
    fn axis_range_pads_linear_span_symmetrically() {
        let (lo, hi) = axis_range((0.0, 100.0), Normalization::Linear);
        // 5% of 100 padded on each end.
        assert!((lo - (-5.0)).abs() < 1e-9, "{lo}");
        assert!((hi - 105.0).abs() < 1e-9, "{hi}");
    }

    #[test]
    fn axis_range_swaps_reversed_input() {
        let (lo, hi) = axis_range((10.0, 2.0), Normalization::Linear);
        assert!(lo < hi);
        assert!(lo < 2.0 && hi > 10.0);
    }

    #[test]
    fn axis_range_degenerate_opens_a_window() {
        let (lo, hi) = axis_range((7.0, 7.0), Normalization::Linear);
        assert!(lo < 7.0 && hi > 7.0, "({lo}, {hi})");
    }

    #[test]
    fn axis_range_log_floors_nonpositive_lo_positive() {
        let (lo, hi) = axis_range((-5.0, 1000.0), Normalization::Log);
        assert!(lo > 0.0, "log lo must be positive, got {lo}");
        assert!(hi > 1000.0);
    }

    #[test]
    fn axis_range_log_all_nonpositive_falls_back() {
        let (lo, hi) = axis_range((-5.0, -1.0), Normalization::Log);
        assert_eq!((lo, hi), (1.0, 10.0));
    }

    // ── value_to_frac / frac_to_value ───────────────────────────────────

    #[test]
    fn value_to_frac_linear_endpoints_and_mid() {
        let n = Normalization::Linear;
        assert!((value_to_frac(0.0, 0.0, 10.0, n) - 0.0).abs() < 1e-12);
        assert!((value_to_frac(10.0, 0.0, 10.0, n) - 1.0).abs() < 1e-12);
        assert!((value_to_frac(5.0, 0.0, 10.0, n) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn value_to_frac_clamps_outside_axis() {
        let n = Normalization::Linear;
        assert_eq!(value_to_frac(-5.0, 0.0, 10.0, n), 0.0);
        assert_eq!(value_to_frac(15.0, 0.0, 10.0, n), 1.0);
    }

    #[test]
    fn value_frac_round_trips_linear_log_sqrt() {
        for (n, lo, hi, v) in [
            (Normalization::Linear, 0.0, 10.0, 3.0),
            (Normalization::Log, 1.0, 1000.0, 50.0),
            (Normalization::Sqrt, 0.0, 100.0, 25.0),
        ] {
            let f = value_to_frac(v, lo, hi, n);
            let back = frac_to_value(f, lo, hi, n);
            assert!((back - v).abs() < 1e-6 * v.max(1.0), "{n:?}: {back} vs {v}");
        }
    }

    #[test]
    fn value_to_frac_log_is_geometric_midpoint() {
        // On a log axis [1, 100], value 10 is the geometric midpoint -> frac 0.5.
        let f = value_to_frac(10.0, 1.0, 100.0, Normalization::Log);
        assert!((f - 0.5).abs() < 1e-9, "{f}");
    }

    #[test]
    fn frac_to_value_degenerate_axis_returns_lo() {
        assert_eq!(frac_to_value(0.7, 5.0, 5.0, Normalization::Linear), 5.0);
    }

    // ── hit_handle ──────────────────────────────────────────────────────

    #[test]
    fn hit_handle_picks_nearest_within_tol() {
        assert_eq!(hit_handle(0.21, 0.2, 0.8, 0.05), Some(Handle::Min));
        assert_eq!(hit_handle(0.79, 0.2, 0.8, 0.05), Some(Handle::Max));
        assert_eq!(hit_handle(0.5, 0.2, 0.8, 0.05), None);
    }

    #[test]
    fn hit_handle_both_in_tol_takes_closer() {
        // Handles close together; pointer nearer the max.
        assert_eq!(hit_handle(0.52, 0.48, 0.53, 0.1), Some(Handle::Max));
        assert_eq!(hit_handle(0.49, 0.48, 0.53, 0.1), Some(Handle::Min));
    }

    // ── auto_range_levels ───────────────────────────────────────────────

    #[test]
    fn auto_range_levels_returns_data_extremes() {
        assert_eq!(auto_range_levels((-0.45, 1.02)), Some((-0.45, 1.02)));
    }

    #[test]
    fn auto_range_levels_swaps_reversed_range() {
        assert_eq!(auto_range_levels((9.0, 1.0)), Some((1.0, 9.0)));
    }

    #[test]
    fn auto_range_levels_rejects_degenerate_and_non_finite() {
        assert_eq!(auto_range_levels((5.0, 5.0)), None);
        assert_eq!(auto_range_levels((f64::NAN, 1.0)), None);
        assert_eq!(auto_range_levels((0.0, f64::INFINITY)), None);
    }

    // ── apply_handle_drag ───────────────────────────────────────────────

    #[test]
    fn apply_handle_drag_min_clamps_to_lo_and_below_vmax() {
        // Drag min below lo -> clamps to lo; vmax untouched.
        let (a, b) = apply_handle_drag(Handle::Min, -100.0, 2.0, 8.0, 0.0, 10.0, 0.5);
        assert_eq!((a, b), (0.0, 8.0));
        // Drag min up past vmax -> stops min_sep below vmax.
        let (a, b) = apply_handle_drag(Handle::Min, 20.0, 2.0, 8.0, 0.0, 10.0, 0.5);
        assert!((a - 7.5).abs() < 1e-12, "{a}");
        assert_eq!(b, 8.0);
    }

    #[test]
    fn apply_handle_drag_max_clamps_to_hi_and_above_vmin() {
        // Drag max above hi -> clamps to hi; vmin untouched.
        let (a, b) = apply_handle_drag(Handle::Max, 100.0, 2.0, 8.0, 0.0, 10.0, 0.5);
        assert_eq!((a, b), (2.0, 10.0));
        // Drag max down past vmin -> stops min_sep above vmin.
        let (a, b) = apply_handle_drag(Handle::Max, -5.0, 2.0, 8.0, 0.0, 10.0, 0.5);
        assert_eq!(a, 2.0);
        assert!((b - 2.5).abs() < 1e-12, "{b}");
    }

    #[test]
    fn apply_handle_drag_does_not_panic_when_levels_touch_lo() {
        // vmax already at lo: upper bound is max(vmax-sep, lo) == lo, clamp(lo,lo).
        let (a, b) = apply_handle_drag(Handle::Min, 5.0, 0.0, 0.0, 0.0, 10.0, 0.5);
        assert_eq!(a, 0.0);
        assert_eq!(b, 0.0);
    }

    // ── widget plumbing ─────────────────────────────────────────────────

    #[test]
    fn new_takes_levels_from_colormap() {
        let w = HistogramColorBar::new(Colormap::viridis(2.0, 9.0));
        assert_eq!((w.vmin, w.vmax), (2.0, 9.0));
        assert_eq!(w.data_range, (2.0, 9.0));
        assert!(w.histogram.is_none());
    }

    #[test]
    fn builders_set_range_histogram_levels() {
        let w = HistogramColorBar::new(Colormap::viridis(0.0, 1.0))
            .with_data_range((-1.0, 5.0))
            .with_histogram(Some((vec![1, 2], vec![0.0, 1.0, 2.0])))
            .with_levels(0.5, 4.0);
        assert_eq!(w.data_range, (-1.0, 5.0));
        assert_eq!(w.vmin, 0.5);
        assert_eq!(w.vmax, 4.0);
        assert!(w.histogram.is_some());
        assert!(w.bar_bounds.is_none());
    }

    #[test]
    fn with_bar_bounds_sets_strip_span() {
        let w = HistogramColorBar::new(Colormap::viridis(0.0, 1.0)).with_bar_bounds(120.0, 480.0);
        assert_eq!(w.bar_bounds, Some((120.0, 480.0)));
    }

    // ── headless paint path (egui painter only, no GPU) ─────────────────

    #[test]
    fn ui_paints_without_panicking() {
        // Exercises the full ui() render path (gradient loop, histogram bars,
        // handles, labels, per-handle interact) headlessly; no input, so no drag.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let bar = HistogramColorBar::new(Colormap::viridis(0.0, 1.0))
                .with_data_range((0.0, 1.0))
                .with_histogram(Some((vec![3, 7, 2], vec![0.0, 0.33, 0.66, 1.0])))
                .with_levels(0.2, 0.8);
            let resp = bar.ui(ui, egui::vec2(150.0, 300.0));
            assert!(resp.dragged_levels.is_none());
        });
    }

    #[test]
    fn ui_with_pinned_bounds_paints_within_box() {
        // Pinned bounds inset well inside the allocated box: the strip aligns to
        // the bounds (image data-area guides) and nothing panics / clips.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let origin = ui.max_rect().top();
            let bar = HistogramColorBar::new(Colormap::viridis(0.0, 1.0))
                .with_data_range((0.0, 1.0))
                .with_histogram(Some((vec![3, 7, 2], vec![0.0, 0.33, 0.66, 1.0])))
                .with_levels(0.2, 0.8)
                .with_bar_bounds(origin + 40.0, origin + 260.0);
            let _ = bar.ui(ui, egui::vec2(170.0, 300.0));
        });
    }

    #[test]
    fn ui_at_renders_at_explicit_rect_without_panic() {
        // Embedded form: render into a fixed gutter rect (as PlotView does for a
        // chrome colorbar) rather than allocating from the layout cursor.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let origin = ui.max_rect().left_top();
            let gutter = Rect::from_min_max(
                pos2(origin.x + 300.0, origin.y + 20.0),
                pos2(origin.x + 470.0, origin.y + 320.0),
            );
            let bar = HistogramColorBar::new(Colormap::viridis(0.0, 1.0))
                .with_data_range((0.0, 1.0))
                .with_histogram(Some((vec![3, 7, 2], vec![0.0, 0.33, 0.66, 1.0])))
                .with_levels(0.2, 0.8)
                .with_bar_bounds(gutter.top(), gutter.bottom());
            let _ = bar.ui_at(ui, gutter);
        });
    }

    #[test]
    fn ui_scientific_notation_levels_paint_within_box() {
        // The reported case: a level small enough to format as "6.20e-2" (silx
        // %.2e). The label gutter is measured from the drawn value, so the wide
        // scientific label must lay out inside the column without panicking.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let bar = HistogramColorBar::new(Colormap::viridis(0.062, 1.0))
                .with_data_range((0.0, 1.0))
                .with_histogram(Some((vec![9, 4, 1], vec![0.0, 0.33, 0.66, 1.0])))
                .with_levels(0.062, 1.0);
            let _ = bar.ui(ui, egui::vec2(175.0, 320.0));
        });
    }

    #[test]
    fn labels_never_pass_the_column_right_edge() {
        // Structural invariant: value labels are RIGHT-anchored at the column's
        // right edge, so no formatted width can clip there — the drag-time
        // regression where "0" widening to "5.74e-2" overran the rect. Render a
        // wide scientific level and assert every painted text shape ends at or
        // before the column edge. The column is made NARROWER than the floor
        // label reserve, so the old left-anchored placement (bar right + GAP)
        // would overrun the edge — the test discriminates, not just documents.
        fn text_right_edges(shape: &egui::Shape, out: &mut Vec<f32>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.pos.x + t.galley.size().x),
                egui::Shape::Vec(v) => v.iter().for_each(|s| text_right_edges(s, out)),
                _ => {}
            }
        }

        let ctx = egui::Context::default();
        let mut gutter = Rect::NOTHING;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let origin = ui.max_rect().left_top();
            gutter = Rect::from_min_max(
                pos2(origin.x + 10.0, origin.y + 10.0),
                pos2(origin.x + 70.0, origin.y + 310.0),
            );
            let bar = HistogramColorBar::new(Colormap::viridis(0.0574, 1.0))
                .with_data_range((0.0, 1.0))
                .with_histogram(Some((vec![9, 4, 1], vec![0.0, 0.33, 0.66, 1.0])))
                .with_levels(0.0574, 1.0);
            let _ = bar.ui_at(ui, gutter);
        });

        let mut rights = Vec::new();
        for clipped in &output.shapes {
            text_right_edges(&clipped.shape, &mut rights);
        }
        assert!(
            rights.len() >= 2,
            "expected at least the two level labels, got {}",
            rights.len()
        );
        for r in rights {
            assert!(
                r <= gutter.right() + 0.5,
                "label right edge {r} passes the column edge {}",
                gutter.right()
            );
        }
    }

    #[test]
    fn ui_handles_degenerate_inputs_without_panic() {
        // All-equal data + vmin==vmax + no histogram + tiny rect: the degenerate
        // axis and clamp paths must not panic.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let bar = HistogramColorBar::new(Colormap::viridis(5.0, 5.0))
                .with_data_range((5.0, 5.0))
                .with_histogram(None)
                .with_levels(5.0, 5.0);
            let _ = bar.ui(ui, egui::vec2(80.0, 50.0));
        });
    }

    #[test]
    fn ui_log_normalization_paints_without_panic() {
        // Log colormap with a non-positive-inclusive data range exercises the
        // log positivity guards in axis_range / value_to_frac during paint.
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let cmap = Colormap::viridis(1.0, 1000.0).with_normalization(Normalization::Log);
            let bar = HistogramColorBar::new(cmap)
                .with_data_range((-2.0, 1000.0))
                .with_histogram(Some((vec![1, 5, 9], vec![1.0, 10.0, 100.0, 1000.0])))
                .with_levels(10.0, 500.0);
            let _ = bar.ui(ui, egui::vec2(150.0, 300.0));
        });
    }
}
