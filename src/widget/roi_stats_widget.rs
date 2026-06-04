//! A table of per-ROI statistics over a plot item, mirroring silx
//! [`ROIStatsWidget`]: one row per region of interest with count / min / max /
//! mean / sum / integral, computed by the pure reductions in
//! [`crate::widget::roi_stats`] (`image_roi_stats` / `curve_roi_stats`).
//!
//! The rows are filled by [`PlotWidget::feed_roi_stats`] /
//! [`PlotWidget::show_roi_stats_widget`] from the active item's retained data,
//! so the table follows the active image / curve and the live ROI list. The
//! widget itself only renders the rows it was given, so the
//! row-building is GPU-free and unit-tested via
//! [`PlotWidget::feed_roi_stats`]'s helper.
//!
//! [`ROIStatsWidget`]: https://www.silx.org/doc/silx/latest/modules/gui/plot/roistatswidget.html
//! [`PlotWidget::feed_roi_stats`]: crate::widget::high_level::PlotWidget::feed_roi_stats
//! [`PlotWidget::show_roi_stats_widget`]: crate::widget::high_level::PlotWidget::show_roi_stats_widget

use crate::widget::roi_stats::RoiStats;
use crate::widget::stats_widget::format_stat;

/// One ROI's row in the ROI-stats table: a display label plus the statistics
/// reduced over the item inside that ROI.
#[derive(Clone, Debug, PartialEq)]
pub struct RoiStatsRow {
    /// Display label for the ROI (its name, or `ROI {index}` when unnamed).
    pub label: String,
    /// Reduced statistics of the item's samples inside the ROI.
    pub stats: RoiStats,
}

/// A per-ROI statistics table (silx `ROIStatsWidget`).
///
/// Holds the rows computed from the active item + ROI list and renders them as
/// a grid: `ROI | N | min | max | mean | sum | integral`. Fill it via
/// [`PlotWidget::feed_roi_stats`] (or render+fill in one call with
/// [`PlotWidget::show_roi_stats_widget`]); [`Self::ui`] draws the current rows.
///
/// [`PlotWidget::feed_roi_stats`]: crate::widget::high_level::PlotWidget::feed_roi_stats
/// [`PlotWidget::show_roi_stats_widget`]: crate::widget::high_level::PlotWidget::show_roi_stats_widget
#[derive(Default)]
pub struct RoiStatsWidget {
    rows: Vec<RoiStatsRow>,
}

impl RoiStatsWidget {
    /// Create an empty ROI-stats table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current rows (one per ROI), as last filled.
    pub fn rows(&self) -> &[RoiStatsRow] {
        &self.rows
    }

    /// Replace the rows shown by the table (called by
    /// [`PlotWidget::feed_roi_stats`]).
    ///
    /// [`PlotWidget::feed_roi_stats`]: crate::widget::high_level::PlotWidget::feed_roi_stats
    pub fn set_rows(&mut self, rows: Vec<RoiStatsRow>) {
        self.rows = rows;
    }

    /// Render the ROI-stats table. Columns: `ROI | N | min | max | mean | sum |
    /// integral`; numeric cells use the shared [`format_stat`] formatting
    /// (silx-style significant digits, `-` for an empty selection).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("roi_stats_grid")
            .striped(true)
            .show(ui, |ui| {
                ui.strong("ROI");
                ui.strong("N");
                ui.strong("min");
                ui.strong("max");
                ui.strong("mean");
                ui.strong("sum");
                ui.strong("integral");
                ui.end_row();

                for row in &self.rows {
                    ui.label(&row.label);
                    ui.label(row.stats.count.to_string());
                    ui.label(format_stat(row.stats.min));
                    ui.label(format_stat(row.stats.max));
                    ui.label(format_stat(row.stats.mean));
                    ui.label(format_stat(Some(row.stats.sum)));
                    ui.label(format_stat(Some(row.stats.integral)));
                    ui.end_row();
                }
            });
    }
}
