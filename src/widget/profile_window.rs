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
            self.placement = self.remembered_pos.or_else(|| self.beside_main_window(ctx));
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

    /// Compute where to first place the profile window so it sits beside the
    /// main window instead of covering it, mirroring silx
    /// `ProfileManager.initProfileWindow` (tools/profile/manager.py:1030-1055):
    /// prefer the right of the main window, else the left, else whichever side
    /// of the screen has more room; vertically centre on the main window.
    ///
    /// Returns `None` when the host cannot report the window/monitor geometry
    /// (e.g. Wayland/Android), in which case no explicit position is set.
    fn beside_main_window(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        let (win, monitor) = ctx.input(|i| {
            let vp = i.viewport();
            (vp.outer_rect, vp.monitor_size)
        });
        // Both are in egui points, in monitor space (monitor origin at 0,0).
        Some(placement_beside(win?, monitor?, self.size, 5.0))
    }
}

/// Pure placement maths behind [`ProfileWindow::beside_main_window`], split out
/// so the silx-parity boundaries can be unit-tested without an egui context.
///
/// `win` is the main window's outer rect and `monitor_size` the monitor size,
/// both in egui points / monitor space (origin at 0,0). Returns the top-left
/// the profile window should take so it sits beside `win` without covering it.
fn placement_beside(
    win: egui::Rect,
    monitor_size: egui::Vec2,
    size: egui::Vec2,
    margin: f32,
) -> egui::Pos2 {
    let profile_w = size.x;

    let space_left = win.left(); // screen-left .. window-left
    let space_right = monitor_size.x - win.right(); // window-right .. screen-right

    // Vertically centre the profile on the main window.
    let top = win.top() + (win.height() - size.y) / 2.0;

    let left = if profile_w < space_right {
        // Place the profile to the right of the main window.
        win.right() + margin
    } else if profile_w < space_left {
        // Place it to the left of the main window.
        (win.left() - profile_w - margin).max(0.0)
    } else if space_left > space_right {
        // Not enough room either side: push it to whichever has more.
        0.0
    } else {
        monitor_size.x - profile_w
    };

    egui::pos2(left, top)
}

#[cfg(test)]
mod tests {
    use super::placement_beside;
    use egui::{Rect, pos2, vec2};

    const MARGIN: f32 = 5.0;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    #[test]
    fn places_to_the_right_when_it_fits() {
        // Main window 100..600 on a 1200-wide monitor: 600 pt free on the right.
        let win = Rect::from_min_max(pos2(100.0, 0.0), pos2(600.0, 400.0));
        let p = placement_beside(win, vec2(1200.0, 800.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 605.0); // win.right() + margin
        approx(p.y, 40.0); // (400 - 320) / 2
    }

    #[test]
    fn places_to_the_left_when_right_is_too_tight() {
        // Right edge near the monitor edge (20 pt free); 600 pt free on the left.
        let win = Rect::from_min_max(pos2(600.0, 0.0), pos2(1180.0, 400.0));
        let p = placement_beside(win, vec2(1200.0, 800.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 175.0); // win.left() - profile_w - margin
    }

    #[test]
    fn left_placement_clamps_to_screen_edge() {
        // Just enough left space to choose the left branch but it would go < 0.
        let win = Rect::from_min_max(pos2(422.0, 0.0), pos2(900.0, 400.0));
        let p = placement_beside(win, vec2(1000.0, 500.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 0.0); // max(0, 422 - 420 - 5)
    }

    #[test]
    fn overflow_both_sides_picks_the_roomier_edge() {
        // Profile wider than either gap. More room on the left -> hug left edge.
        let win = Rect::from_min_max(pos2(300.0, 0.0), pos2(900.0, 400.0));
        let p = placement_beside(win, vec2(1000.0, 500.0), vec2(420.0, 320.0), MARGIN);
        approx(p.x, 0.0);

        // More room on the right -> hug the right edge (monitor.x - profile_w).
        let win = Rect::from_min_max(pos2(80.0, 0.0), pos2(400.0, 400.0));
        let p = placement_beside(win, vec2(1000.0, 500.0), vec2(700.0, 320.0), MARGIN);
        approx(p.x, 300.0); // 1000 - 700
    }
}
