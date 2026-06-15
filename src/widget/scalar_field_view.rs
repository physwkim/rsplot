//! [`ScalarFieldView`] — an interactive 3D scalar-field view inside an egui `Ui`.
//!
//! Port of silx `silx.gui.plot3d.ScalarFieldView.ScalarFieldView`: a 3D scene
//! that owns a single [`ScalarField3D`] data group (iso-surfaces + one cut
//! plane) and renders it through a [`SceneWidget`]. It is the plot3d analogue of
//! the 2D [`crate::widget::high_level::ImageView`] — a thin, opinionated wrapper
//! that wires one data item into the generic scene widget and frames the camera
//! to the volume.
//!
//! Faithful behaviours carried over from silx `ScalarFieldView`:
//!
//! - **`setData`** stores the field, updates the scene bounds to the volume box,
//!   and re-frames the camera (`centerScene`) **only the first time** data is set
//!   (`if not wasData: self.centerScene()`, `ScalarFieldView.py`). Subsequent
//!   `set_data` calls update the data and bounds but keep the user's viewpoint.
//! - **`addIsosurface` / `removeIsosurface` / `clearIsosurfaces`** manage the
//!   field's iso-surfaces; **`getCutPlanes()`** exposes the single cut plane
//!   (here via [`ScalarFieldView::field_mut`] + [`ScalarFieldView::rebuild`]).
//!
//! Like [`SceneWidget`], geometry is uploaded eagerly when the data layer
//! changes (not rebuilt per frame): the mutating methods take a [`RenderState`]
//! and re-extract the field's geometry into the inner widget. After editing the
//! field through [`field_mut`](ScalarFieldView::field_mut) (e.g. configuring the
//! cut plane or an iso-surface level), call
//! [`rebuild`](ScalarFieldView::rebuild) to push the change to the GPU.

use egui::{Color32, Response, Ui};
use egui_wgpu::RenderState;

use crate::core::scene3d::mat4::Vec3;
use crate::core::scene3d::pick::picking_segment;
use crate::render::gpu_scene3d::{Scene3dGeometry, Scene3dId};
use crate::render::scene3d_items::ScalarField3D;
use crate::widget::scene_widget::SceneWidget;

/// The result of picking a [`ScalarFieldView`] at a screen position: the nearest
/// hit's world-space position and the field value sampled there (`None` when the
/// hit lies outside the volume box). Port of the data silx
/// `PositionInfoWidget.pick` reads from a `PickingResult` (scene position + data
/// value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldPick {
    /// World-space position of the nearest hit.
    pub position: Vec3,
    /// Field value at the hit, sampled through the cut plane's interpolation, or
    /// `None` if the position is outside the field box.
    pub value: Option<f32>,
}

/// An interactive 3D view of one [`ScalarField3D`] (iso-surfaces + a cut plane).
///
/// Construct with [`ScalarFieldView::new`], push data with
/// [`set_data`](ScalarFieldView::set_data), add iso-surfaces / configure the cut
/// plane, then call [`show`](ScalarFieldView::show) each frame.
pub struct ScalarFieldView {
    scene: SceneWidget,
    field: ScalarField3D,
    /// Whether data has ever been set — drives the silx `centerScene`-once
    /// behaviour (re-frame the camera on the first `set_data` only).
    had_data: bool,
}

impl ScalarFieldView {
    /// Create a scalar-field view bound to `id`, installing the 3D GPU resources
    /// into `render_state` if needed. Starts empty (no data, no iso-surfaces,
    /// hidden cut plane).
    pub fn new(render_state: &RenderState, id: Scene3dId) -> Self {
        ScalarFieldView {
            scene: SceneWidget::new(render_state, id),
            field: ScalarField3D::new(),
            had_data: false,
        }
    }

    /// Set the 3D scalar field, `data` row-major as `(depth, height, width)` with
    /// `width` contiguous (see [`ScalarField3D::set_data`]). Returns `false`
    /// (leaving the view unchanged) when the data is inconsistent or any
    /// dimension is `< 2`.
    ///
    /// On the **first** successful call the camera is framed to the volume box
    /// (silx `centerScene`); later calls update the data and bounds but keep the
    /// current viewpoint. Either way the scene geometry (iso-surfaces + cut
    /// plane) is rebuilt and re-uploaded.
    pub fn set_data(
        &mut self,
        render_state: &RenderState,
        data: &[f32],
        depth: usize,
        height: usize,
        width: usize,
    ) -> bool {
        let first = !self.had_data;
        if !self.field.set_data(data, depth, height, width) {
            return false;
        }
        self.had_data = true;
        if let Some(bounds) = self.field.bounds() {
            if first {
                self.scene.set_bounds(render_state, bounds);
            } else {
                self.scene.set_bounds_keep_view(render_state, bounds);
            }
        }
        self.rebuild(render_state);
        true
    }

    /// Read-only access to the underlying field.
    pub fn field(&self) -> &ScalarField3D {
        &self.field
    }

    /// Mutable access to the underlying field — configure the cut plane, change
    /// an iso-surface level, etc. Call [`rebuild`](ScalarFieldView::rebuild)
    /// afterwards to push the change to the GPU.
    pub fn field_mut(&mut self) -> &mut ScalarField3D {
        &mut self.field
    }

    /// Read-only access to the inner scene widget (camera, bounds, background).
    pub fn scene(&self) -> &SceneWidget {
        &self.scene
    }

    /// Mutable access to the inner scene widget — e.g. to apply a viewpoint
    /// preset via [`SceneWidget::camera_mut`] or set the background colour.
    pub fn scene_mut(&mut self) -> &mut SceneWidget {
        &mut self.scene
    }

    /// Add a fixed-level iso-surface and rebuild (silx `addIsosurface`). Returns
    /// the iso-surface index.
    pub fn add_isosurface(
        &mut self,
        render_state: &RenderState,
        level: f32,
        color: Color32,
    ) -> usize {
        let index = self.field.add_isosurface(level, color);
        self.rebuild(render_state);
        index
    }

    /// Add an auto-level iso-surface (silx `addIsosurface` with a callable) and
    /// rebuild. Returns the iso-surface index.
    pub fn add_auto_isosurface(
        &mut self,
        render_state: &RenderState,
        auto: fn(&[f32]) -> f32,
        color: Color32,
    ) -> usize {
        let index = self.field.add_auto_isosurface(auto, color);
        self.rebuild(render_state);
        index
    }

    /// Remove the iso-surface at `index` and rebuild (silx `removeIsosurface`);
    /// out-of-range is a no-op returning `false` (no rebuild).
    pub fn remove_isosurface(&mut self, render_state: &RenderState, index: usize) -> bool {
        if self.field.remove_isosurface(index) {
            self.rebuild(render_state);
            true
        } else {
            false
        }
    }

    /// Remove all iso-surfaces and rebuild (silx `clearIsosurfaces`).
    pub fn clear_isosurfaces(&mut self, render_state: &RenderState) {
        self.field.clear_isosurfaces();
        self.rebuild(render_state);
    }

    /// Re-extract the field's geometry (iso-surfaces + cut plane) and re-upload
    /// it to the inner scene widget. Call this after mutating the field through
    /// [`field_mut`](ScalarFieldView::field_mut).
    pub fn rebuild(&mut self, render_state: &RenderState) {
        let mut geometry = Scene3dGeometry::new();
        self.field.append_to(&mut geometry);
        self.scene.set_geometry(render_state, geometry);
    }

    /// Pick the field under a click at normalized device coordinates `ndc`
    /// (`x, y ∈ [-1, 1]`), returning the nearest hit's world position and the
    /// field value there, or `None` if the ray misses everything.
    ///
    /// Combines the two pick channels silx's `PositionInfoWidget` reduces to for
    /// a [`ScalarFieldView`]: the iso-surfaces / scatter geometry
    /// ([`SceneWidget::pick`]) and the cut plane
    /// ([`ScalarField3D::pick_cut_plane`]). The nearest by NDC depth wins; the
    /// value is sampled with [`ScalarField3D::value_at`] at the chosen position.
    /// Call after [`show`](ScalarFieldView::show) so the camera aspect is current.
    pub fn pick(&self, ndc: (f32, f32)) -> Option<FieldPick> {
        let camera = self.scene.camera();
        let mvp = camera.matrix();

        // Nearest data surface / scatter point, by NDC depth.
        let mut best: Option<(f32, Vec3)> = None;
        if let Some(hit) = self.scene.pick(ndc) {
            best = Some((hit.ndc_depth, hit.position));
        }
        // The cut plane (textured mesh, not in the triangle channel) is picked
        // against the field directly.
        if let Some(segment) = picking_segment(camera, ndc)
            && let Some(pos) = self.field.pick_cut_plane(segment)
        {
            let depth = mvp.transform_point(pos, true).z;
            if best.is_none_or(|(d, _)| depth < d) {
                best = Some((depth, pos));
            }
        }

        let (_, position) = best?;
        Some(FieldPick {
            position,
            value: self.field.value_at(position),
        })
    }

    /// Lay out the view, handle orbit/pan/zoom interaction, and paint. Returns
    /// the egui [`Response`] for the scene rect.
    pub fn show(&mut self, ui: &mut Ui) -> Response {
        self.scene.show(ui)
    }
}
