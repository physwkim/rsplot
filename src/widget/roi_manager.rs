//! ROI manager: tracks a list of regions of interest together with their
//! per-ROI metadata (color, name, selection) and the "current" ROI, mirroring
//! silx `RegionOfInterestManager` (`tools/roi.py`).
//!
//! The geometry [`Roi`] is kept pure; the color / name / selection that silx
//! stores on each `RegionOfInterest` live here on [`ManagedRoi`] instead, so the
//! geometry enum stays a clean value type. Drawing of a managed ROI (with its
//! color, a name label, and a thicker outline when selected) goes through
//! [`chrome::draw_roi`](crate::widget::chrome::draw_roi).

use egui::{Color32, Window};

use crate::core::roi::Roi;
use crate::core::transform::Transform;
use crate::widget::chrome::{self, RoiAppearance, Style};
use crate::widget::high_level::Plot2D;

/// silx `RegionOfInterestManager._color` default (`rgba("red")`).
const DEFAULT_ROI_COLOR: Color32 = Color32::RED;

/// A region of interest plus the metadata silx keeps on its `RegionOfInterest`:
/// an optional per-ROI color (falls back to the manager default, silx
/// `useManagerColor`), a display name (silx `getName`/`setName`), and whether
/// it is currently selected/highlighted (silx `setHighlighted`).
#[derive(Clone, Debug, PartialEq)]
pub struct ManagedRoi {
    /// Pure geometry of the region of interest.
    pub roi: Roi,
    /// Per-ROI color override; `None` uses the manager's default color.
    pub color: Option<Color32>,
    /// Display name (may be empty).
    pub name: String,
    /// Whether this ROI is the highlighted/current one.
    pub selected: bool,
}

impl ManagedRoi {
    /// Wrap `roi` with default metadata: no color override, empty name, not
    /// selected.
    pub fn new(roi: Roi) -> Self {
        Self {
            roi,
            color: None,
            name: String::new(),
            selected: false,
        }
    }
}

/// A dedicated widget to track and manage multiple ROIs drawn on a plot.
///
/// Holds the authoritative list of [`ManagedRoi`] and the current-ROI index,
/// mirroring silx `RegionOfInterestManager`. The geometry can be drawn over a
/// plot with [`Self::draw`].
pub struct RoiManagerWidget {
    window_id: egui::Id,
    /// Whether the floating manager window is shown.
    pub open: bool,
    rois: Vec<ManagedRoi>,
    /// Index of the current ROI, or `None`.
    current: Option<usize>,
    /// Default color applied to ROIs without an explicit color (silx
    /// `RegionOfInterestManager.getColor`/`setColor`, default red).
    default_color: Color32,
}

impl Default for RoiManagerWidget {
    fn default() -> Self {
        Self {
            window_id: egui::Id::new("roi_manager_widget"),
            open: false,
            rois: Vec::new(),
            current: None,
            default_color: DEFAULT_ROI_COLOR,
        }
    }
}

impl RoiManagerWidget {
    /// Create a new ROI Manager Widget.
    pub fn new() -> Self {
        Self::default()
    }

    /// The managed ROIs.
    pub fn rois(&self) -> &[ManagedRoi] {
        &self.rois
    }

    /// Mutable access to the managed ROIs.
    pub fn rois_mut(&mut self) -> &mut [ManagedRoi] {
        &mut self.rois
    }

    /// The manager's default ROI color (silx `getColor`).
    pub fn default_color(&self) -> Color32 {
        self.default_color
    }

    /// Set the manager's default ROI color (silx `setColor`). Existing ROIs are
    /// not affected, matching silx.
    pub fn set_default_color(&mut self, color: Color32) {
        self.default_color = color;
    }

    /// Append a ROI with default metadata and return its index.
    pub fn add_roi(&mut self, roi: Roi) -> usize {
        self.rois.push(ManagedRoi::new(roi));
        self.rois.len() - 1
    }

    /// Append a fully-specified managed ROI and return its index.
    pub fn add_managed(&mut self, managed: ManagedRoi) -> usize {
        self.rois.push(managed);
        self.rois.len() - 1
    }

    /// Remove the ROI at `index`. Adjusts the current-ROI index so it keeps
    /// pointing at the same ROI (or clears it when the current one is removed),
    /// mirroring silx `removeRoi` clearing the current ROI it removes.
    pub fn remove_roi(&mut self, index: usize) {
        if index >= self.rois.len() {
            return;
        }
        self.rois.remove(index);
        self.current = match self.current {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };
        self.sync_selection();
    }

    /// Remove all ROIs and clear the current selection.
    pub fn clear(&mut self) {
        self.rois.clear();
        self.current = None;
    }

    /// The index of the current ROI (silx `getCurrentRoi`).
    pub fn current_roi(&self) -> Option<usize> {
        self.current
    }

    /// Set the current ROI by index, or `None` to clear it (silx
    /// `setCurrentRoi`): the previous current ROI loses its highlight and the
    /// new one gains it. An out-of-range index clears the selection.
    pub fn set_current_roi(&mut self, index: Option<usize>) {
        self.current = match index {
            Some(i) if i < self.rois.len() => Some(i),
            _ => None,
        };
        self.sync_selection();
    }

    /// Mirror the current-ROI index onto each ROI's `selected` flag so exactly
    /// the current ROI is highlighted (silx highlights only the current ROI).
    fn sync_selection(&mut self) {
        for (i, r) in self.rois.iter_mut().enumerate() {
            r.selected = Some(i) == self.current;
        }
    }

    /// Draw every managed ROI over the data area with its color, name label, and
    /// (for the current one) a thicker outline, via [`chrome::draw_roi`].
    pub fn draw(&self, painter: &egui::Painter, t: &Transform, style: &Style) {
        for r in &self.rois {
            let appearance = RoiAppearance {
                color: Some(r.color.unwrap_or(self.default_color)),
                name: if r.name.is_empty() {
                    None
                } else {
                    Some(r.name.as_str())
                },
                selected: r.selected,
            };
            chrome::draw_roi(painter, t, &r.roi, &appearance, style);
        }
    }

    /// Show the ROI Manager floating window: a list of the managed ROIs with a
    /// per-row name field, color swatch, select (current) and remove controls,
    /// buttons to add each ROI kind centered on the plot view, and a clear-all
    /// button (silx `RegionOfInterestTableWidget` / `RegionOfInterestManager`).
    pub fn show(&mut self, ctx: &egui::Context, plot: &mut Plot2D) {
        let mut open = self.open;
        Window::new("ROI Manager")
            .id(self.window_id)
            .open(&mut open)
            .resizable(true)
            .min_width(240.0)
            .show(ctx, |ui| {
                self.ui(ui, plot);
            });
        self.open = open;
    }

    /// Render the manager controls into `ui`. Seeds new ROIs at the center of
    /// the plot's current view.
    pub fn ui(&mut self, ui: &mut egui::Ui, plot: &Plot2D) {
        let mut remove_idx: Option<usize> = None;
        let mut make_current: Option<usize> = None;
        let default_color = self.default_color;

        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                for (i, r) in self.rois.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        // Current-ROI radio: selecting one deselects the rest.
                        if ui
                            .radio(r.selected, "")
                            .on_hover_text("Make current")
                            .clicked()
                        {
                            make_current = Some(i);
                        }
                        ui.label(roi_kind(&r.roi));
                        // Per-ROI color (defaults to the manager color).
                        let mut color = r.color.unwrap_or(default_color);
                        if ui.color_edit_button_srgba(&mut color).changed() {
                            r.color = Some(color);
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut r.name)
                                .desired_width(90.0)
                                .hint_text("name"),
                        );
                        if ui.small_button("×").on_hover_text("Remove").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
            });

        if let Some(i) = make_current {
            // Toggle: clicking the current ROI's radio clears the selection.
            let next = if self.current == Some(i) {
                None
            } else {
                Some(i)
            };
            self.set_current_roi(next);
        }
        if let Some(idx) = remove_idx {
            self.remove_roi(idx);
        }

        // Seed new ROIs at the center of the current view.
        let (x0, x1) = plot.x_limits();
        let (y0, y1) = plot
            .y_limits(crate::core::transform::YAxis::Left)
            .unwrap_or((0.0, 1.0));
        let cx = (x0 + x1) * 0.5;
        let cy = (y0 + y1) * 0.5;
        let dx = (x1 - x0) * 0.2;
        let dy = (y1 - y0) * 0.2;

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Rect").clicked() {
                self.add_roi(Roi::Rect {
                    x: (cx - dx, cx + dx),
                    y: (cy - dy, cy + dy),
                });
            }
            if ui.button("+ HRange").clicked() {
                self.add_roi(Roi::HRange {
                    y: (cy - dy, cy + dy),
                });
            }
            if ui.button("+ VRange").clicked() {
                self.add_roi(Roi::VRange {
                    x: (cx - dx, cx + dx),
                });
            }
            if ui.button("+ Point").clicked() {
                self.add_roi(Roi::Point { x: cx, y: cy });
            }
            if ui.button("+ Cross").clicked() {
                self.add_roi(Roi::Cross { center: (cx, cy) });
            }
            if ui.button("+ Line").clicked() {
                self.add_roi(Roi::Line {
                    start: (cx - dx, cy),
                    end: (cx + dx, cy),
                });
            }
            if ui.button("+ Circle").clicked() {
                self.add_roi(Roi::Circle {
                    center: (cx, cy),
                    radius: dx.abs().max(dy.abs()),
                });
            }
            if ui.button("+ Ellipse").clicked() {
                self.add_roi(Roi::Ellipse {
                    center: (cx, cy),
                    radii: (dx.abs(), dy.abs()),
                });
            }
        });

        if !self.rois.is_empty() && ui.button("Clear all").clicked() {
            self.clear();
        }
    }
}

/// A short human-readable kind label for a ROI, for the list UI (silx
/// `RegionOfInterest.SHORT_NAME`).
fn roi_kind(roi: &Roi) -> &'static str {
    match roi {
        Roi::Rect { .. } => "rectangle",
        Roi::HRange { .. } => "hrange",
        Roi::VRange { .. } => "vrange",
        Roi::Point { .. } => "point",
        Roi::Cross { .. } => "cross",
        Roi::Line { .. } => "line",
        Roi::Polygon { .. } => "polygon",
        Roi::Circle { .. } => "circle",
        Roi::Ellipse { .. } => "ellipse",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_current_highlights_only_one() {
        let mut m = RoiManagerWidget::new();
        m.add_roi(Roi::Point { x: 0.0, y: 0.0 });
        m.add_roi(Roi::Point { x: 1.0, y: 1.0 });
        m.add_roi(Roi::Point { x: 2.0, y: 2.0 });

        m.set_current_roi(Some(1));
        assert_eq!(m.current_roi(), Some(1));
        assert!(!m.rois()[0].selected);
        assert!(m.rois()[1].selected);
        assert!(!m.rois()[2].selected);

        // Switching the current ROI moves the highlight.
        m.set_current_roi(Some(2));
        assert!(!m.rois()[1].selected);
        assert!(m.rois()[2].selected);

        // Clearing removes all highlights.
        m.set_current_roi(None);
        assert_eq!(m.current_roi(), None);
        assert!(m.rois().iter().all(|r| !r.selected));
    }

    #[test]
    fn out_of_range_current_clears_selection() {
        let mut m = RoiManagerWidget::new();
        m.add_roi(Roi::Point { x: 0.0, y: 0.0 });
        m.set_current_roi(Some(5));
        assert_eq!(m.current_roi(), None);
    }

    #[test]
    fn remove_adjusts_current_index() {
        let mut m = RoiManagerWidget::new();
        for i in 0..3 {
            m.add_roi(Roi::Point {
                x: i as f64,
                y: 0.0,
            });
        }
        // Current after the removed index shifts down by one.
        m.set_current_roi(Some(2));
        m.remove_roi(0);
        assert_eq!(m.current_roi(), Some(1));
        assert!(m.rois()[1].selected);

        // Removing the current ROI clears the selection.
        m.set_current_roi(Some(1));
        m.remove_roi(1);
        assert_eq!(m.current_roi(), None);

        // Current before the removed index is unaffected.
        m.clear();
        for i in 0..3 {
            m.add_roi(Roi::Point {
                x: i as f64,
                y: 0.0,
            });
        }
        m.set_current_roi(Some(0));
        m.remove_roi(2);
        assert_eq!(m.current_roi(), Some(0));
    }

    #[test]
    fn color_falls_back_to_manager_default() {
        let mut m = RoiManagerWidget::new();
        assert_eq!(m.default_color(), Color32::RED);
        let idx = m.add_roi(Roi::Point { x: 0.0, y: 0.0 });
        assert_eq!(m.rois()[idx].color, None);
        m.set_default_color(Color32::GREEN);
        assert_eq!(m.default_color(), Color32::GREEN);
    }
}
