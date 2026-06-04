use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::plot::PlotId;
use crate::core::roi::Roi;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::{Plot1D, line_profile_values, rect_profile_values};

/// A window widget to display the 1D profile of an image based on an ROI.
pub struct ProfileWindow {
    plot: Plot1D,
    curve_handle: Option<ItemHandle>,
    window_id: egui::Id,
    open: bool,
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
            size: egui::vec2(420.0, 320.0),
            placement: None,
            remembered_pos: None,
        }
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

    /// Re-calculate and update the profile curve based on the given ROI.
    pub fn update_profile(&mut self, width: u32, height: u32, data: &[f32], roi: &Roi) {
        let profile = match roi {
            Roi::Line { start, end } => line_profile_values(width, height, data, *start, *end).ok(),
            Roi::Rect { x, y } => {
                // By default, average along the columns (vertical axis) for a row profile.
                rect_profile_values(width, height, data, (x.0, x.1, y.0, y.1), true).ok()
            }
            Roi::HRange { y } => {
                let row = ((y.0 + y.1) / 2.0).round() as u32;
                crate::widget::high_level::horizontal_profile_values(width, height, data, row)
                    .ok()
                    .map(|y_vals| {
                        let x_vals: Vec<f64> = (0..width as usize).map(|i| i as f64).collect();
                        (x_vals, y_vals)
                    })
            }
            Roi::VRange { x } => {
                let col = ((x.0 + x.1) / 2.0).round() as u32;
                crate::widget::high_level::vertical_profile_values(width, height, data, col)
                    .ok()
                    .map(|y_vals| {
                        let x_vals: Vec<f64> = (0..height as usize).map(|i| i as f64).collect();
                        (x_vals, y_vals)
                    })
            }
            _ => None,
        };

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
