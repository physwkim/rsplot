use crate::widget::high_level::Plot2D;
use egui::Window;

/// A dedicated widget to track and manage multiple ROIs drawn on a plot.
pub struct RoiManagerWidget {
    window_id: egui::Id,
    pub open: bool,
}

impl Default for RoiManagerWidget {
    fn default() -> Self {
        Self {
            window_id: egui::Id::new("roi_manager_widget"),
            open: false,
        }
    }
}

impl RoiManagerWidget {
    /// Create a new ROI Manager Widget.
    pub fn new() -> Self {
        Self::default()
    }

    /// Show the ROI Manager floating window.
    pub fn show(&mut self, ctx: &egui::Context, plot: &mut Plot2D) {
        let mut open = self.open;

        Window::new("ROI Manager")
            .id(self.window_id)
            .open(&mut open)
            .resizable(true)
            .min_width(200.0)
            .show(ctx, |ui| {
                // Plot2D already implements the core ROI list and management UI,
                // so we can seamlessly embed it here.
                plot.show_roi_manager(ui);
            });

        self.open = open;
    }
}
