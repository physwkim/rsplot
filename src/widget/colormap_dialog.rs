use crate::core::colormap::{
    AutoscaleMode, Colormap, ColormapName, DEFAULT_PERCENTILES, Normalization,
};
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

    /// Initialize the dialog from an existing Colormap.
    pub fn with_colormap(mut self, cmap: &Colormap) -> Self {
        self.vmin = cmap.vmin;
        self.vmax = cmap.vmax;
        self.normalization = cmap.normalization;
        self.gamma = cmap.gamma;
        self.nan_color = cmap.nan_color;
        self
    }

    /// Show the Colormap dialog. If it's open and modified, updates the plot in real-time.
    pub fn show(&mut self, ctx: &egui::Context, plot: &mut Plot2D) {
        if !self.open {
            return;
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
                                .range(0.1..=10.0),
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
            });

        self.win.apply_signals(&signals, &mut self.open);

        if changed {
            self.apply(plot);
        }
    }

    /// The autoscale `(vmin, vmax)` this dialog applies over `pixels` for its
    /// current mode and percentiles (silx `Colormap` autoscale via the
    /// `ColormapDialog`-fed histogram). MinMax = finite min/max, Stddev3 =
    /// mean ± 3·std clamped to the data range, Percentile = the dialog's
    /// `(low, high)` percentiles. Split out so the mode/percentile selection is
    /// testable without a GPU-backed [`Plot2D`]; [`Self::apply`] feeds it the
    /// active image's raw pixels.
    pub(crate) fn autoscale_range(&self, pixels: &[f64]) -> (f64, f64) {
        self.autoscale_mode.range(pixels, self.percentiles)
    }

    /// Re-calculate and apply the colormap to the plot.
    pub fn apply(&self, plot: &mut Plot2D) {
        let mut final_vmin = self.vmin;
        let mut final_vmax = self.vmax;

        if self.autoscale {
            // Autoscale from the active image's raw scalar pixels so every mode
            // uses the data distribution — MinMax, Stddev3 (mean ± 3·std clamped
            // to the data range), and Percentile (the dialog's percentile pair)
            // all via the shared AutoscaleMode::range (silx ColormapDialog's
            // setHistogram-fed autoscale, ColormapDialog.py:240-280). Falls back
            // to the aggregated image stats min/max (== MinMax) when the active
            // item has no retained scalar pixels (e.g. an RGBA image), and to
            // [0, 1] when there is no image at all.
            if let Some(pixels) = plot.get_image_pixels_raw() {
                let (vmin, vmax) = self.autoscale_range(&pixels);
                final_vmin = vmin;
                final_vmax = vmax;
            } else if let Some(&handle) = plot.get_all_images().first()
                && let Some(stats) = plot.image_stats(handle)
                && let Some(scalar) = &stats.scalar
                && let (Some(smin), Some(smax)) = (scalar.min, scalar.max)
            {
                final_vmin = smin;
                final_vmax = smax;
            } else {
                final_vmin = 0.0;
                final_vmax = 1.0;
            }
        }

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
        let (rmin, rmax) = AutoscaleMode::Percentile
            .range(&(0..=100).map(|i| i as f64).collect::<Vec<_>>(), (lo, hi));
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
        assert_eq!(pct, AutoscaleMode::Percentile.range(&data, (10.0, 90.0)));
        assert!(
            pct.0 > minmax.0 && pct.1 < minmax.1,
            "percentile {pct:?} must be tighter than minmax {minmax:?}"
        );

        // Stddev3 likewise routes through the public computation for the mode.
        dialog.autoscale_mode = AutoscaleMode::Stddev3;
        assert_eq!(
            dialog.autoscale_range(&data),
            AutoscaleMode::Stddev3.range(&data, dialog.percentiles)
        );
    }
}
