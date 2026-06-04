//! A table of per-ROI raw/net counts and raw/net area over the active curve,
//! mirroring silx [`CurvesROIWidget`]: one row per curve ROI with `From` / `To`
//! / `Raw Counts` / `Net Counts` / `Raw Area` / `Net Area`, computed by the pure
//! [`curve_roi_counts`] reduction in [`crate::widget::roi_stats`].
//!
//! The rows are filled by [`PlotWidget::feed_curves_roi_stats`] /
//! [`PlotWidget::show_curves_roi_widget`] from the active item's retained curve
//! data, so the table follows the active curve and the live ROI list. Only ROIs
//! with an `x`-span (silx's 1D `from`/`to` ROIs) appear; the widget itself only
//! renders the rows it was given, so the row-building is GPU-free and unit-tested
//! via [`PlotWidget::feed_curves_roi_stats`]'s helper.
//!
//! [`CurvesROIWidget`]: https://www.silx.org/doc/silx/latest/modules/gui/plot/curvesroiwidget.html
//! [`curve_roi_counts`]: crate::widget::roi_stats::curve_roi_counts
//! [`PlotWidget::feed_curves_roi_stats`]: crate::widget::high_level::PlotWidget::feed_curves_roi_stats
//! [`PlotWidget::show_curves_roi_widget`]: crate::widget::high_level::PlotWidget::show_curves_roi_widget

use crate::widget::roi_stats::CurveRoiCounts;
use crate::widget::stats_widget::format_stat;

/// One curve ROI's row: a display label, its inclusive `x`-span (`from`, `to`),
/// and the raw/net counts and area reduced over the active curve inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct CurveRoiRow {
    /// Display label for the ROI (its name, or `ROI {index}` when unnamed).
    pub label: String,
    /// Lower edge of the ROI's `x`-span (silx `From`).
    pub from: f64,
    /// Upper edge of the ROI's `x`-span (silx `To`).
    pub to: f64,
    /// Raw/net counts and area of the active curve inside the ROI.
    pub counts: CurveRoiCounts,
}

/// A per-ROI curve-statistics table (silx `CurvesROIWidget`).
///
/// Holds the rows computed from the active curve + ROI list and renders them as
/// a grid: `ROI | From | To | Raw Counts | Net Counts | Raw Area | Net Area`.
/// Fill it via [`PlotWidget::feed_curves_roi_stats`] (or render+fill in one call
/// with [`PlotWidget::show_curves_roi_widget`]); [`Self::ui`] draws the current
/// rows.
///
/// [`PlotWidget::feed_curves_roi_stats`]: crate::widget::high_level::PlotWidget::feed_curves_roi_stats
/// [`PlotWidget::show_curves_roi_widget`]: crate::widget::high_level::PlotWidget::show_curves_roi_widget
#[derive(Default)]
pub struct CurvesRoiWidget {
    rows: Vec<CurveRoiRow>,
}

impl CurvesRoiWidget {
    /// Create an empty curve-ROI table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current rows (one per curve ROI), as last filled.
    pub fn rows(&self) -> &[CurveRoiRow] {
        &self.rows
    }

    /// Replace the rows shown by the table (called by
    /// [`PlotWidget::feed_curves_roi_stats`]).
    ///
    /// [`PlotWidget::feed_curves_roi_stats`]: crate::widget::high_level::PlotWidget::feed_curves_roi_stats
    pub fn set_rows(&mut self, rows: Vec<CurveRoiRow>) {
        self.rows = rows;
    }

    /// Render the curve-ROI table. Columns: `ROI | From | To | Raw Counts | Net
    /// Counts | Raw Area | Net Area`; numeric cells use the shared
    /// [`format_stat`] formatting (silx-style significant digits).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("curves_roi_grid")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("ROI");
                ui.strong("From");
                ui.strong("To");
                ui.strong("Raw Counts");
                ui.strong("Net Counts");
                ui.strong("Raw Area");
                ui.strong("Net Area");
                ui.end_row();

                for row in &self.rows {
                    ui.label(&row.label);
                    ui.label(format_stat(Some(row.from)));
                    ui.label(format_stat(Some(row.to)));
                    ui.label(format_stat(Some(row.counts.raw_counts)));
                    ui.label(format_stat(Some(row.counts.net_counts)));
                    ui.label(format_stat(Some(row.counts.raw_area)));
                    ui.label(format_stat(Some(row.counts.net_area)));
                    ui.end_row();
                }
            });
    }
}
