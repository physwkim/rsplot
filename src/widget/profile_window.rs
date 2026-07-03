use egui::Color32;
use egui_wgpu::RenderState;

use crate::core::backend::ItemHandle;
use crate::core::plot::PlotId;
use crate::core::roi::Roi;
use crate::render::gpu_curve::CurveData;
use crate::widget::high_level::{
    Plot1D, ProfileMethod, aligned_profile_values, free_line_profile, rect_profile_values,
};

/// A single named profile curve extracted from a profile ROI: a legend label, a
/// draw color, and the `(x, y)` samples. A line/range/rect ROI yields one of
/// these; a [`Roi::Cross`] yields two (silx `ProfileImageCrossROI`'s horizontal
/// and vertical sub-profiles), which is why extraction returns a `Vec`.
struct ProfileCurve {
    label: &'static str,
    color: Color32,
    x: Vec<f64>,
    y: Vec<f64>,
}

/// The horizontal full-row profile `(x, y)` through image `row` (silx
/// `ProfileImageHorizontalLineROI` / `_alignedFullProfile`): `x` is the column
/// index, `y` the band reduction over `line_width` rows centered on `row`. The
/// caller attaches a label/color to wrap it into a [`ProfileCurve`].
fn horizontal_profile_xy(
    width: u32,
    height: u32,
    data: &[f32],
    row: f64,
    line_width: u32,
    method: ProfileMethod,
) -> Option<(Vec<f64>, Vec<f64>)> {
    aligned_profile_values(width, height, data, row, line_width, true, method)
        .ok()
        .map(|y| {
            let x = (0..width as usize).map(|i| i as f64).collect();
            (x, y)
        })
}

/// The vertical full-column profile `(x, y)` through image `col` (silx
/// `ProfileImageVerticalLineROI`): `x` is the row index, `y` the band reduction
/// over `line_width` columns centered on `col`.
fn vertical_profile_xy(
    width: u32,
    height: u32,
    data: &[f32],
    col: f64,
    line_width: u32,
    method: ProfileMethod,
) -> Option<(Vec<f64>, Vec<f64>)> {
    aligned_profile_values(width, height, data, col, line_width, false, method)
        .ok()
        .map(|y| {
            let x = (0..height as usize).map(|i| i as f64).collect();
            (x, y)
        })
}

/// Compute the named profile curve(s) for `roi` over a row-major image,
/// integrating a band of `line_width` pixels and reducing it with `method`
/// (silx `ProfileToolButtons` line-width + mean/sum). Returns an empty `Vec` for
/// ROI kinds that have no profile. Pure dispatch over the tested profile
/// extractors:
///
/// - [`Roi::Line`] -> [`line_profile_band`] (bilinear band, silx
///   `BilinearImage.profile_line`).
/// - [`Roi::Rect`] -> [`rect_profile_values`] reduced along the columns.
/// - [`Roi::HRange`] / [`Roi::VRange`] -> [`aligned_profile_values`] centered on
///   the range's midpoint with `line_width` as the integration band (silx
///   `_alignedFullProfile`; `int(position)` placement). `line_width == 1`,
///   `Mean` reproduces the single-row/column average.
/// - [`Roi::Cross`] -> **two** curves, the horizontal row-profile and the
///   vertical column-profile through the cross center, shown simultaneously
///   (silx `ProfileImageCrossROI`, which manages an `hline` + `vline` sub-ROI).
fn profiles_for_roi(
    width: u32,
    height: u32,
    data: &[f32],
    roi: &Roi,
    line_width: u32,
    method: ProfileMethod,
) -> Vec<ProfileCurve> {
    match roi {
        Roi::Line { start, end } => {
            free_line_profile(width, height, data, *start, *end, line_width, method)
                .ok()
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        Roi::Rect { x, y } => {
            rect_profile_values(width, height, data, (x.0, x.1, y.0, y.1), true, method)
                .ok()
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        Roi::HRange { y } => {
            let row = (y.0 + y.1) / 2.0;
            horizontal_profile_xy(width, height, data, row, line_width, method)
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        Roi::VRange { x } => {
            let col = (x.0 + x.1) / 2.0;
            vertical_profile_xy(width, height, data, col, line_width, method)
                .map(|(x, y)| ProfileCurve {
                    label: "profile",
                    color: Color32::YELLOW,
                    x,
                    y,
                })
                .into_iter()
                .collect()
        }
        // Cross profile: extract both the horizontal (row through cy) and
        // vertical (column through cx) full profiles and show them together,
        // mirroring silx `ProfileImageCrossROI` (two sub-ROIs, one window).
        Roi::Cross { center } => {
            let (cx, cy) = *center;
            let h =
                horizontal_profile_xy(width, height, data, cy, line_width, method).map(|(x, y)| {
                    ProfileCurve {
                        label: "h profile",
                        color: Color32::YELLOW,
                        x,
                        y,
                    }
                });
            let v =
                vertical_profile_xy(width, height, data, cx, line_width, method).map(|(x, y)| {
                    ProfileCurve {
                        label: "v profile",
                        color: Color32::from_rgb(0, 200, 255),
                        x,
                        y,
                    }
                });
            [h, v].into_iter().flatten().collect()
        }
        _ => Vec::new(),
    }
}

/// The image + ROI the current profile was extracted from, retained so the
/// profile recomputes when the line width, reduction method, or image data
/// change — not only during a fresh drag. Mirrors silx `ProfileManager`, which
/// recomputes on item DATA/POSITION change and on `setProfileMethod`/
/// `setProfileLineWidth` (`manager.py:936-944`, `rois.py:238-257`). `None` until
/// the first image-ROI [`ProfileWindow::update_profile`]; cleared by the
/// precomputed-curve path so a later width/method edit never re-derives a stale
/// image ROI over a scatter/stack profile.
struct ProfileSource {
    width: u32,
    height: u32,
    data: Vec<f32>,
    roi: Roi,
}

/// A window widget to display the 1D profile of an image based on an ROI.
pub struct ProfileWindow {
    plot: Plot1D,
    /// The retained image + ROI the profile was last extracted from (see
    /// [`ProfileSource`]); drives width/method/data-change recompute.
    source: Option<ProfileSource>,
    /// Handles of the live profile curves. One for a line/range/rect ROI; two
    /// for a cross ROI (the horizontal and vertical sub-profiles). Rebuilt when
    /// the curve count changes between updates (silx `ProfileImageCrossROI`).
    curve_handles: Vec<ItemHandle>,
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
            source: None,
            curve_handles: Vec::new(),
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

    /// Set the profile band width in pixels (clamped to at least 1) and
    /// recompute the profile from the retained source (silx
    /// `setProfileLineWidth` -> `invalidateProfile`). A no-op recompute when no
    /// image-ROI source is retained.
    pub fn set_line_width(&mut self, width: u32) {
        self.line_width = width.max(1);
        self.recompute();
    }

    /// The current band reduction method (silx `ProfileOptionToolButton`).
    pub fn method(&self) -> ProfileMethod {
        self.method
    }

    /// Set the band reduction method (mean vs sum) and recompute the profile
    /// from the retained source (silx `setProfileMethod` -> `invalidateProfile`).
    /// A no-op recompute when no image-ROI source is retained.
    pub fn set_method(&mut self, method: ProfileMethod) {
        self.method = method;
        self.recompute();
    }

    /// The profile curve `y`-values the retained image-ROI source currently
    /// produces at the active line width and method — one entry per curve
    /// ([`Roi::Cross`] yields two). Empty when no image-ROI source is retained
    /// (before the first drag, or after a precomputed-curve profile). Lets a
    /// caller/test confirm that a width/method/data change flows into the
    /// profile without a fresh drag (the R2-4 recompute contract).
    pub fn active_profile_values(&self) -> Vec<Vec<f64>> {
        match &self.source {
            Some(src) => profiles_for_roi(
                src.width,
                src.height,
                &src.data,
                &src.roi,
                self.line_width,
                self.method,
            )
            .into_iter()
            .map(|c| c.y)
            .collect(),
            None => Vec::new(),
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

    /// Re-calculate and update the profile curve based on the given ROI, using
    /// the current line width and reduction method. Retains `(data, roi)` as the
    /// active [`ProfileSource`] so subsequent width/method edits and
    /// [`refresh_image`](Self::refresh_image) calls recompute from it.
    pub fn update_profile(&mut self, width: u32, height: u32, data: &[f32], roi: &Roi) {
        self.source = Some(ProfileSource {
            width,
            height,
            data: data.to_vec(),
            roi: roi.clone(),
        });
        self.recompute();
    }

    /// Replace the retained image data (keeping the active ROI) and recompute —
    /// the host calls this when its image changes while a profile is open (silx
    /// recompute on item DATA change, `manager.py:936-944`). A no-op when no
    /// profile ROI has been drawn yet, so hosts may call it unconditionally on
    /// every image update.
    pub fn refresh_image(&mut self, width: u32, height: u32, data: &[f32]) {
        let Some(src) = self.source.as_mut() else {
            return;
        };
        src.width = width;
        src.height = height;
        src.data = data.to_vec();
        self.recompute();
    }

    /// Re-extract the profile curve(s) from the retained [`ProfileSource`] with
    /// the current line width and method, and push them to the plot. The single
    /// recompute path for the image-ROI profile: shared by
    /// [`update_profile`](Self::update_profile),
    /// [`refresh_image`](Self::refresh_image), and the in-window width/method
    /// edits. No-op when no source is retained.
    fn recompute(&mut self) {
        let curves = match &self.source {
            Some(src) => profiles_for_roi(
                src.width,
                src.height,
                &src.data,
                &src.roi,
                self.line_width,
                self.method,
            ),
            None => return,
        };
        self.set_curves(curves);
    }

    /// Display a single precomputed `(x, y)` profile curve, for tool bars whose
    /// profile is sampled directly rather than re-derived from an image + ROI
    /// (silx `ScatterProfileToolBar` / `Profile3DToolBar`, whose profiles come
    /// from [`crate::core::scatter_viz::scatter_line_profile`] / a stack
    /// reduction). `label` names the curve in the legend; `color` is its stroke.
    /// An empty `x` is ignored (the previous profile stays shown).
    pub fn set_profile_curve(
        &mut self,
        label: &'static str,
        color: Color32,
        x: Vec<f64>,
        y: Vec<f64>,
    ) {
        if x.is_empty() {
            return;
        }
        // This profile is sampled upstream, not from a retained image + ROI, so
        // drop any retained source: a later width/method edit must not re-derive
        // a stale image profile over this precomputed curve.
        self.source = None;
        self.set_curves(vec![ProfileCurve { label, color, x, y }]);
    }

    /// Push `curves` into the backing [`Plot1D`] and auto-scale, the shared body
    /// of [`Self::update_profile`] and [`Self::set_profile_curve`]. An empty list
    /// leaves the current profile untouched (so a no-op extraction does not blank
    /// the window).
    fn set_curves(&mut self, curves: Vec<ProfileCurve>) {
        if curves.is_empty() {
            return;
        }

        // When the curve count changes (line/range/rect ↔ cross), drop the old
        // handles so stale curves do not linger; otherwise update in place.
        if self.curve_handles.len() != curves.len() {
            for handle in self.curve_handles.drain(..) {
                self.plot.remove(handle);
            }
        }

        for (i, c) in curves.into_iter().enumerate() {
            if let Some(&handle) = self.curve_handles.get(i) {
                let curve = CurveData::new(c.x, c.y, c.color);
                self.plot.update_curve_data(handle, &curve);
            } else {
                let handle = self
                    .plot
                    .add_curve_with_legend(&c.x, &c.y, c.color, c.label);
                self.curve_handles.push(handle);
            }
        }
        // Auto-scale limits based on data.
        self.plot.reset_zoom_to_data();
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
            // option). Each edit routes through `set_line_width`/`set_method`,
            // which recompute the profile immediately from the retained source
            // (silx `setProfileLineWidth`/`setProfileMethod` ->
            // `invalidateProfile`), so it no longer waits for the next drag.
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
                let mut method = self.method;
                egui::ComboBox::from_id_salt("profile_method")
                    .selected_text(match method {
                        ProfileMethod::Mean => "Mean",
                        ProfileMethod::Sum => "Sum",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut method, ProfileMethod::Mean, "Mean");
                        ui.selectable_value(&mut method, ProfileMethod::Sum, "Sum");
                    });
                if method != self.method {
                    self.set_method(method);
                }
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
        let curves = profiles_for_roi(
            3,
            3,
            &data,
            &Roi::HRange { y: (1.0, 1.0) },
            1,
            ProfileMethod::Mean,
        );
        assert_eq!(curves.len(), 1);
        assert_eq!(curves[0].y, vec![10.0, 11.0, 12.0]);

        // Width 3, Sum -> every column summed over all three rows:
        // col c -> (0+10+20) + c*3 = 30 + 3c = [30, 33, 36].
        let curves = profiles_for_roi(
            3,
            3,
            &data,
            &Roi::HRange { y: (1.0, 1.0) },
            3,
            ProfileMethod::Sum,
        );
        assert_eq!(curves.len(), 1);
        assert_eq!(curves[0].y, vec![30.0, 33.0, 36.0]);
    }

    #[test]
    fn profile_for_roi_cross_yields_horizontal_and_vertical_curves() {
        // A cross at (col=1, row=1) extracts BOTH the row-1 horizontal profile
        // and the col-1 vertical profile simultaneously (silx
        // ProfileImageCrossROI), width 1 / Mean = the raw line.
        let data = ramp_3x3();
        let curves = profiles_for_roi(
            3,
            3,
            &data,
            &Roi::Cross { center: (1.0, 1.0) },
            1,
            ProfileMethod::Mean,
        );
        assert_eq!(curves.len(), 2);
        // Horizontal profile = row 1 across columns: value == 10 + col.
        assert_eq!(curves[0].label, "h profile");
        assert_eq!(curves[0].y, vec![10.0, 11.0, 12.0]);
        // Vertical profile = column 1 across rows: value == row*10 + 1.
        assert_eq!(curves[1].label, "v profile");
        assert_eq!(curves[1].y, vec![1.0, 11.0, 21.0]);
    }

    #[test]
    fn profile_for_roi_returns_empty_for_unsupported_kind() {
        let data = ramp_3x3();
        assert!(
            profiles_for_roi(
                3,
                3,
                &data,
                &Roi::Point { x: 1.0, y: 1.0 },
                1,
                ProfileMethod::Mean,
            )
            .is_empty()
        );
    }
}
