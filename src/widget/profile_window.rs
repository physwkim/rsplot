use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::plot::PlotId;
use crate::core::roi::Roi;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::{
    Plot1D, ProfileMethod, aligned_profile_values, line_profile_band, rect_profile_values,
};

/// Compute the `(x, y)` profile curve for `roi` over a row-major image,
/// integrating a band of `line_width` pixels and reducing it with `method`
/// (silx `ProfileToolButtons` line-width + mean/sum). Returns `None` for ROI
/// kinds that have no profile. Pure dispatch over the tested profile extractors:
///
/// - [`Roi::Line`] -> [`line_profile_band`] (bilinear band, silx
///   `BilinearImage.profile_line`).
/// - [`Roi::Rect`] -> [`rect_profile_values`] reduced along the columns.
/// - [`Roi::HRange`] / [`Roi::VRange`] -> [`aligned_profile_values`] centered on
///   the range's midpoint with `line_width` as the integration band (silx
///   `_alignedFullProfile`; `int(position)` placement). `line_width == 1`,
///   `Mean` reproduces the single-row/column average.
fn profile_for_roi(
    width: u32,
    height: u32,
    data: &[f32],
    roi: &Roi,
    line_width: u32,
    method: ProfileMethod,
) -> Option<(Vec<f64>, Vec<f64>)> {
    match roi {
        Roi::Line { start, end } => {
            line_profile_band(width, height, data, *start, *end, line_width, method).ok()
        }
        Roi::Rect { x, y } => {
            rect_profile_values(width, height, data, (x.0, x.1, y.0, y.1), true, method).ok()
        }
        Roi::HRange { y } => {
            let row = (y.0 + y.1) / 2.0;
            aligned_profile_values(width, height, data, row, line_width, true, method)
                .ok()
                .map(|y_vals| {
                    let x_vals: Vec<f64> = (0..width as usize).map(|i| i as f64).collect();
                    (x_vals, y_vals)
                })
        }
        Roi::VRange { x } => {
            let col = (x.0 + x.1) / 2.0;
            aligned_profile_values(width, height, data, col, line_width, false, method)
                .ok()
                .map(|y_vals| {
                    let x_vals: Vec<f64> = (0..height as usize).map(|i| i as f64).collect();
                    (x_vals, y_vals)
                })
        }
        _ => None,
    }
}

/// A window widget to display the 1D profile of an image based on an ROI.
pub struct ProfileWindow {
    plot: Plot1D,
    curve_handle: Option<ItemHandle>,
    window_id: egui::Id,
    open: bool,
    /// Band width in pixels for the profile integration (silx
    /// `ProfileToolButton` line width); `1` is a single-pixel line.
    line_width: u32,
    /// Band reduction: average (silx default) or sum (silx
    /// `ProfileOptionToolButton` method).
    method: ProfileMethod,
    /// Initial outer size of the profile viewport, in points. Reused for both
    /// the viewport builder and the "beside the main window" placement maths.
    size: egui::Vec2,
    /// Position chosen for the *current* open session. Computed once when the
    /// window opens and then left untouched so the user can freely drag it
    /// (re-passing an unchanged position never re-issues `OuterPosition`).
    placement: Option<egui::Pos2>,
    /// Last observed outer position of the profile viewport, restored as the
    /// initial placement on the next open — mirrors silx
    /// `ProfileManager._previousWindowGeometry`.
    remembered_pos: Option<egui::Pos2>,
}

impl ProfileWindow {
    /// Create a new ProfileWindow with a backing Plot1D.
    pub fn new(render_state: &RenderState, plot_id: PlotId) -> Self {
        let mut plot = Plot1D::new(render_state, plot_id);
        plot.set_graph_title("Profile");

        Self {
            plot,
            curve_handle: None,
            window_id: egui::Id::new(plot_id).with("profile_window"),
            open: false,
            line_width: 1,
            method: ProfileMethod::Mean,
            size: egui::vec2(420.0, 320.0),
            placement: None,
            remembered_pos: None,
        }
    }

    /// The current profile band width in pixels (silx `ProfileToolButton`).
    pub fn line_width(&self) -> u32 {
        self.line_width
    }

    /// Set the profile band width in pixels (clamped to at least 1).
    pub fn set_line_width(&mut self, width: u32) {
        self.line_width = width.max(1);
    }

    /// The current band reduction method (silx `ProfileOptionToolButton`).
    pub fn method(&self) -> ProfileMethod {
        self.method
    }

    /// Set the band reduction method (mean vs sum).
    pub fn set_method(&mut self, method: ProfileMethod) {
        self.method = method;
    }

    /// Is the window currently open?
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close the window.
    pub fn set_open(&mut self, open: bool) {
        // Closing forgets the current placement so the next open re-runs the
        // beside-the-main-window logic against the latest window position.
        if !open {
            self.placement = None;
        }
        self.open = open;
    }

    /// Re-calculate and update the profile curve based on the given ROI, using
    /// the current line width and reduction method.
    pub fn update_profile(&mut self, width: u32, height: u32, data: &[f32], roi: &Roi) {
        let profile = profile_for_roi(width, height, data, roi, self.line_width, self.method);

        if let Some((x, y)) = profile {
            if let Some(handle) = self.curve_handle {
                let curve = CurveData::new(x, y, Color32::YELLOW);
                self.plot.update_curve_data(handle, &curve);
            } else {
                self.curve_handle =
                    Some(
                        self.plot
                            .add_curve_with_legend(&x, &y, Color32::YELLOW, "profile"),
                    );
            }
            // Auto-scale limits based on data.
            self.plot.reset_zoom_to_data();
        }
    }

    /// Show the profile in its own native OS window (a separate egui viewport).
    ///
    /// Using a viewport instead of an [`egui::Window`] lets the profile be
    /// moved anywhere on the desktop, including outside the parent application
    /// window. When it first opens it is positioned *beside* the main window
    /// (preferring the right side, then the left, then the roomier screen
    /// edge) and vertically centred on it, so it does not cover the image —
    /// mirroring silx `ProfileManager.initProfileWindow`. After that the user
    /// can drag it anywhere, and the position is restored on the next open.
    ///
    /// On backends without multi-viewport support (Wayland, Android, web) egui
    /// transparently falls back to an embedded in-app window and the placement
    /// maths is skipped because the window position is not exposed.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        // Choose the initial position once per open session: restore the last
        // place the user left it, else sit beside the main window.
        if self.placement.is_none() {
            self.placement = self
                .remembered_pos
                .or_else(|| crate::widget::detached::beside_main_window(ctx, self.size));
        }

        let viewport_id = egui::ViewportId::from_hash_of(self.window_id);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Profile")
            .with_inner_size(self.size);
        if let Some(pos) = self.placement {
            builder = builder.with_position(pos);
        }

        let mut close_requested = false;
        let mut live_pos = None;
        ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
            // Line-width + method controls (silx ProfileToolButton / method
            // option). Edits take effect on the next `update_profile`, which the
            // host re-drives from the active ROI each frame.
            ui.horizontal(|ui| {
                ui.label("Width:");
                let mut width = self.line_width;
                if ui
                    .add(
                        egui::DragValue::new(&mut width)
                            .speed(1.0)
                            .range(1..=u32::MAX),
                    )
                    .on_hover_text("Profile band width in pixels")
                    .changed()
                {
                    self.set_line_width(width);
                }
                ui.separator();
                ui.label("Method:");
                egui::ComboBox::from_id_salt("profile_method")
                    .selected_text(match self.method {
                        ProfileMethod::Mean => "Mean",
                        ProfileMethod::Sum => "Sum",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.method, ProfileMethod::Mean, "Mean");
                        ui.selectable_value(&mut self.method, ProfileMethod::Sum, "Sum");
                    });
            });
            ui.separator();
            self.plot.show(ui);
            ui.ctx().input(|i| {
                let vp = i.viewport();
                if vp.close_requested() {
                    close_requested = true;
                }
                // Track where the user has moved the window so the next open
                // restores it (silx `_previousWindowGeometry`).
                live_pos = vp.outer_rect.map(|r| r.min);
            });
        });

        if let Some(pos) = live_pos {
            self.remembered_pos = Some(pos);
        }
        if close_requested {
            self.open = false;
            self.placement = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 3×3 ramp where value == row*10 + col, so band reductions are easy to
    // verify by hand.
    fn ramp_3x3() -> Vec<f32> {
        let mut v = Vec::with_capacity(9);
        for row in 0..3 {
            for col in 0..3 {
                v.push((row * 10 + col) as f32);
            }
        }
        v
    }

    #[test]
    fn profile_for_roi_hrange_width_and_method() {
        let data = ramp_3x3();
        // HRange centred on row 1: width 1, Mean -> just row 1 = [10, 11, 12].
        let (_x, y) = profile_for_roi(
            3,
            3,
            &data,
            &Roi::HRange { y: (1.0, 1.0) },
            1,
            ProfileMethod::Mean,
        )
        .unwrap();
        assert_eq!(y, vec![10.0, 11.0, 12.0]);

        // Width 3, Sum -> every column summed over all three rows:
        // col c -> (0+10+20) + c*3 = 30 + 3c = [30, 33, 36].
        let (_x, y) = profile_for_roi(
            3,
            3,
            &data,
            &Roi::HRange { y: (1.0, 1.0) },
            3,
            ProfileMethod::Sum,
        )
        .unwrap();
        assert_eq!(y, vec![30.0, 33.0, 36.0]);
    }

    #[test]
    fn profile_for_roi_returns_none_for_unsupported_kind() {
        let data = ramp_3x3();
        assert!(
            profile_for_roi(
                3,
                3,
                &data,
                &Roi::Point { x: 1.0, y: 1.0 },
                1,
                ProfileMethod::Mean,
            )
            .is_none()
        );
    }
}
