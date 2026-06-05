//! ROI manager: a detached window that edits the plot's regions of interest —
//! each ROI's color, name, current/highlighted state, line width/style, and
//! fill — directly on the plot's single ROI collection, mirroring silx
//! `RegionOfInterestManager` / `RegionOfInterestTableWidget` (`tools/roi.py`).
//!
//! The ROIs themselves (geometry + appearance, [`ManagedRoi`]) live on the
//! [`Plot`](crate::core::plot::Plot) as the single source of truth and are
//! rendered by the live plot, so edits here appear on the plot immediately.
//! This widget owns no ROI state — only its window's open/position — so there
//! is one ROI collection, not two.

use crate::core::roi::Roi;
// `ManagedRoi`/`RoiLineStyle` live in `core::roi` (the geometry's home) so
// `Plot::rois` can own them; re-exported here to keep the
// `widget::roi_manager::{ManagedRoi, RoiLineStyle}` import path valid.
pub use crate::core::roi::{ManagedRoi, RoiLineStyle};
use crate::widget::high_level::Plot2D;

/// A detached window that edits the plot's ROIs (silx `RegionOfInterestManager`
/// table). Stateless beyond its window: every control mutates the plot's single
/// [`ManagedRoi`] collection, so changes render on the plot at once.
pub struct RoiManagerWidget {
    win: crate::widget::detached::DetachedWindow,
    /// Whether the detached manager window is shown.
    pub open: bool,
}

impl Default for RoiManagerWidget {
    fn default() -> Self {
        Self {
            win: crate::widget::detached::DetachedWindow::new(
                egui::Id::new("roi_manager_widget"),
                egui::vec2(320.0, 360.0),
            ),
            open: false,
        }
    }
}

impl RoiManagerWidget {
    /// Create a new ROI Manager Widget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Show the ROI Manager floating window: a row per plot ROI (current-ROI
    /// radio, color swatch, name field, line width/style, fill toggle, remove
    /// button), buttons to add each ROI kind centered on the plot view, and a
    /// clear-all button (silx `RegionOfInterestTableWidget` /
    /// `RegionOfInterestManager`). Every control edits `plot`'s ROIs directly.
    pub fn show(&mut self, ctx: &egui::Context, plot: &mut Plot2D) {
        if !self.open {
            return;
        }
        let pos = self.win.position(ctx);
        let id = self.win.id();
        let size = self.win.size();
        let signals =
            crate::widget::detached::show_detached(ctx, id, "ROI Manager", size, pos, |ui| {
                self.ui(ui, plot);
            });
        self.win.apply_signals(&signals, &mut self.open);
    }

    /// Render the manager controls into `ui`, editing `plot`'s ROIs. Seeds new
    /// ROIs at the center of the plot's current view.
    pub fn ui(&mut self, ui: &mut egui::Ui, plot: &mut Plot2D) {
        let mut remove_idx: Option<usize> = None;
        let mut make_current: Option<usize> = None;
        // Per-row color fallback and the current selection, read before the
        // mutable row loop (the Plot owns both).
        let default_color = plot.plot().roi_color;
        let current = plot.current_roi();

        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                for (i, r) in plot.rois_mut().iter_mut().enumerate() {
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
                        // Per-ROI color (defaults to the plot's ROI color).
                        let mut color = r.color.unwrap_or(default_color);
                        if ui.color_edit_button_srgba(&mut color).changed() {
                            r.color = Some(color);
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut r.name)
                                .desired_width(70.0)
                                .hint_text("name"),
                        );
                        // Line width (silx setLineWidth).
                        ui.add(
                            egui::DragValue::new(&mut r.line_width)
                                .speed(0.1)
                                .range(0.1..=20.0)
                                .prefix("w "),
                        )
                        .on_hover_text("Line width");
                        // Line style (silx setLineStyle).
                        egui::ComboBox::from_id_salt(("roi_line_style", i))
                            .selected_text(r.line_style.label())
                            .width(36.0)
                            .show_ui(ui, |ui| {
                                for s in [
                                    RoiLineStyle::Solid,
                                    RoiLineStyle::Dashed,
                                    RoiLineStyle::Dotted,
                                ] {
                                    ui.selectable_value(&mut r.line_style, s, s.label());
                                }
                            });
                        // Fill toggle (silx setFill).
                        ui.checkbox(&mut r.fill, "fill")
                            .on_hover_text("Fill interior");
                        if ui.small_button("×").on_hover_text("Remove").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
            });

        if let Some(i) = make_current {
            // Toggle: clicking the current ROI's radio clears the selection.
            let next = if current == Some(i) { None } else { Some(i) };
            plot.set_current_roi(next);
        }
        if let Some(idx) = remove_idx {
            plot.remove_roi(idx);
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
                plot.add_roi(Roi::Rect {
                    x: (cx - dx, cx + dx),
                    y: (cy - dy, cy + dy),
                });
            }
            if ui.button("+ HRange").clicked() {
                plot.add_roi(Roi::HRange {
                    y: (cy - dy, cy + dy),
                });
            }
            if ui.button("+ VRange").clicked() {
                plot.add_roi(Roi::VRange {
                    x: (cx - dx, cx + dx),
                });
            }
            if ui.button("+ Point").clicked() {
                plot.add_roi(Roi::Point { x: cx, y: cy });
            }
            if ui.button("+ Cross").clicked() {
                plot.add_roi(Roi::Cross { center: (cx, cy) });
            }
            if ui.button("+ Line").clicked() {
                plot.add_roi(Roi::Line {
                    start: (cx - dx, cy),
                    end: (cx + dx, cy),
                });
            }
            if ui.button("+ Circle").clicked() {
                plot.add_roi(Roi::Circle {
                    center: (cx, cy),
                    radius: dx.abs().max(dy.abs()),
                });
            }
            if ui.button("+ Ellipse").clicked() {
                plot.add_roi(Roi::Ellipse {
                    center: (cx, cy),
                    radii: (dx.abs(), dy.abs()),
                    orientation: 0.0,
                });
            }
            if ui.button("+ Arc").clicked() {
                // A quarter-ring centered on the view (silx ArcROI).
                let r = dx.abs().max(dy.abs()).max(f64::EPSILON);
                plot.add_roi(Roi::Arc {
                    center: (cx, cy),
                    inner_radius: r * 0.5,
                    outer_radius: r,
                    start_angle: 0.0,
                    end_angle: std::f64::consts::FRAC_PI_2,
                });
            }
            if ui.button("+ Band").clicked() {
                plot.add_roi(Roi::Band {
                    begin: (cx - dx, cy),
                    end: (cx + dx, cy),
                    width: dy.abs(),
                });
            }
        });

        if !plot.rois().is_empty() && ui.button("Clear all").clicked() {
            plot.clear_rois();
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
        Roi::Arc { .. } => "arc",
        Roi::Band { .. } => "band",
    }
}
