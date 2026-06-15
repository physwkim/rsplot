//! A side window displaying the 2D "profile over stack" image — silx
//! `Profile3DToolBar`'s 2D profile (`ProfileImageStack*ROI` with
//! `profileType == "2D"`, `tools/profile/rois.py:1087-1120`).
//!
//! One 1D profile is taken from every frame along the browsed dimension and the
//! rows are stacked into an image of shape `(frame_count, profile_len)`
//! ([`StackProfile`]); this window shows that image, colormapped, in its own
//! viewport — mirroring silx, which builds a `core.ImageProfileData` and
//! displays the 2D profile in a `Plot2D` profile window with the stack's own
//! colormap.

use egui_wgpu::RenderState;

use crate::core::backend::{ImageSpec, ItemHandle};
use crate::core::colormap::Colormap;
use crate::core::plot::PlotId;
use crate::core::transform::YAxis;
use crate::widget::high_level::{Plot2D, StackProfile};

/// A window widget showing the stacked 2D profile of a [`StackView`] as a
/// colormapped image (silx `Profile3DToolBar` 2D profile window).
///
/// [`StackView`]: crate::widget::high_level::StackView
pub struct StackProfileWindow {
    plot: Plot2D,
    /// Handle of the live stacked-profile image, recreated when the profile
    /// dimensions change so a shrinking/growing profile does not keep a stale
    /// extent.
    image_handle: Option<ItemHandle>,
    /// `(profile_len, frame_count)` of the currently-uploaded image, so a
    /// dimension change re-fits the view (silx re-creates the profile image).
    last_dims: Option<(usize, usize)>,
    window_id: egui::Id,
    open: bool,
    /// Initial outer size of the profile viewport, in points (matches
    /// [`ProfileWindow`](crate::widget::profile_window::ProfileWindow)).
    size: egui::Vec2,
    /// Position chosen for the *current* open session; computed once on open and
    /// then left untouched so the user can freely drag it.
    placement: Option<egui::Pos2>,
    /// Last observed outer position, restored as the initial placement on the
    /// next open (silx `ProfileManager._previousWindowGeometry`).
    remembered_pos: Option<egui::Pos2>,
}

impl StackProfileWindow {
    /// Create a new stacked-profile window backed by a [`Plot2D`].
    pub fn new(render_state: &RenderState, plot_id: PlotId) -> Self {
        let mut plot = Plot2D::new(render_state, plot_id);
        plot.set_graph_title("Profile over stack");
        // The stacked image is profile-position (X) × frame index (Y); unlike the
        // browsed image the frame axis reads bottom-up, so do not invert Y.
        plot.set_y_inverted(false);
        plot.set_keep_data_aspect_ratio(false);
        plot.set_graph_x_label("Profile position");
        plot.set_graph_y_label("Frame", YAxis::Left);
        Self {
            plot,
            image_handle: None,
            last_dims: None,
            window_id: egui::Id::new(plot_id).with("stack_profile_window"),
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

    /// Open or close the window. Closing forgets the placement so the next open
    /// re-runs the beside-the-main-window logic (mirrors
    /// [`ProfileWindow`](crate::widget::profile_window::ProfileWindow)).
    pub fn set_open(&mut self, open: bool) {
        if !open {
            self.placement = None;
        }
        self.open = open;
    }

    /// Upload `profile` as a colormapped image of shape
    /// `(profile.frame_count, profile.profile_len)` (row-major `[frame,
    /// position]`), using the stack's `colormap` — silx
    /// `core.ImageProfileData(profile=..., colormap=item.getColormap())`.
    ///
    /// An empty profile (no frames or zero-length per-frame profile) is ignored,
    /// leaving the current image shown (a no-op extraction does not blank the
    /// window). The view is re-fit only when the image dimensions change.
    pub fn set_profile(&mut self, profile: &StackProfile, colormap: Colormap) {
        if profile.frame_count == 0 || profile.profile_len == 0 {
            return;
        }
        // The image pipeline takes f32 scalars; the stacked profile is f64.
        let data: Vec<f32> = profile.values.iter().map(|&v| v as f32).collect();
        let spec = ImageSpec::scalar(
            profile.profile_len as u32,
            profile.frame_count as u32,
            &data,
            colormap,
        );
        let dims = (profile.profile_len, profile.frame_count);
        if self.last_dims == Some(dims)
            && let Some(handle) = self.image_handle
        {
            self.plot.update_image_spec(handle, spec);
        } else {
            // First profile, or the profile changed shape (e.g. a longer line):
            // drop the old item and re-add so the extent tracks the new shape.
            if let Some(handle) = self.image_handle.take() {
                self.plot.remove_image(handle);
            }
            self.image_handle = Some(self.plot.add_image_spec(spec));
            self.last_dims = Some(dims);
            self.plot.reset_zoom();
        }
    }

    /// Mutable access to the backing plot (e.g. to change its colormap or fetch
    /// pick data in tests).
    pub fn plot_mut(&mut self) -> &mut Plot2D {
        &mut self.plot
    }

    /// Show the stacked profile in its own native OS window (a separate egui
    /// viewport), mirroring [`ProfileWindow::show`]: positioned beside the main
    /// window on first open, then freely draggable, its position restored on the
    /// next open.
    ///
    /// [`ProfileWindow::show`]: crate::widget::profile_window::ProfileWindow::show
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        if self.placement.is_none() {
            self.placement = self
                .remembered_pos
                .or_else(|| crate::widget::detached::beside_main_window(ctx, self.size));
        }

        let viewport_id = egui::ViewportId::from_hash_of(self.window_id);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Profile over stack")
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
