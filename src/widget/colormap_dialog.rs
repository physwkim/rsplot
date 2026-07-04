use crate::core::colormap::{
    AutoscaleMode, Colormap, ColormapName, DEFAULT_PERCENTILES, Normalization,
};
use crate::core::histogram::compute_histogram;
use crate::widget::high_level::Plot2D;

/// A widget for interactively configuring the colormap of a Plot2D.
pub struct ColormapDialog {
    pub name: ColormapName,
    pub normalization: Normalization,
    pub vmin: f64,
    pub vmax: f64,
    pub autoscale: bool,

    /// How autoscale derives the range from the image data (silx
    /// `Colormap.setAutoscaleMode`).
    pub autoscale_mode: AutoscaleMode,
    /// `(low, high)` percentiles for [`AutoscaleMode::Percentile`] (silx
    /// `Colormap.setAutoscalePercentiles`).
    pub percentiles: (f64, f64),

    // Gamma for Gamma normalization
    pub gamma: f32,

    /// RGBA color used for Not-A-Number values, fed into the applied colormap
    /// (silx `Colormap.setNaNColor`). Defaults to silx's
    /// `Colormap._DEFAULT_NAN_COLOR`: fully transparent white `(255, 255, 255,
    /// 0)`.
    pub nan_color: [u8; 4],

    /// Data-distribution histogram drawn behind the colormap range (silx
    /// `ColormapDialog.setHistogram`/`getHistogram`): `(counts, edges)` with
    /// `counts.len() + 1 == edges.len()`. `None` until set via
    /// [`Self::set_histogram`] or auto-computed from the active image.
    histogram: Option<(Vec<u64>, Vec<f64>)>,
    /// Whether [`Self::histogram`] came from [`Self::set_histogram`]
    /// (user-provided). A user histogram is never overwritten by the
    /// auto-compute path — silx prefers the dialog-set histogram over the
    /// image-derived one (`_computeNormalizedHistogram`).
    histogram_user_set: bool,
    /// Normalization the auto-computed histogram was binned for. Lets the dialog
    /// recompute only when the normalization changes (log vs linear bins),
    /// mirroring silx's per-norm `_histogramData` cache instead of rebinning the
    /// whole image every frame.
    histogram_norm: Option<Normalization>,
    /// Whether the dialog was open on the previous frame, to detect the
    /// open transition and refresh the auto histogram from current image data
    /// on (re)open (silx recomputes on `setData`).
    was_open: bool,

    win: crate::widget::detached::DetachedWindow,
    pub open: bool,
}

impl Default for ColormapDialog {
    fn default() -> Self {
        Self {
            name: ColormapName::Viridis,
            normalization: Normalization::Linear,
            vmin: 0.0,
            vmax: 1.0,
            autoscale: true,
            autoscale_mode: AutoscaleMode::MinMax,
            percentiles: DEFAULT_PERCENTILES,
            gamma: 2.0,
            // silx Colormap._DEFAULT_NAN_COLOR = (255, 255, 255, 0).
            nan_color: [255, 255, 255, 0],
            histogram: None,
            histogram_user_set: false,
            histogram_norm: None,
            was_open: false,
            win: crate::widget::detached::DetachedWindow::new(
                egui::Id::new("colormap_dialog"),
                egui::vec2(320.0, 420.0),
            ),
            open: false,
        }
    }
}

impl ColormapDialog {
    /// Create a new ColormapDialog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Provide the data-distribution histogram to display behind the colormap
    /// range, mirroring silx `ColormapDialog.setHistogram(hist, bin_edges)`.
    /// `counts` are per-bin sample counts and `edges` the `counts.len() + 1` bin
    /// boundaries (ascending). A user-set histogram takes precedence over the
    /// image-derived one until [`Self::clear_histogram`].
    pub fn set_histogram(&mut self, counts: Vec<u64>, edges: Vec<f64>) {
        self.histogram = Some((counts, edges));
        self.histogram_user_set = true;
    }

    /// The currently displayed histogram as `(counts, edges)`, if any (silx
    /// `getHistogram`).
    pub fn histogram(&self) -> Option<(&[u64], &[f64])> {
        self.histogram
            .as_ref()
            .map(|(c, e)| (c.as_slice(), e.as_slice()))
    }

    /// Clear any user-set or auto-computed histogram (silx `setHistogram(None)`);
    /// the dialog then re-derives it from the active image when displayed.
    pub fn clear_histogram(&mut self) {
        self.histogram = None;
        self.histogram_user_set = false;
    }

    /// Initialize the dialog from an existing Colormap.
    pub fn with_colormap(mut self, cmap: &Colormap) -> Self {
        self.vmin = cmap.vmin;
        self.vmax = cmap.vmax;
        self.normalization = cmap.normalization;
        self.gamma = cmap.gamma;
        self.nan_color = cmap.nan_color;
        self
    }

    /// A checkable toolbar button that shows/hides this dialog, mirroring silx
    /// `ColormapAction` (`actions/control.py:352-448`): a checkable action whose
    /// state tracks the colormap dialog's visibility. The button's selected
    /// state reflects [`Self::open`]; clicking it flips it. Place it in a plot
    /// toolbar row, e.g. inside [`PlotWidget::show_toolbar_with`](crate::PlotWidget::show_toolbar_with). Returns the button
    /// response.
    pub fn toggle_button(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let response = ui
            .selectable_label(self.open, "Colormap")
            .on_hover_text("Show/hide the colormap dialog");
        if response.clicked() {
            self.open = !self.open;
        }
        response
    }

    /// Show the Colormap dialog. If it's open and modified, updates the plot in real-time.
    pub fn show(&mut self, ctx: &egui::Context, plot: &mut Plot2D) {
        if !self.open {
            self.was_open = false;
            return;
        }
        // Refresh the auto-computed distribution histogram from the active image
        // when the dialog (re)opens or the normalization (log vs linear binning)
        // changed since it was last binned. A user-set histogram is left
        // untouched (silx prefers it). Recomputing only on these triggers avoids
        // rebinning the whole image every frame.
        let just_opened = !self.was_open;
        self.was_open = true;
        if !self.histogram_user_set {
            if just_opened {
                self.histogram = None;
            }
            if self.histogram.is_none() || self.histogram_norm != Some(self.normalization) {
                // silx computes the histogram over the normalization's data
                // range first (`_computeNormalizedHistogram` →
                // `computeHistogram(data, scale=norm, dataRange)`), then bins
                // log-uniformly only for LOG.
                let log = self.normalization == Normalization::Log;
                self.histogram = plot.get_image_pixels_raw().and_then(|px| {
                    let range = normalized_data_range(&px, self.normalization)?;
                    compute_histogram(&px, Some(range), log)
                });
                self.histogram_norm = Some(self.normalization);
            }
        }

        let mut changed = false;
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();

        let signals =
            crate::widget::detached::show_detached(ctx, id, "Colormap", size, pos, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    let prev_name = self.name;
                    egui::ComboBox::from_id_salt("cmap_name")
                        .selected_text(self.name.label())
                        .show_ui(ui, |ui| {
                            for &name in &ColormapName::ALL {
                                ui.selectable_value(&mut self.name, name, name.label());
                            }
                        });
                    if self.name != prev_name {
                        changed = true;
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Normalization:");
                    let prev_norm = self.normalization;
                    egui::ComboBox::from_id_salt("cmap_norm")
                        .selected_text(format!("{:?}", self.normalization))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.normalization,
                                Normalization::Linear,
                                "Linear",
                            );
                            ui.selectable_value(&mut self.normalization, Normalization::Log, "Log");
                            ui.selectable_value(
                                &mut self.normalization,
                                Normalization::Sqrt,
                                "Sqrt",
                            );
                            ui.selectable_value(
                                &mut self.normalization,
                                Normalization::Gamma,
                                "Gamma",
                            );
                            ui.selectable_value(
                                &mut self.normalization,
                                Normalization::Arcsinh,
                                "Arcsinh",
                            );
                        });
                    if self.normalization != prev_norm {
                        changed = true;
                    }
                });

                if self.normalization == Normalization::Gamma {
                    ui.horizontal(|ui| {
                        ui.label("Gamma:");
                        let prev = self.gamma;
                        ui.add(
                            egui::DragValue::new(&mut self.gamma)
                                .speed(0.1)
                                // silx _gammaSpinBox.setRange(0.01, 100.0)
                                // (ColormapDialog.py:947).
                                .range(0.01..=100.0),
                        );
                        if self.gamma != prev {
                            changed = true;
                        }
                    });
                }

                // NaN color picker (silx Colormap.setNaNColor): the RGBA shown
                // for Not-A-Number samples. The picker round-trips through an
                // egui Color32 (unmultiplied sRGBA) so the stored bytes match the
                // colormap's `nan_color` exactly.
                ui.horizontal(|ui| {
                    ui.label("NaN color:");
                    let [r, g, b, a] = self.nan_color;
                    let mut color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        self.nan_color = color.to_array();
                        changed = true;
                    }
                });

                ui.separator();

                let prev_auto = self.autoscale;
                ui.checkbox(&mut self.autoscale, "Autoscale");
                if self.autoscale != prev_auto {
                    changed = true;
                }

                if self.autoscale {
                    ui.horizontal(|ui| {
                        ui.label("Mode:");
                        let prev_mode = self.autoscale_mode;
                        egui::ComboBox::from_id_salt("cmap_autoscale_mode")
                            .selected_text(self.autoscale_mode.label())
                            .show_ui(ui, |ui| {
                                for mode in AutoscaleMode::ALL {
                                    ui.selectable_value(
                                        &mut self.autoscale_mode,
                                        mode,
                                        mode.label(),
                                    );
                                }
                            });
                        if self.autoscale_mode != prev_mode {
                            changed = true;
                        }
                    });

                    if self.autoscale_mode == AutoscaleMode::Percentile {
                        ui.horizontal(|ui| {
                            ui.label("Percentiles:");
                            let (prev_lo, prev_hi) = self.percentiles;
                            ui.add(
                                egui::DragValue::new(&mut self.percentiles.0)
                                    .prefix("Low: ")
                                    .speed(0.5)
                                    .range(0.0..=100.0),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.percentiles.1)
                                    .prefix("High: ")
                                    .speed(0.5)
                                    .range(0.0..=100.0),
                            );
                            if self.percentiles.0 != prev_lo || self.percentiles.1 != prev_hi {
                                changed = true;
                            }
                        });
                    }

                    ui.add_enabled(false, egui::DragValue::new(&mut self.vmin).prefix("Min: "));
                    ui.add_enabled(false, egui::DragValue::new(&mut self.vmax).prefix("Max: "));
                } else {
                    let prev_vmin = self.vmin;
                    let prev_vmax = self.vmax;
                    ui.add(
                        egui::DragValue::new(&mut self.vmin)
                            .prefix("Min: ")
                            .speed(0.1),
                    );
                    ui.add(
                        egui::DragValue::new(&mut self.vmax)
                            .prefix("Max: ")
                            .speed(0.1),
                    );
                    if self.vmin != prev_vmin || self.vmax != prev_vmax {
                        changed = true;
                    }
                }

                // Data-distribution histogram behind the colormap range (silx
                // ColormapDialog histogram mode). Only shown when a histogram is
                // available (user-set or auto-derived from the active image).
                if self.histogram.is_some() {
                    ui.separator();
                    self.draw_histogram_panel(ui, self.vmin, self.vmax);
                }
            });

        self.win.apply_signals(&signals, &mut self.open);

        if changed {
            self.apply(plot);
        }
    }

    /// The autoscale `(vmin, vmax)` this dialog applies over `pixels` for its
    /// current mode, percentiles, and normalization (silx `Colormap` autoscale
    /// via the `ColormapDialog`-fed histogram; the range dispatches per
    /// normalizer — `_computeAutoscaleRange`, colors.py:682-692, so e.g. a log
    /// dialog autoscales to `(min positive, max)`). Split out so the
    /// mode/percentile/normalization selection is testable without a
    /// GPU-backed [`Plot2D`]; [`Self::apply`] feeds it the active image's raw
    /// pixels.
    pub(crate) fn autoscale_range(&self, pixels: &[f64]) -> (f64, f64) {
        self.autoscale_mode
            .range(self.normalization, pixels, self.percentiles)
    }

    /// The autoscale `(vmin, vmax)` for the active image — the single data source
    /// shared by the autoscale path and the per-side invalid-bound repair (R2-41,
    /// silx `getColormapRange`'s `normalizer.autoscale`). Autoscales from the raw
    /// scalar pixels so every mode uses the data distribution — MinMax, Stddev3
    /// (mean ± 3·std clamped to the data range), and Percentile (the dialog's
    /// percentile pair) all via the shared `AutoscaleMode::range` (silx
    /// ColormapDialog's setHistogram-fed autoscale, ColormapDialog.py:240-280).
    /// Falls back to the aggregated image stats min/max (== MinMax) when the
    /// active item has no retained scalar pixels (e.g. an RGBA image), and to
    /// `[0, 1]` when there is no image at all.
    fn autoscale_from_plot(&self, plot: &mut Plot2D) -> (f64, f64) {
        if let Some(pixels) = plot.get_image_pixels_raw() {
            self.autoscale_range(&pixels)
        } else if let Some(&handle) = plot.get_all_images().first()
            && let Some(stats) = plot.image_stats(handle)
            && let Some(scalar) = &stats.scalar
            && let (Some(smin), Some(smax)) = (scalar.min, scalar.max)
        {
            (smin, smax)
        } else {
            (0.0, 1.0)
        }
    }

    /// The per-side repair math of silx `getColormapRange` (colors.py:723-750):
    /// given the explicit bounds `(vmin, vmax)` and the data-driven autoscale
    /// `(fmin, fmax)`, keep each bound that is valid under `norm` and replace each
    /// invalid one, preserving `vmin <= vmax` (`vmin2 = min(fmin, vmax)` when only
    /// vmin is invalid, `vmax2 = max(fmax, vmin2)` when vmax is invalid — the
    /// latter is silx's "handle max <= 0 for log scale" clamp). Pure so the repair
    /// is testable without a GPU-backed [`Plot2D`].
    pub(crate) fn repair_range(
        norm: Normalization,
        vmin: f64,
        vmax: f64,
        fmin: f64,
        fmax: f64,
    ) -> (f64, f64) {
        let vmin_valid = norm.is_valid_autoscale_value(vmin);
        let vmax_valid = norm.is_valid_autoscale_value(vmax);
        let vmin2 = if vmin_valid {
            vmin
        } else if vmax_valid {
            fmin.min(vmax)
        } else {
            fmin
        };
        let vmax2 = if vmax_valid { vmax } else { fmax.max(vmin2) };
        (vmin2, vmax2)
    }

    /// Resolve the effective `(vmin, vmax)` for an explicit (autoscale-off) range,
    /// repairing a bound that is invalid under the current normalization — silx
    /// `getColormapRange` (colors.py:711-750): a bound failing `is_valid` (e.g.
    /// `vmin <= 0` under Log) is treated as autoscale *for that side only*, so an
    /// invalid entry recovers to a valid data-driven bound instead of collapsing
    /// the render (`cmap_one_over_range == 0` mapping everything to the low color).
    /// Both-valid short-circuits without touching the data; otherwise the data
    /// autoscale feeds [`Self::repair_range`].
    fn resolve_explicit_range(&self, plot: &mut Plot2D) -> (f64, f64) {
        if self.normalization.is_valid_autoscale_value(self.vmin)
            && self.normalization.is_valid_autoscale_value(self.vmax)
        {
            return (self.vmin, self.vmax);
        }
        let (fmin, fmax) = self.autoscale_from_plot(plot);
        Self::repair_range(self.normalization, self.vmin, self.vmax, fmin, fmax)
    }

    /// Re-calculate and apply the colormap to the plot.
    pub fn apply(&self, plot: &mut Plot2D) {
        let (final_vmin, final_vmax) = if self.autoscale {
            self.autoscale_from_plot(plot)
        } else {
            self.resolve_explicit_range(plot)
        };

        plot.set_default_colormap(self.build_colormap(final_vmin, final_vmax));
    }

    /// Build the [`Colormap`] for the dialog's current settings over
    /// `[vmin, vmax]`, carrying the chosen name, normalization, gamma, and NaN
    /// color (silx `Colormap` with `setNaNColor`). Pure so the colormap wiring
    /// is testable without a GPU-backed [`Plot2D`]; [`Self::apply`] computes the
    /// effective range and delegates here.
    fn build_colormap(&self, vmin: f64, vmax: f64) -> Colormap {
        Colormap::new(self.name, vmin, vmax)
            .with_normalization(self.normalization)
            .with_gamma(self.gamma)
            .with_nan_color(self.nan_color)
    }

    /// Draw the data-distribution histogram (silx ColormapDialog histogram
    /// mode): normalized gray bars over the data range, a colormap gradient
    /// strip across the current `[vmin, vmax]` span (clamped outside), and
    /// vmin/vmax markers. A no-op without a histogram or with a degenerate data
    /// range. Counts are normalized to the bin maximum, matching silx
    /// `histogram / nanmax(histogram)`.
    fn draw_histogram_panel(&self, ui: &mut egui::Ui, vmin: f64, vmax: f64) {
        let Some((counts, edges)) = &self.histogram else {
            return;
        };
        if counts.is_empty() || edges.len() != counts.len() + 1 {
            return;
        }
        // silx clips leading bins whose lower edge is invalid under the
        // current normalization before displaying (a user-set histogram can
        // carry log/sqrt-invalid bins; the auto-computed one is already
        // range-clipped, so its first valid bin is 0).
        let Some(first) = first_valid_bin(edges, counts.len(), self.normalization) else {
            return; // every bin invalid: nothing to display (silx (None, None))
        };
        let (counts, edges) = (&counts[first..], &edges[first..]);
        let dmin = edges[0];
        let dmax = edges[edges.len() - 1];
        let dspan = dmax - dmin;
        let maxc = counts.iter().copied().max().unwrap_or(0);
        // Degenerate (all-equal) data has zero-width edges: nothing to map.
        if dspan <= 0.0 || maxc == 0 {
            return;
        }

        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 70.0), egui::Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter_at(rect);

        // The bottom strip carries the colormap gradient; the bars fill the rest.
        let strip_h = 8.0;
        let bars_bottom = rect.bottom() - strip_h - 2.0;
        let bars_h = (bars_bottom - rect.top()).max(1.0);
        let x_of =
            |value: f64| -> f32 { rect.left() + ((value - dmin) / dspan) as f32 * rect.width() };

        // Normalized gray bars (silx histogram fill="gray").
        let bar_color = egui::Color32::from_rgba_unmultiplied(160, 160, 160, 200);
        for (i, &c) in counts.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let x0 = x_of(edges[i]);
            let x1 = x_of(edges[i + 1]);
            let h = (c as f32 / maxc as f32) * bars_h;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x0, bars_bottom - h),
                    egui::pos2(x1.max(x0 + 1.0), bars_bottom),
                ),
                0.0,
                bar_color,
            );
        }

        // Colormap gradient strip across [vmin, vmax], clamped outside the range.
        let cmap = self.build_colormap(vmin, vmax);
        let cspan = (vmax - vmin).max(f64::MIN_POSITIVE);
        let strip_top = rect.bottom() - strip_h;
        let n = 64usize;
        for s in 0..n {
            let fx0 = rect.left() + (s as f32 / n as f32) * rect.width();
            let fx1 = rect.left() + ((s + 1) as f32 / n as f32) * rect.width();
            let value = dmin + ((s as f64 + 0.5) / n as f64) * dspan;
            let frac = ((value - vmin) / cspan).clamp(0.0, 1.0);
            let col = cmap.lut[((frac * 255.0).round() as usize).min(255)];
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(fx0, strip_top),
                    egui::pos2(fx1 + 0.5, rect.bottom()),
                ),
                0.0,
                egui::Color32::from_rgb(col[0], col[1], col[2]),
            );
        }

        // vmin / vmax markers where they fall within the data range.
        let marker = egui::Stroke::new(1.0, ui.visuals().text_color());
        for v in [vmin, vmax] {
            if v >= dmin && v <= dmax {
                let x = x_of(v);
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    marker,
                );
            }
        }
    }
}

/// The auto-histogram data range for a normalization — silx
/// `_computeNormalizedDataRange` (ColormapDialog.py:445-476): `Linear`/`Gamma`
/// use the finite `(min, max)`; `Log` and `Sqrt` clip the lower bound to the
/// smallest strictly-positive finite value (silx
/// `min_max(data, min_positive=True)` → `(minimum, min_positive, maximum)`,
/// :1208-1214, indexed `[1], [2]` for both LOG and SQRT). Returns `None` when
/// there is no finite value, or no positive one under `Log`/`Sqrt` (silx
/// `_computeNormalizedHistogram` bails when the range has a `None` bound).
fn normalized_data_range(data: &[f64], norm: Normalization) -> Option<(f64, f64)> {
    let mut min = f64::INFINITY;
    let mut min_positive = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in data {
        if !v.is_finite() {
            continue;
        }
        min = min.min(v);
        max = max.max(v);
        if v > 0.0 {
            min_positive = min_positive.min(v);
        }
    }
    if !max.is_finite() {
        return None;
    }
    let lo = match norm {
        Normalization::Log | Normalization::Sqrt => {
            if !min_positive.is_finite() {
                return None;
            }
            min_positive
        }
        // silx: `norm in (LINEAR, GAMMA, ARCSINH)` → (min, max) (:455-456).
        Normalization::Linear | Normalization::Gamma | Normalization::Arcsinh => min,
    };
    Some((lo, max))
}

/// The first displayable bin of a histogram under `norm` — silx
/// `_computeNormalizedHistogram`'s lower-edge validity clip
/// (ColormapDialog.py:409-418): a bin is kept when its LOWER edge satisfies
/// the normalizer's `is_valid` (`Log`: `> 0`, `Sqrt`: `>= 0`,
/// math/colormap.py:416-435; `Linear`/`Gamma`: always). Edges increase
/// monotonically, so everything from the first valid bin on is displayable;
/// `None` when every lower edge is invalid (silx returns `(None, None)`).
fn first_valid_bin(edges: &[f64], n_bins: usize, norm: Normalization) -> Option<usize> {
    let valid = |e: f64| match norm {
        Normalization::Log => e > 0.0,
        Normalization::Sqrt => e >= 0.0,
        // Arcsinh has the default all-valid `is_valid` in silx.
        Normalization::Linear | Normalization::Gamma | Normalization::Arcsinh => true,
    };
    edges.get(..n_bins)?.iter().position(|&e| valid(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Item 1: NaN color control ───────────────────────────────────────────

    #[test]
    fn nan_color_defaults_to_silx_transparent_white() {
        // silx Colormap._DEFAULT_NAN_COLOR = (255, 255, 255, 0).
        let dialog = ColormapDialog::new();
        assert_eq!(dialog.nan_color, [255, 255, 255, 0]);
    }

    #[test]
    fn picking_a_nan_color_feeds_the_built_colormap() {
        // The picker writes `self.nan_color`; the built colormap must carry it
        // (the egui color picker round-trips an unmultiplied sRGBA Color32).
        let mut dialog = ColormapDialog::new();
        let picked = egui::Color32::from_rgba_unmultiplied(10, 20, 30, 255);
        dialog.nan_color = picked.to_array();
        assert_eq!(dialog.nan_color, [10, 20, 30, 255]);

        let cmap = dialog.build_colormap(0.0, 1.0);
        assert_eq!(cmap.nan_color, [10, 20, 30, 255]);
    }

    #[test]
    fn with_colormap_carries_over_nan_color() {
        let source = Colormap::viridis(0.0, 1.0).with_nan_color([1, 2, 3, 4]);
        let dialog = ColormapDialog::new().with_colormap(&source);
        assert_eq!(dialog.nan_color, [1, 2, 3, 4]);
        assert_eq!(dialog.build_colormap(0.0, 1.0).nan_color, [1, 2, 3, 4]);
    }

    // ── Item 2: percentile bounds fields ────────────────────────────────────

    #[test]
    fn percentiles_default_to_silx_defaults() {
        let dialog = ColormapDialog::new();
        assert_eq!(dialog.percentiles, DEFAULT_PERCENTILES);
    }

    #[test]
    fn percentile_fields_round_trip_edited_values() {
        // The (low, high) DragValues are bound directly to `self.percentiles`;
        // editing them stores and returns the values verbatim.
        let mut dialog = ColormapDialog::new();
        dialog.autoscale = true;
        dialog.autoscale_mode = AutoscaleMode::Percentile;
        dialog.percentiles = (2.5, 97.5);
        assert_eq!(dialog.percentiles, (2.5, 97.5));
        // The chosen percentiles round-trip into the colormap's autoscale
        // percentiles via the public AutoscaleMode::range consumer (the dialog
        // stores them; the range computation in 6B-2 reads them back).
        let (lo, hi) = dialog.percentiles;
        let (rmin, rmax) = AutoscaleMode::Percentile.range(
            Normalization::Linear,
            &(0..=100).map(|i| i as f64).collect::<Vec<_>>(),
            (lo, hi),
        );
        // percentile 2.5 -> 2.5, 97.5 -> 97.5 over 0..=100 (numpy linear interp).
        assert!((rmin - 2.5).abs() < 1e-9, "rmin {rmin}");
        assert!((rmax - 97.5).abs() < 1e-9, "rmax {rmax}");
    }

    // ── Row 133: autoscale from raw pixels honors the selected mode ──────────

    #[test]
    fn autoscale_range_uses_selected_mode_not_always_minmax() {
        // Row 133 regression: the dialog must compute the autoscale range for
        // its CURRENT mode + percentiles over the raw pixels, not always fall
        // back to MinMax (which is what the aggregated-stats path produced).
        let data: Vec<f64> = (0..100).map(|i| i as f64).collect(); // 0..=99
        let mut dialog = ColormapDialog::new();
        dialog.autoscale = true;

        dialog.autoscale_mode = AutoscaleMode::MinMax;
        let minmax = dialog.autoscale_range(&data);
        assert_eq!(minmax, (0.0, 99.0));

        // Percentile (10, 90) is strictly tighter than min/max — a MinMax
        // fallback would instead equal `minmax`, so this proves the mode is
        // honored — and must match the public AutoscaleMode::range computation
        // with the dialog's percentiles.
        dialog.autoscale_mode = AutoscaleMode::Percentile;
        dialog.percentiles = (10.0, 90.0);
        let pct = dialog.autoscale_range(&data);
        assert_eq!(
            pct,
            AutoscaleMode::Percentile.range(Normalization::Linear, &data, (10.0, 90.0))
        );
        assert!(
            pct.0 > minmax.0 && pct.1 < minmax.1,
            "percentile {pct:?} must be tighter than minmax {minmax:?}"
        );

        // Stddev3 likewise routes through the public computation for the mode.
        dialog.autoscale_mode = AutoscaleMode::Stddev3;
        assert_eq!(
            dialog.autoscale_range(&data),
            AutoscaleMode::Stddev3.range(Normalization::Linear, &data, dialog.percentiles)
        );
    }

    #[test]
    fn autoscale_range_honors_the_dialog_normalization() {
        // R1-9: the dialog's autoscale is normalization-aware (silx
        // _computeAutoscaleRange, colors.py:682-692). Counting data with
        // zeros under a log dialog autoscales to (min positive, max), not to
        // a vmin of 0 that collapses the log mapping.
        let data = [0.0, 0.0, 0.5, 100.0];
        let mut dialog = ColormapDialog::new();
        dialog.autoscale = true;
        dialog.autoscale_mode = AutoscaleMode::MinMax;

        dialog.normalization = Normalization::Log;
        assert_eq!(dialog.autoscale_range(&data), (0.5, 100.0));

        dialog.normalization = Normalization::Linear;
        assert_eq!(dialog.autoscale_range(&data), (0.0, 100.0));
    }

    #[test]
    fn repair_range_keeps_a_valid_explicit_range_untouched() {
        // R2-41: both bounds valid under Log (both > 0) → explicit range as-is,
        // silx getColormapRange's early return (colors.py:723-724). The data
        // autoscale (fmin, fmax) is ignored.
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Log, 2.0, 50.0, 0.1, 999.0);
        assert_eq!((v0, v1), (2.0, 50.0));
    }

    #[test]
    fn repair_range_recovers_invalid_lower_bound_from_data_per_side() {
        // R2-41: vmin <= 0 is invalid under Log → that side becomes autoscale
        // (fmin), the valid vmax is kept, and vmin2 = min(fmin, vmax) preserves
        // ordering (silx colors.py:740-743).
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Log, 0.0, 50.0, 0.5, 999.0);
        assert_eq!((v0, v1), (0.5, 50.0));

        // fmin above the kept vmax clamps down to vmax so vmin2 <= vmax2.
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Log, -1.0, 10.0, 42.0, 999.0);
        assert_eq!((v0, v1), (10.0, 10.0));
    }

    #[test]
    fn repair_range_recovers_invalid_upper_bound_with_max_clamp() {
        // R2-41: vmax invalid under Log → vmax2 = max(fmax, vmin2), silx's
        // "handle max <= 0 for log scale" clamp so the upper bound never sinks
        // below the (kept) lower bound (colors.py:745-748).
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Log, 5.0, 0.0, 0.1, 100.0);
        assert_eq!((v0, v1), (5.0, 100.0));

        // fmax below the kept vmin clamps up to vmin.
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Log, 20.0, -3.0, 0.1, 8.0);
        assert_eq!((v0, v1), (20.0, 20.0));
    }

    #[test]
    fn repair_range_both_invalid_falls_back_to_full_autoscale() {
        // R2-41: both bounds invalid under Log → full data autoscale (fmin, fmax)
        // with the max clamp (colors.py:740-748).
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Log, 0.0, -1.0, 0.5, 100.0);
        assert_eq!((v0, v1), (0.5, 100.0));
    }

    #[test]
    fn repair_range_linear_norm_never_repairs() {
        // R2-41: Linear's is_valid is all-true, so even a zero/negative explicit
        // range is left untouched (silx LinearNormalization.is_valid == True).
        let (v0, v1) = ColormapDialog::repair_range(Normalization::Linear, -5.0, 0.0, 1.0, 2.0);
        assert_eq!((v0, v1), (-5.0, 0.0));
    }

    // ── Histogram display (wave 6): data model ───────────────────────────────

    #[test]
    fn set_get_clear_histogram_round_trips() {
        let mut dialog = ColormapDialog::new();
        assert!(dialog.histogram().is_none());
        assert!(!dialog.histogram_user_set);

        dialog.set_histogram(vec![1, 2, 3], vec![0.0, 1.0, 2.0, 3.0]);
        assert!(dialog.histogram_user_set);
        let (counts, edges) = dialog.histogram().expect("histogram set");
        assert_eq!(counts, &[1, 2, 3]);
        assert_eq!(edges, &[0.0, 1.0, 2.0, 3.0]);

        dialog.clear_histogram();
        assert!(dialog.histogram().is_none());
        assert!(!dialog.histogram_user_set);
    }

    // ── R2-24: normalization-dependent histogram range & validity ────────────

    #[test]
    fn normalized_range_clips_to_min_positive_for_log_and_sqrt() {
        // silx _computeNormalizedDataRange: LOG and SQRT both take
        // (min_positive, max) (ColormapDialog.py:455-459); LINEAR/GAMMA take
        // the full finite (min, max).
        let data = [-4.0, -1.0, 0.0, 0.5, 2.0, 8.0, f64::NAN];
        assert_eq!(
            normalized_data_range(&data, Normalization::Sqrt),
            Some((0.5, 8.0))
        );
        assert_eq!(
            normalized_data_range(&data, Normalization::Log),
            Some((0.5, 8.0))
        );
        assert_eq!(
            normalized_data_range(&data, Normalization::Linear),
            Some((-4.0, 8.0))
        );
        assert_eq!(
            normalized_data_range(&data, Normalization::Gamma),
            Some((-4.0, 8.0))
        );
        // No positive value: LOG/SQRT have no range; LINEAR still does.
        let negatives = [-3.0, -1.0, 0.0];
        assert_eq!(normalized_data_range(&negatives, Normalization::Sqrt), None);
        assert_eq!(normalized_data_range(&negatives, Normalization::Log), None);
        assert_eq!(
            normalized_data_range(&negatives, Normalization::Linear),
            Some((-3.0, 0.0))
        );
        // No finite value at all: no range for any normalization.
        assert_eq!(
            normalized_data_range(&[f64::NAN], Normalization::Linear),
            None
        );
    }

    #[test]
    fn first_valid_bin_applies_the_normalizer_validity_to_lower_edges() {
        // silx _computeNormalizedHistogram clips a user histogram to the bins
        // whose LOWER edge is valid (ColormapDialog.py:409-418); Log needs
        // > 0, Sqrt >= 0 (is_valid, math/colormap.py:416-435).
        let edges = [-1.0, 0.0, 1.0, 2.0];
        assert_eq!(first_valid_bin(&edges, 3, Normalization::Log), Some(2));
        assert_eq!(first_valid_bin(&edges, 3, Normalization::Sqrt), Some(1));
        assert_eq!(first_valid_bin(&edges, 3, Normalization::Linear), Some(0));
        assert_eq!(first_valid_bin(&edges, 3, Normalization::Gamma), Some(0));
        // Every lower edge invalid -> nothing displayable (silx (None, None)).
        assert_eq!(
            first_valid_bin(&[-3.0, -2.0, -1.0], 2, Normalization::Log),
            None
        );
    }
}
