//! Standalone colorbar widget.
//!
//! [`ColorBarWidget`] paints a 256-step gradient bar from a [`Colormap`] with
//! "nice"-number / decade ticks, formatted min/max end labels, and an optional
//! rotated legend beside the bar. It is self-contained: it draws with an
//! [`egui::Painter`] only (no GPU), so its geometry/format helpers are pure and
//! unit-tested without a device.
//!
//! Faithful to silx `silx/gui/plot/ColorBar.py`:
//!
//! - the gradient strip is `ColorScaleBar` / `_ColorScale` (256 control points,
//!   `vmax` at the top, `vmin` at the bottom under the normalization);
//! - tick values use `_TickBar.computeTicks` → `ticklayout.niceNumbers` for
//!   linear/sqrt/gamma/arcsinh and `ticklayout.niceNumbersForLog10` (plus
//!   `computeLogSubTicks`) for log;
//! - tick label format follows `_TickBar._guessType` (standard `%.<nfrac>f`,
//!   else scientific `%.0e`);
//! - the min/max end labels follow `ColorScaleBar._updateMinMax`
//!   (`%.7g` when `0 <= log10(abs) < 7`, else `%.2e`);
//! - the rotated title is `_VerticalLegend` (text drawn rotated 270°).
//!
//! Deferred (see this wave's report): wiring the widget into
//! ImageView/ScatterView/chrome and tracking a plot's active item.

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, Vec2, pos2};

use crate::core::colormap::{Colormap, Normalization};
use crate::core::ticklayout::{nice_numbers, number_of_digits};

/// Orientation of the gradient bar (silx `ColorBar` is orientation-agnostic;
/// `ColorScaleBar`/`_TickBar` lay out vertically, the horizontal form mirrors
/// it across the diagonal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorBarOrientation {
    /// `vmin` at the bottom, `vmax` at the top; ticks/labels to the right
    /// (silx default).
    #[default]
    Vertical,
    /// `vmin` at the left, `vmax` at the right; ticks/labels below.
    Horizontal,
}

/// A self-contained colorbar widget painting a [`Colormap`] gradient with ticks,
/// end labels, and an optional legend.
#[derive(Clone, Debug)]
pub struct ColorBarWidget {
    /// The colormap whose gradient and value range are displayed.
    pub colormap: Colormap,
    /// Bar orientation.
    pub orientation: ColorBarOrientation,
    /// Legend/title drawn beside the bar (rotated when vertical). Empty hides
    /// it (silx `setLegend(None)`).
    pub legend: String,
}

/// Tick line length in points (silx `_TickBar._LINE_WIDTH`).
const LINE_WIDTH: f32 = 10.0;
/// Gradient-strip thickness in points (silx `_ColorScale.setFixedWidth(25)`),
/// minus its 1px border on each side.
const BAR_THICKNESS: f32 = 25.0;
/// Tick label font size in points (silx `_TickBar._FONT_SIZE`).
const TICK_FONT_SIZE: f32 = 11.0;
/// Gap between a tick mark and its label, in points.
const TICK_LABEL_GAP: f32 = 3.0;
/// silx default tick density (`_TickBar.DEFAULT_TICK_DENSITY`): ticks per pixel.
const DEFAULT_TICK_DENSITY: f64 = 0.015;

impl ColorBarWidget {
    /// A vertical colorbar for `colormap` with no legend.
    pub fn new(colormap: Colormap) -> Self {
        Self {
            colormap,
            orientation: ColorBarOrientation::Vertical,
            legend: String::new(),
        }
    }

    /// Set the orientation (builder form).
    pub fn with_orientation(mut self, orientation: ColorBarOrientation) -> Self {
        self.orientation = orientation;
        self
    }

    /// Set the legend/title text (builder form). Empty hides it.
    pub fn with_legend(mut self, legend: impl Into<String>) -> Self {
        self.legend = legend.into();
        self
    }

    /// Paint the colorbar into a `desired`-sized region of `ui` and return the
    /// allocated [`egui::Response`]. The gradient strip, ticks, labels, end
    /// labels, and (when set) the rotated legend are drawn inside the allotted
    /// rect; nothing is drawn outside it.
    pub fn ui(&self, ui: &mut egui::Ui, desired: Vec2) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let fg = ui.visuals().text_color();
        let painter = ui.painter_at(rect);
        self.paint(&painter, rect, fg);
        response
    }

    /// Paint into an explicit rect with an explicit foreground color. Split out
    /// from [`Self::ui`] so callers that own a painter (e.g. chrome) can reuse
    /// the drawing without re-allocating.
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, fg: Color32) {
        let stroke = Stroke::new(1.0, fg);
        // Reserve the legend strip first; the bar + ticks fill what remains.
        let (bar_area, legend_area) = self.split_legend(rect);
        let bar_rect = self.bar_rect(bar_area);

        self.paint_gradient(painter, bar_rect);
        painter.rect_stroke(
            bar_rect,
            egui::CornerRadius::ZERO,
            stroke,
            egui::StrokeKind::Inside,
        );

        self.paint_ticks_and_labels(painter, bar_rect, fg);
        self.paint_end_labels(painter, bar_rect, fg);
        if let Some(legend_area) = legend_area {
            self.paint_legend(painter, legend_area, fg);
        }
    }

    /// The bar's "long" pixel extent (height when vertical, width when
    /// horizontal): the axis along which the value range is mapped.
    fn bar_length(&self, bar_rect: Rect) -> f32 {
        match self.orientation {
            ColorBarOrientation::Vertical => bar_rect.height(),
            ColorBarOrientation::Horizontal => bar_rect.width(),
        }
    }

    /// Carve the legend strip off `rect`, returning `(remaining, legend_rect)`.
    /// The legend sits at the far end of the cross axis (to the right of the
    /// ticks when vertical, below them when horizontal). `None` when no legend.
    fn split_legend(&self, rect: Rect) -> (Rect, Option<Rect>) {
        if self.legend.is_empty() {
            return (rect, None);
        }
        // A single line of text rotated to run along the bar: reserve its line
        // height across the cross axis (silx `_VerticalLegend` fixes its width
        // to the font height).
        let strip = TICK_FONT_SIZE + 6.0;
        match self.orientation {
            ColorBarOrientation::Vertical => {
                let split = rect.right() - strip;
                (
                    Rect::from_min_max(rect.min, pos2(split, rect.bottom())),
                    Some(Rect::from_min_max(pos2(split, rect.top()), rect.max)),
                )
            }
            ColorBarOrientation::Horizontal => {
                let split = rect.bottom() - strip;
                (
                    Rect::from_min_max(rect.min, pos2(rect.right(), split)),
                    Some(Rect::from_min_max(pos2(rect.left(), split), rect.max)),
                )
            }
        }
    }

    /// The gradient strip rect inside `area`: a fixed-thickness band against the
    /// leading edge, leaving the rest of the cross axis for ticks/labels.
    fn bar_rect(&self, area: Rect) -> Rect {
        let thickness = BAR_THICKNESS.min(match self.orientation {
            ColorBarOrientation::Vertical => area.width(),
            ColorBarOrientation::Horizontal => area.height(),
        });
        match self.orientation {
            ColorBarOrientation::Vertical => {
                Rect::from_min_max(area.min, pos2(area.left() + thickness, area.bottom()))
            }
            ColorBarOrientation::Horizontal => {
                Rect::from_min_max(area.min, pos2(area.right(), area.top() + thickness))
            }
        }
    }

    /// Pixel position along the bar for normalized fraction `frac` in `[0, 1]`
    /// (0 at `vmin`, 1 at `vmax`): top→bottom when vertical (`vmax` on top),
    /// left→right when horizontal.
    fn pos_for_frac(&self, bar_rect: Rect, frac: f32) -> f32 {
        match self.orientation {
            // silx `_TickBar._getRelativePosition` returns `1 - frac`, then
            // `height * relPos`: frac=1 (vmax) -> top, frac=0 (vmin) -> bottom.
            ColorBarOrientation::Vertical => bar_rect.bottom() - frac * bar_rect.height(),
            ColorBarOrientation::Horizontal => bar_rect.left() + frac * bar_rect.width(),
        }
    }

    /// Fill the strip with 256 gradient steps (silx `_ColorScale`,
    /// `_NB_CONTROL_POINTS = 256`). `vmax` is at the high-value end.
    fn paint_gradient(&self, painter: &egui::Painter, bar_rect: Rect) {
        let n = 256usize;
        let length = self.bar_length(bar_rect);
        let step = length / n as f32;
        for i in 0..n {
            // i = 0 at the low-value end -> LUT 0 (vmin); i = n-1 -> LUT 255.
            let c = self.colormap.lut[i];
            let color = Color32::from_rgb(c[0], c[1], c[2]);
            let strip = match self.orientation {
                ColorBarOrientation::Vertical => {
                    // Low value (i = 0) at the bottom, high at the top.
                    let y1 = bar_rect.bottom() - i as f32 * step;
                    Rect::from_min_max(
                        pos2(bar_rect.left(), y1 - step - 0.5),
                        pos2(bar_rect.right(), y1),
                    )
                }
                ColorBarOrientation::Horizontal => {
                    let x0 = bar_rect.left() + i as f32 * step;
                    Rect::from_min_max(
                        pos2(x0, bar_rect.top()),
                        pos2(x0 + step + 0.5, bar_rect.bottom()),
                    )
                }
            };
            painter.rect_filled(strip, egui::CornerRadius::ZERO, color);
        }
    }

    /// Draw major ticks (with labels) and minor sub-ticks along the bar.
    fn paint_ticks_and_labels(&self, painter: &egui::Painter, bar_rect: Rect, fg: Color32) {
        if self.colormap.vmax <= self.colormap.vmin {
            return;
        }
        let length = self.bar_length(bar_rect) as f64;
        let nticks = optimal_nb_ticks(length, DEFAULT_TICK_DENSITY);
        let layout = self.tick_layout(nticks);
        let stroke = Stroke::new(1.0, fg);
        let font = FontId::proportional(TICK_FONT_SIZE);

        for &v in &layout.sub_ticks {
            self.paint_tick(painter, bar_rect, v, None, stroke, &font, fg);
        }
        for &v in &layout.ticks {
            let label = layout.format.format(v);
            self.paint_tick(painter, bar_rect, v, Some(label), stroke, &font, fg);
        }
    }

    /// Paint a single tick mark at value `v`; major ticks (with `label`) draw
    /// the full line and the text, minor sub-ticks a half-length line only
    /// (silx `_TickBar._paintTick`).
    #[allow(clippy::too_many_arguments)]
    fn paint_tick(
        &self,
        painter: &egui::Painter,
        bar_rect: Rect,
        v: f64,
        label: Option<String>,
        stroke: Stroke,
        font: &FontId,
        fg: Color32,
    ) {
        // UNCLAMPED fraction (silx `_getRelativePosition`, ColorBar.py:808-820):
        // an out-of-range tick extrapolates past the bar and is clipped by the
        // widget viewport rather than landing on the bar edge with a wrong label.
        let frac = tick_frac(&self.colormap, v);
        let in_bar = (0.0..=1.0).contains(&frac);
        let line_width = if label.is_some() {
            LINE_WIDTH
        } else {
            LINE_WIDTH / 2.0
        };
        match self.orientation {
            ColorBarOrientation::Vertical => {
                let y = self.pos_for_frac(bar_rect, frac);
                painter.line_segment(
                    [
                        pos2(bar_rect.right() - line_width, y),
                        pos2(bar_rect.right(), y),
                    ],
                    stroke,
                );
                if let Some(label) = label {
                    // An in-range tick landing at the bar edge (frac 0/1) would
                    // center its label on the clip boundary and lose half the
                    // glyph; clamp the text center into the bar span
                    // (chrome::clamp_label_center). An OUT-of-range tick must
                    // NOT be pulled back in — its label extrapolates with the
                    // line and clips away (silx viewport clipping).
                    let galley = painter.layout_no_wrap(label, font.clone(), fg);
                    let half_h = galley.size().y * 0.5;
                    let cy = if in_bar {
                        crate::widget::chrome::clamp_label_center(
                            y,
                            bar_rect.top(),
                            bar_rect.bottom(),
                            half_h,
                        )
                    } else {
                        y
                    };
                    painter.galley(
                        pos2(bar_rect.right() + TICK_LABEL_GAP, cy - half_h),
                        galley,
                        fg,
                    );
                }
            }
            ColorBarOrientation::Horizontal => {
                let x = self.pos_for_frac(bar_rect, frac);
                painter.line_segment(
                    [
                        pos2(x, bar_rect.bottom() - line_width),
                        pos2(x, bar_rect.bottom()),
                    ],
                    stroke,
                );
                if let Some(label) = label {
                    // Same edge-overhang guard on the horizontal (x) axis,
                    // same out-of-range exemption.
                    let galley = painter.layout_no_wrap(label, font.clone(), fg);
                    let half_w = galley.size().x * 0.5;
                    let cx = if in_bar {
                        crate::widget::chrome::clamp_label_center(
                            x,
                            bar_rect.left(),
                            bar_rect.right(),
                            half_w,
                        )
                    } else {
                        x
                    };
                    painter.galley(
                        pos2(cx - half_w, bar_rect.bottom() + TICK_LABEL_GAP),
                        galley,
                        fg,
                    );
                }
            }
        }
    }

    /// Draw the formatted `vmin`/`vmax` labels at the bar ends (silx
    /// `ColorScaleBar._updateMinMax`).
    fn paint_end_labels(&self, painter: &egui::Painter, bar_rect: Rect, fg: Color32) {
        let font = FontId::proportional(TICK_FONT_SIZE);
        let max_text = format_end_label(self.colormap.vmax);
        let min_text = format_end_label(self.colormap.vmin);
        match self.orientation {
            ColorBarOrientation::Vertical => {
                painter.text(
                    pos2(bar_rect.right() + TICK_LABEL_GAP, bar_rect.top()),
                    Align2::LEFT_TOP,
                    max_text,
                    font.clone(),
                    fg,
                );
                painter.text(
                    pos2(bar_rect.right() + TICK_LABEL_GAP, bar_rect.bottom()),
                    Align2::LEFT_BOTTOM,
                    min_text,
                    font,
                    fg,
                );
            }
            ColorBarOrientation::Horizontal => {
                painter.text(
                    pos2(bar_rect.left(), bar_rect.bottom() + TICK_LABEL_GAP),
                    Align2::LEFT_TOP,
                    min_text,
                    font.clone(),
                    fg,
                );
                painter.text(
                    pos2(bar_rect.right(), bar_rect.bottom() + TICK_LABEL_GAP),
                    Align2::RIGHT_TOP,
                    max_text,
                    font,
                    fg,
                );
            }
        }
    }

    /// Draw the legend text rotated to run along the bar (silx
    /// `_VerticalLegend`: rotated 270° for the vertical layout).
    fn paint_legend(&self, painter: &egui::Painter, area: Rect, fg: Color32) {
        let font = FontId::proportional(TICK_FONT_SIZE);
        let galley = painter.layout_no_wrap(self.legend.clone(), font, fg);
        match self.orientation {
            ColorBarOrientation::Vertical => {
                // Rotated 270° (silx painter.rotate(270)): text reads bottom-up,
                // centered along the bar. egui rotation is clockwise-positive, so
                // -90° gives the silx orientation.
                //
                // epaint's `with_angle_and_anchor` with `CENTER_CENTER` lands the
                // galley center at `pos + galley_center` (the rotation term cancels,
                // so the offset is angle-independent), NOT at `pos`. Pre-subtracting
                // the galley center cancels that offset so the legend's visual center
                // sits on the strip center regardless of legend length — the same
                // idiom as `chrome::draw_rotated_label`.
                let angle = -std::f32::consts::FRAC_PI_2;
                let pos = area.center() - galley.rect.center().to_vec2();
                painter.add(egui::Shape::Text(
                    egui::epaint::TextShape::new(pos, galley, fg)
                        .with_angle_and_anchor(angle, Align2::CENTER_CENTER),
                ));
            }
            ColorBarOrientation::Horizontal => {
                let center = area.center();
                let pos = center - galley.size() * 0.5;
                painter.add(egui::epaint::TextShape::new(pos, galley, fg));
            }
        }
    }

    /// Compute the tick values, sub-ticks, and label format for the current
    /// range under the colormap's normalization (silx `_TickBar.computeTicks`).
    fn tick_layout(&self, nticks: usize) -> TickLayout {
        tick_layout(
            self.colormap.vmin,
            self.colormap.vmax,
            self.colormap.normalization,
            nticks,
        )
    }
}

/// silx `_TickBar._getOptimalNbTicks`: `max(2, round(density * length))`.
fn optimal_nb_ticks(length: f64, density: f64) -> usize {
    (2.0_f64).max((density * length).round()) as usize
}

/// The UNCLAMPED bar fraction for a tick value — silx
/// `_TickBar._getRelativePosition` (ColorBar.py:808-820), `1 - relativePos`
/// in rsplot's frac convention (0 at `vmin`, 1 at `vmax`).
///
/// Distinct by design from [`Colormap::normalize`], the shader mirror that
/// clamps into `[0, 1]` for color lookup: a tick outside `[vmin, vmax]` (the
/// nice-number `graphmin`/`graphmax`, or a log decade below `vmin`) must
/// extrapolate past the bar and be clipped by the widget viewport, not land on
/// the bar edge labeled with a value that is not the edge value.
///
/// Ported quirks: silx returns relative position `0.0` — the `vmax` end, frac
/// `1.0` here — when the normalized value is non-finite (:818-819; a log tick
/// at `v <= 0`, or a gamma tick below `vmin` whose negative ratio powers to
/// NaN). A degenerate or log-invalid *range* has `one_over_range == 0`
/// (`Colormap::norm_bounds`), collapsing every tick to frac `0` — silx has no
/// defined behavior there (its arithmetic yields NaN heights), and
/// `paint_ticks_and_labels` never reaches this path for `vmax <= vmin`.
fn tick_frac(colormap: &Colormap, v: f64) -> f32 {
    let (cmap_min, one_over_range) = colormap.norm_bounds();
    let base = one_over_range * (colormap.normalization.transform(v) as f32 - cmap_min);
    let frac = match colormap.normalization {
        Normalization::Gamma => base.powf(colormap.gamma),
        _ => base,
    };
    if frac.is_finite() { frac } else { 1.0 }
}

// --- Tick layout (pure) ------------------------------------------------------

/// The label-format chosen for a tick set (silx `_TickBar._getFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TickFormat {
    /// Standard fixed-point with `nfrac` fractional digits (`{:.nfrac}f`).
    Standard(usize),
    /// Scientific with no fractional digits (`{:.0e}`).
    Scientific,
}

impl TickFormat {
    /// Format a tick value (silx `self.form.format(val)`).
    fn format(self, v: f64) -> String {
        match self {
            TickFormat::Standard(nfrac) => format!("{v:.nfrac$}"),
            // silx `{0:.0e}` -> mantissa with no decimals plus exponent.
            TickFormat::Scientific => format!("{v:.0e}"),
        }
    }
}

/// The result of laying out ticks for one range: major tick values, minor
/// sub-tick values, and the chosen label format (silx `_TickBar` state).
#[derive(Clone, Debug, PartialEq)]
struct TickLayout {
    ticks: Vec<f64>,
    sub_ticks: Vec<f64>,
    format: TickFormat,
}

/// Lay out ticks for `[vmin, vmax]` under `norm` requesting `nticks` ticks
/// (silx `_TickBar.computeTicks`). Log normalization uses decade layout, every
/// other normalization (linear/sqrt/gamma/arcsinh) falls back to linear nice
/// numbers, matching silx where only `LogarithmicNormalization` branches.
fn tick_layout(vmin: f64, vmax: f64, norm: Normalization, nticks: usize) -> TickLayout {
    if vmin == vmax {
        // No range: no ticks (silx returns empty tuples).
        return TickLayout {
            ticks: Vec::new(),
            sub_ticks: Vec::new(),
            format: TickFormat::Standard(0),
        };
    }
    let (ticks, sub_ticks, nfrac) = match norm {
        Normalization::Log => compute_ticks_log(vmin, vmax, nticks),
        _ => {
            let (ticks, nfrac) = compute_ticks_lin(vmin, vmax, nticks);
            (ticks, Vec::new(), nfrac)
        }
    };
    let format = guess_format(&ticks, nfrac);
    TickLayout {
        ticks,
        sub_ticks,
        format,
    }
}

/// silx `_TickBar._computeTicksLin` via `ticklayout.niceNumbers`: tick values
/// from `arange(graphmin, graphmax, spacing)` and the fractional-digit count.
fn compute_ticks_lin(vmin: f64, vmax: f64, nticks: usize) -> (Vec<f64>, usize) {
    let (graphmin, graphmax, spacing, nfrac) = nice_numbers(vmin, vmax, nticks);
    (arange(graphmin, graphmax, spacing), nfrac)
}

/// silx `_TickBar._computeTicksLog`: decade ticks `10^arange(low, high, spacing)`
/// and, when `spacing == 1`, the 2..10 sub-ticks per decade
/// (`ticklayout.computeLogSubTicks`).
fn compute_ticks_log(vmin: f64, vmax: f64, nticks: usize) -> (Vec<f64>, Vec<f64>, usize) {
    let log_min = vmin.log10();
    let log_max = vmax.log10();
    let (low, high, spacing, nfrac) = nice_numbers_for_log10(log_min, log_max, nticks);
    let exps = arange(low as f64, high as f64, spacing as f64);
    let ticks: Vec<f64> = exps.iter().map(|&e| 10f64.powf(e)).collect();
    let sub_ticks = if spacing == 1 {
        compute_log_sub_ticks(&ticks, 10f64.powi(low), 10f64.powi(high))
    } else {
        Vec::new()
    };
    (ticks, sub_ticks, nfrac)
}

/// silx `_TickBar._guessType`: keep the standard fixed-point format unless the
/// widest label would overflow the label gutter, in which case use scientific.
/// Without font metrics we approximate label width by character count against
/// the silx `_WIDTH_DISP_VAL - _LINE_WIDTH` budget.
fn guess_format(ticks: &[f64], nfrac: usize) -> TickFormat {
    let standard = TickFormat::Standard(nfrac);
    // silx budget: _WIDTH_DISP_VAL (45) - _LINE_WIDTH (10) = 35 px at font
    // size 10; at ~6 px per glyph that is ~5-6 characters before scientific.
    const MAX_CHARS: usize = 8;
    let widest = ticks
        .iter()
        .map(|&t| standard.format(t).len())
        .max()
        .unwrap_or(0);
    if widest > MAX_CHARS {
        TickFormat::Scientific
    } else {
        standard
    }
}

// --- ticklayout.py ports (pure) ----------------------------------------------
//
// The linear nice-number core (`number_of_digits`, `nice_num`, `nice_numbers`)
// lives in `crate::core::ticklayout`; only the log10 decade layout below is
// colorbar-specific.

/// silx `ticklayout.niceNumbersForLog10`: integer decade layout `(low, high,
/// spacing, nfrac)` in log10 space.
fn nice_numbers_for_log10(min_log: f64, max_log: f64, nticks: usize) -> (i32, i32, i32, usize) {
    let mut graphminlog = min_log.floor();
    let mut graphmaxlog = max_log.ceil();
    let rangelog = graphmaxlog - graphminlog;

    let spacing;
    if rangelog <= nticks as f64 {
        spacing = 1.0;
    } else {
        spacing = (rangelog / nticks as f64).floor();
        graphminlog = (graphminlog / spacing).floor() * spacing;
        graphmaxlog = (graphmaxlog / spacing).ceil() * spacing;
    }
    let nfrac = number_of_digits(spacing);
    (
        graphminlog as i32,
        graphmaxlog as i32,
        spacing as i32,
        nfrac,
    )
}

/// silx `ticklayout.computeLogSubTicks`: the 2..10 minor ticks within
/// `[low_bound, high_bound]` for each decade in `ticks`.
fn compute_log_sub_ticks(ticks: &[f64], low_bound: f64, high_bound: f64) -> Vec<f64> {
    if ticks.is_empty() {
        return Vec::new();
    }
    let mut res = Vec::new();
    for &orig in ticks {
        for index in 2..10 {
            let data_pos = orig * index as f64;
            if low_bound <= data_pos && data_pos <= high_bound {
                res.push(data_pos);
            }
        }
    }
    res
}

/// numpy `arange(start, stop, step)`: values `start, start+step, …` strictly
/// below `stop` (the stop is exclusive). silx ticks are produced this way, so
/// `graphmax` itself is not emitted.
fn arange(start: f64, stop: f64, step: f64) -> Vec<f64> {
    let mut out = Vec::new();
    if step <= 0.0 {
        return out;
    }
    // Mirror numpy's count: ceil((stop - start) / step) elements.
    let n = ((stop - start) / step).ceil();
    if !n.is_finite() || n <= 0.0 {
        return out;
    }
    let n = n as i64;
    for i in 0..n {
        out.push(start + i as f64 * step);
    }
    out
}

/// silx `ColorScaleBar._updateMinMax` end-label format: `%.7g` when the value is
/// 0 or `0 <= log10(abs(v)) < 7`, else `%.2e`. Shared with
/// [`crate::widget::histogram_colorbar`] for its end/handle value labels.
///
/// The `%.7g` branch routes through the single `%g` owner,
/// [`crate::widget::stats_widget::format_significant`], so the fixed-vs-
/// exponential decision is made on the post-rounding exponent (R2-25): a value
/// that carries up across a decade inside the `< 7` gate (`9999999.9`) formats
/// like silx `"%.7g"` (`1e+07`), not `10000000`.
pub(crate) fn format_end_label(v: f64) -> String {
    if v == 0.0 {
        return crate::widget::stats_widget::format_significant(0.0, 7);
    }
    let log = v.abs().log10();
    if (0.0..7.0).contains(&log) {
        crate::widget::stats_widget::format_significant(v, 7)
    } else {
        format!("{v:.2e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::colormap::Colormap;

    // --- tick_frac (silx _TickBar._getRelativePosition) -------------------

    #[test]
    fn tick_frac_is_unclamped_outside_the_range() {
        // silx `_getRelativePosition` does not clamp (ColorBar.py:808-820):
        // graphmin below vmin extrapolates past the bar (negative frac) and
        // graphmax above vmax past the far end (frac > 1) — unlike the
        // shader-mirror Colormap::normalize, which clamps both.
        let cm = Colormap::viridis(0.13, 0.87);
        assert!(tick_frac(&cm, 0.0) < 0.0, "graphmin tick must extrapolate");
        assert!(tick_frac(&cm, 1.0) > 1.0, "graphmax tick must extrapolate");
        assert_eq!(cm.normalize(0.0), 0.0, "the shader mirror still clamps");
        assert_eq!(cm.normalize(1.0), 1.0);
        // In-range values agree with the shader mirror.
        let mid = f64::from(cm.normalize(0.5));
        assert!((f64::from(tick_frac(&cm, 0.5)) - mid).abs() < 1e-6);
    }

    #[test]
    fn tick_frac_log_decade_below_vmin_extrapolates() {
        // The R2-13 impact case: a log bar with vmin = 3 must NOT show the
        // decade tick "1" at 3's position — it extrapolates below the bar.
        let cm = Colormap::viridis(3.0, 300.0).with_normalization(Normalization::Log);
        assert!(tick_frac(&cm, 1.0) < 0.0);
        assert_eq!(cm.normalize(1.0), 0.0, "the shader mirror still clamps");
    }

    #[test]
    fn tick_frac_non_finite_norm_lands_at_the_vmax_end() {
        // silx returns relative position 0.0 — the vmax end — for a
        // non-finite normalized value (ColorBar.py:818-819).
        let log = Colormap::viridis(1.0, 100.0).with_normalization(Normalization::Log);
        assert_eq!(tick_frac(&log, 0.0), 1.0); // log10(0) = -inf
        assert_eq!(tick_frac(&log, -5.0), 1.0); // log10(neg) = NaN
        // Gamma below vmin: negative ratio to a fractional power is NaN.
        let gamma = Colormap::viridis(0.0, 1.0)
            .with_normalization(Normalization::Gamma)
            .with_gamma(0.5);
        assert_eq!(tick_frac(&gamma, -1.0), 1.0);
    }

    // The linear `number_of_digits` / `nice_num` / `nice_numbers` core is
    // tested in `crate::core::ticklayout`; only the log10 decade layout below
    // is colorbar-specific.

    // --- niceNumbersForLog10 --------------------------------------------

    #[test]
    fn nice_numbers_log_small_range_unit_spacing() {
        // log10(1)=0 .. log10(1000)=3, 5 ticks: range 3 <= 5 -> spacing 1.
        let (low, high, spacing, nfrac) = nice_numbers_for_log10(0.0, 3.0, 5);
        assert_eq!((low, high, spacing), (0, 3, 1));
        assert_eq!(nfrac, 0);
    }

    #[test]
    fn nice_numbers_log_wide_range_spacing_above_one() {
        // 0 .. 12 decades, 5 ticks: range 12 > 5 -> spacing floor(12/5) = 2.
        let (low, high, spacing, _nfrac) = nice_numbers_for_log10(0.0, 12.0, 5);
        assert_eq!(spacing, 2);
        // graphmin/max re-floored/ceiled onto the spacing grid.
        assert_eq!(low % 2, 0);
        assert_eq!(high % 2, 0);
        assert!(low <= 0 && high >= 12);
    }

    // --- arange ----------------------------------------------------------

    #[test]
    fn arange_is_stop_exclusive() {
        // numpy arange(0, 10, 2) -> [0, 2, 4, 6, 8]; 10 is excluded.
        assert_eq!(arange(0.0, 10.0, 2.0), vec![0.0, 2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn arange_zero_or_negative_step_is_empty() {
        assert!(arange(0.0, 10.0, 0.0).is_empty());
        assert!(arange(0.0, 10.0, -1.0).is_empty());
    }

    #[test]
    fn arange_empty_when_start_at_or_above_stop() {
        assert!(arange(5.0, 5.0, 1.0).is_empty());
        assert!(arange(6.0, 5.0, 1.0).is_empty());
    }

    // --- computeLogSubTicks ---------------------------------------------

    #[test]
    fn log_sub_ticks_are_2_to_9_multiples_in_bounds() {
        // For decade 1 within [1, 100]: 2..9 (all in-bounds); for decade 10:
        // 20..90 (all in-bounds). 100 is the high bound (10*10 excluded by the
        // 2..10 loop anyway).
        let subs = compute_log_sub_ticks(&[1.0, 10.0], 1.0, 100.0);
        assert!(subs.contains(&2.0));
        assert!(subs.contains(&9.0));
        assert!(subs.contains(&20.0));
        assert!(subs.contains(&90.0));
        // Multiples outside [low, high] are dropped: 0.x can't occur here, but
        // the upper decade's 10*index beyond 100 would be; none exceed 100.
        assert!(subs.iter().all(|&s| (1.0..=100.0).contains(&s)));
    }

    #[test]
    fn log_sub_ticks_empty_for_no_ticks() {
        assert!(compute_log_sub_ticks(&[], 1.0, 100.0).is_empty());
    }

    // --- tick_layout (integration over the layout, still pure) ----------

    #[test]
    fn tick_layout_equal_range_has_no_ticks() {
        let layout = tick_layout(3.0, 3.0, Normalization::Linear, 5);
        assert!(layout.ticks.is_empty());
        assert!(layout.sub_ticks.is_empty());
    }

    #[test]
    fn tick_layout_linear_ticks_within_padded_range() {
        // [0, 10] linear: ticks are multiples of 2 from 0 (stop-exclusive).
        let layout = tick_layout(0.0, 10.0, Normalization::Linear, 5);
        assert_eq!(layout.ticks, vec![0.0, 2.0, 4.0, 6.0, 8.0]);
        assert!(layout.sub_ticks.is_empty());
        assert_eq!(layout.format, TickFormat::Standard(0));
    }

    #[test]
    fn tick_layout_log_produces_decades_and_subticks() {
        // [1, 1000] log, 5 ticks: spacing 1 -> decades 1, 10, 100 (10^3 = 1000
        // excluded as the stop), plus 2..9 sub-ticks per decade.
        let layout = tick_layout(1.0, 1000.0, Normalization::Log, 5);
        assert_eq!(layout.ticks, vec![1.0, 10.0, 100.0]);
        // Every major tick lies inside [vmin, vmax].
        assert!(layout.ticks.iter().all(|&t| (1.0..=1000.0).contains(&t)));
        // Sub-ticks exist (spacing == 1) and lie within the decade bounds.
        assert!(!layout.sub_ticks.is_empty());
        assert!(
            layout
                .sub_ticks
                .iter()
                .all(|&t| (1.0..=1000.0).contains(&t))
        );
    }

    #[test]
    fn tick_layout_non_log_norms_use_linear_layout() {
        // sqrt/gamma/arcsinh all fall back to linear nice numbers (silx only
        // branches on LogarithmicNormalization), so the tick *values* match the
        // linear layout for the same range.
        let linear = tick_layout(0.0, 10.0, Normalization::Linear, 5).ticks;
        for norm in [
            Normalization::Sqrt,
            Normalization::Gamma,
            Normalization::Arcsinh,
        ] {
            assert_eq!(tick_layout(0.0, 10.0, norm, 5).ticks, linear, "{norm:?}");
        }
    }

    // --- guess_format ----------------------------------------------------

    #[test]
    fn guess_format_switches_to_scientific_for_wide_labels() {
        // Short standard labels stay standard.
        let short = guess_format(&[0.0, 2.0, 4.0], 0);
        assert_eq!(short, TickFormat::Standard(0));
        // A very large value with many digits overflows the char budget.
        let wide = guess_format(&[0.0, 123456789.0], 0);
        assert_eq!(wide, TickFormat::Scientific);
    }

    #[test]
    fn tick_format_renders_each_style() {
        assert_eq!(TickFormat::Standard(2).format(1.5), "1.50");
        assert_eq!(TickFormat::Standard(0).format(3.0), "3");
        assert_eq!(TickFormat::Scientific.format(12345.0), "1e4");
    }

    // --- end-label format ------------------------------------------------

    #[test]
    fn end_label_uses_g_for_moderate_values() {
        // 0 -> "0"; values with 0 <= log10(abs) < 7 use %.7g (trimmed).
        assert_eq!(format_end_label(0.0), "0");
        assert_eq!(format_end_label(1.0), "1");
        assert_eq!(format_end_label(123.456), "123.456");
        // log10(9_999_999) < 7 -> still %.7g.
        assert_eq!(format_end_label(9_999_999.0), "9999999");
    }

    #[test]
    fn end_label_uses_scientific_for_large_and_small() {
        // log10(abs) >= 7 -> %.2e.
        assert_eq!(format_end_label(1.0e8), "1.00e8");
        // log10(abs) < 0 -> %.2e (negative log10).
        assert_eq!(format_end_label(0.5), "5.00e-1");
        // Negative large magnitude.
        assert_eq!(format_end_label(-2.0e9), "-2.00e9");
    }

    #[test]
    fn end_label_gate_delegates_in_range_to_g_else_scientific() {
        // `0 <= log10(abs) < 7` -> `%.7g` (via format_significant); else `%.2e`.
        assert_eq!(format_end_label(0.0), "0");
        assert_eq!(format_end_label(1.0), "1");
        assert_eq!(format_end_label(1234.5), "1234.5");
        // log10(0.001) = -3 is below the gate -> the `%.2e` branch.
        assert_eq!(format_end_label(0.001), "1.00e-3");
        // R2-25: 9999999.9 is inside the `< 7` gate (log10 ≈ 6.9999999) but
        // its `%.7g` rounds up across the decade to 1e7 -> silx `"%.7g"` gives
        // "1e+07", not "10000000" (the pre-rounding-exponent bug).
        assert_eq!(format_end_label(9999999.9), "1e+07");
    }

    // --- optimal_nb_ticks ------------------------------------------------

    #[test]
    fn optimal_nb_ticks_floors_at_two() {
        // round(density * length); never below 2.
        assert_eq!(optimal_nb_ticks(0.0, DEFAULT_TICK_DENSITY), 2); // round(0) -> max(2,0)
        assert_eq!(optimal_nb_ticks(10.0, DEFAULT_TICK_DENSITY), 2); // round(0.15) = 0 -> 2
        // 0.015 * 400 = 6.
        assert_eq!(optimal_nb_ticks(400.0, DEFAULT_TICK_DENSITY), 6);
    }

    // --- widget plumbing -------------------------------------------------

    #[test]
    fn new_defaults_to_vertical_no_legend() {
        let w = ColorBarWidget::new(Colormap::viridis(0.0, 1.0));
        assert_eq!(w.orientation, ColorBarOrientation::Vertical);
        assert!(w.legend.is_empty());
    }

    #[test]
    fn builders_set_orientation_and_legend() {
        let w = ColorBarWidget::new(Colormap::viridis(0.0, 1.0))
            .with_orientation(ColorBarOrientation::Horizontal)
            .with_legend("Intensity");
        assert_eq!(w.orientation, ColorBarOrientation::Horizontal);
        assert_eq!(w.legend, "Intensity");
    }

    // --- vertical legend centering --------------------------------------

    /// The rotated vertical legend must land its *visual* center on the strip
    /// center. epaint rotates a raw [`TextShape`] about `self.pos` (the galley
    /// origin), so the galley center renders at `pos + Rot(angle)*galley_center`
    /// — placing it at the target requires `pos = center - Rot(angle)*galley_center`,
    /// which the safe [`TextShape::with_angle_and_anchor`] + pre-subtract idiom
    /// expresses. The old `center + (H/2, -W/2)` offset instead lands the center
    /// at `center + (H, -W)` (wrong sign), pushing a long legend off the strip.
    #[test]
    fn vertical_legend_visual_center_lands_at_strip_center() {
        use egui::epaint::TextShape;
        use egui::vec2;

        let ctx = egui::Context::default();
        let mut galley = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            galley = Some(ui.painter().layout_no_wrap(
                "Intensity [counts]".to_owned(),
                FontId::proportional(TICK_FONT_SIZE),
                Color32::WHITE,
            ));
        });
        let galley = galley.expect("run closure executes once");
        assert!(
            galley.rect.width() > 40.0,
            "fixture legend should be wide so a sign error is visible"
        );

        let angle = -std::f32::consts::FRAC_PI_2;
        let center = egui::Pos2::new(312.0, 480.0);

        // Fixed construction (matches chrome::draw_rotated_label): pre-subtract
        // the galley center, then anchor CENTER_CENTER so the visual center lands
        // exactly on `center`.
        let pos = center - galley.rect.center().to_vec2();
        let fixed = TextShape::new(pos, galley.clone(), Color32::WHITE)
            .with_angle_and_anchor(angle, Align2::CENTER_CENTER);
        let c = fixed.visual_bounding_rect().center();
        // Allow ~1px: we center the galley *layout box* (`galley.rect.center()`)
        // while `visual_bounding_rect` reports the *ink* AABB, and font metrics
        // (line gap / descent) make the two differ by under a pixel. The wrong-sign
        // bug below is ~90px off, so the threshold separates them unambiguously.
        assert!(
            (c.x - center.x).abs() < 2.0 && (c.y - center.y).abs() < 2.0,
            "fixed legend center {c:?} should sit on strip center {center:?}"
        );

        // Old construction: raw `shape.angle` + `center + (H/2, -W/2)` offset.
        // It lands the visual center at `center + (H, -W)` — far off for a wide
        // legend. Guards against a regression back to the buggy offset.
        let half = vec2(galley.size().y * 0.5, -galley.size().x * 0.5);
        let mut old = TextShape::new(center + half, galley, Color32::WHITE);
        old.angle = angle;
        let oc = old.visual_bounding_rect().center();
        assert!(
            (oc - center).length() > 10.0,
            "old offset should be visibly off-center, got {oc:?} vs {center:?}"
        );
    }
}
