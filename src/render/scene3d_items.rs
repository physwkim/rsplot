//! 3D data items — the `silx.gui.plot3d.items` port.
//!
//! Items hold data plus presentation state (colormap, marker, size) and emit
//! their geometry into a [`Scene3dGeometry`] via [`append_to`](Scatter3D::append_to),
//! the analogue of silx's scene-primitive build. The GPU primitives themselves
//! live in [`crate::render::gpu_scene3d`]; this module is the headless item layer
//! (color mapping + bounds), unit-tested without a GPU.

use std::collections::BTreeSet;

use egui::Color32;

use crate::core::colormap::{AutoscaleMode, Colormap, ColormapName};
use crate::core::complex::ComplexMode;
use crate::core::scatter_viz::delaunay;
use crate::core::scene3d::marching_cubes::isosurface as marching_cubes_isosurface;
use crate::core::scene3d::mat4::{Mat4, Vec3, mat4_rotate};
use crate::core::scene3d::plane::{Plane, box_plane_intersect, segment_plane_intersect};
use crate::core::scene3d::transform::Item3DTransform;
use crate::render::gpu_scene3d::{
    ImageInterpolation, PointMarker, Scene3dGeometry, Scene3dImageLayer, Scene3dTexturedMesh,
    flat_normal,
};

/// silx's default plot symbol size in pixels (`_config.DEFAULT_PLOT_SYMBOL_SIZE`).
pub const DEFAULT_SCATTER3D_SIZE: f32 = 6.0;

/// Append `build`'s raw geometry through `transform` — the single owner of the
/// bake-time transform application. silx applies the `DataItem3D` transform
/// stack in the scene graph at render time (`items/core.py:288-315`); this
/// port bakes the composed matrix into the channels
/// ([`Scene3dGeometry::apply_transform`]) when the item appends, so rendering
/// and the CPU pick traversal read the same transformed positions by
/// construction. `raw_bounds` are the item's untransformed data bounds,
/// against which bbox-relative rotation centres resolve
/// (`items/core.py:376-405`).
fn append_with_transform(
    transform: &Item3DTransform,
    raw_bounds: Option<(Vec3, Vec3)>,
    geometry: &mut Scene3dGeometry,
    build: impl FnOnce(&mut Scene3dGeometry),
) {
    if transform.is_identity() {
        build(geometry);
    } else {
        let mut local = Scene3dGeometry::new();
        build(&mut local);
        local.apply_transform(&transform.composed_matrix(raw_bounds));
        geometry.extend_from(&local);
    }
}

/// Generate the transform-stack accessors of a 3D item. The stack itself —
/// with the silx public setters `set_scale`/`set_translation`/
/// `set_rotation_center`/`set_rotation`/`set_matrix`
/// (`items/core.py:335-485`) — is [`Item3DTransform`], reached through
/// `transform()`/`transform_mut()` (the names stay on the transform type
/// because some items already use `set_scale` for their 2D pixel scale).
macro_rules! impl_item3d_transform {
    ($ty:ty) => {
        impl $ty {
            /// The item's transform stack (silx `DataItem3D` transforms,
            /// `items/core.py:288-315`).
            pub fn transform(&self) -> &Item3DTransform {
                &self.transform
            }

            /// Mutable access to the transform stack — e.g.
            /// `item.transform_mut().set_scale(2.0, 2.0, 1.0)` (silx
            /// `setScale`, `items/core.py:335-345`). Applied on the next
            /// `append_to`; [`Self::bounds`] reflects it immediately.
            pub fn transform_mut(&mut self) -> &mut Item3DTransform {
                &mut self.transform
            }
        }
    };
}

/// A 3D scatter plot: per-point `(x, y, z)` positions coloured by a per-point
/// `value` through a [`Colormap`], drawn as [`PointMarker`] sprites of one size.
///
/// Port of silx `plot3d.items.Scatter3D` (`DataItem3D` + `ColormapMixIn` +
/// `SymbolMixIn`). silx colours points on the GPU from a colormap texture; here
/// the mapping is done on the CPU via [`Colormap::color_at`] when building the
/// geometry — simpler, and points are few relative to image rasters.
#[derive(Clone, Debug)]
pub struct Scatter3D {
    x: Vec<f32>,
    y: Vec<f32>,
    z: Vec<f32>,
    values: Vec<f64>,
    colormap: Colormap,
    marker: PointMarker,
    size: f32,
    transform: Item3DTransform,
}

impl Default for Scatter3D {
    fn default() -> Self {
        Self::new()
    }
}

impl Scatter3D {
    /// An empty scatter with silx defaults: the gray colormap over `[0, 1]`,
    /// circle markers at [`DEFAULT_SCATTER3D_SIZE`].
    pub fn new() -> Self {
        Self {
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            values: Vec::new(),
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            marker: PointMarker::Circle,
            size: DEFAULT_SCATTER3D_SIZE,
            transform: Item3DTransform::default(),
        }
    }

    /// Replace the point data (silx `Scatter3D.setData`). The four arrays must be
    /// the same length; on a length mismatch the data is left unchanged and
    /// `false` is returned (silx asserts equal lengths).
    pub fn set_data(&mut self, x: &[f32], y: &[f32], z: &[f32], values: &[f64]) -> bool {
        let n = x.len();
        if y.len() != n || z.len() != n || values.len() != n {
            return false;
        }
        self.x = x.to_vec();
        self.y = y.to_vec();
        self.z = z.to_vec();
        self.values = values.to_vec();
        true
    }

    /// Builder form of [`set_data`](Self::set_data); a length mismatch leaves the
    /// data empty.
    pub fn with_data(mut self, x: &[f32], y: &[f32], z: &[f32], values: &[f64]) -> Self {
        self.set_data(x, y, z, values);
        self
    }

    /// Set the colormap (silx `ColormapMixIn.setColormap`).
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
    }

    /// Builder form of [`set_colormap`](Self::set_colormap).
    pub fn with_colormap(mut self, colormap: Colormap) -> Self {
        self.colormap = colormap;
        self
    }

    /// Read-only access to the colormap.
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Mutable access to the colormap (e.g. to set the value range directly).
    pub fn colormap_mut(&mut self) -> &mut Colormap {
        &mut self.colormap
    }

    /// Fit the colormap's value range to the current data with `mode` (silx's
    /// colormap autoscale over the value array), returning the new `(vmin, vmax)`.
    /// With no data the range falls back to the autoscale default, matching
    /// [`AutoscaleMode::range`].
    pub fn autoscale_colormap(&mut self, mode: AutoscaleMode) -> (f64, f64) {
        let (vmin, vmax) = self.colormap.autoscale_range(mode, &self.values);
        self.colormap.vmin = vmin;
        self.colormap.vmax = vmax;
        (vmin, vmax)
    }

    /// Set the marker shape (silx `SymbolMixIn.setSymbol`).
    pub fn set_marker(&mut self, marker: PointMarker) {
        self.marker = marker;
    }

    /// Builder form of [`set_marker`](Self::set_marker).
    pub fn with_marker(mut self, marker: PointMarker) -> Self {
        self.marker = marker;
        self
    }

    /// Set the marker size in pixels (silx `SymbolMixIn.setSymbolSize`), clamped
    /// to be non-negative.
    pub fn set_size(&mut self, size: f32) {
        self.size = size.max(0.0);
    }

    /// Builder form of [`set_size`](Self::set_size).
    pub fn with_size(mut self, size: f32) -> Self {
        self.set_size(size);
        self
    }

    /// Number of points.
    pub fn len(&self) -> usize {
        self.x.len()
    }

    /// True when there are no points.
    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }

    /// Raw (untransformed) data bounds over the points.
    fn raw_bounds(&self) -> Option<(Vec3, Vec3)> {
        if self.is_empty() {
            return None;
        }
        let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
        let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for i in 0..self.len() {
            let (px, py, pz) = (self.x[i], self.y[i], self.z[i]);
            min.x = min.x.min(px);
            min.y = min.y.min(py);
            min.z = min.z.min(pz);
            max.x = max.x.max(px);
            max.y = max.y.max(py);
            max.z = max.z.max(pz);
        }
        Some((min, max))
    }

    /// Axis-aligned scene bounds `(min, max)` over the points, through the
    /// item's transform (silx `DataItem3D` bounds, `transformed=True`), or
    /// `None` when empty. Useful to frame a
    /// [`crate::widget::scene_widget::SceneWidget`].
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(self.raw_bounds())
    }

    /// Append this scatter's points (coloured through the colormap) to
    /// `geometry`, ready to upload via [`crate::render::gpu_scene3d::set_scene3d`].
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        append_with_transform(&self.transform, self.raw_bounds(), geometry, |g| {
            for i in 0..self.len() {
                let [r, gr, b, a] = self.colormap.color_at(self.values[i]);
                g.add_point(
                    [self.x[i], self.y[i], self.z[i]],
                    Color32::from_rgba_unmultiplied(r, gr, b, a),
                    self.size,
                    self.marker,
                );
            }
        });
    }
}

impl_item3d_transform!(Scatter3D);

/// silx default line width for a [`Scatter2D`] in LINES mode
/// (`Scatter2D.__init__`: `self._lineWidth = 1.0`).
pub const DEFAULT_SCATTER2D_LINE_WIDTH: f32 = 1.0;

/// How a [`Scatter2D`]'s `(x, y, value)` data is drawn (silx
/// `ScatterVisualizationMixIn.Visualization`, restricted to the three modes
/// `Scatter2D` supports).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scatter2DVisualization {
    /// A marker sprite at each point, coloured by its value (silx `POINTS` —
    /// uses `symbol` + `symbolSize`).
    #[default]
    Points,
    /// The edges of the points' Delaunay triangulation, coloured by value (silx
    /// `LINES` — uses `lineWidth`).
    Lines,
    /// The filled Delaunay triangles, coloured by value (silx `SOLID`).
    Solid,
}

/// A 2D scatter `(x, y, value)` placed in the 3D scene: the points lie on the
/// `z = 0` plane, or are lifted to `z = value` in height-map mode. The value
/// drives a [`Colormap`], and the data is drawn as markers, triangulation edges,
/// or filled triangles per its [`Scatter2DVisualization`].
///
/// Port of silx `plot3d.items.Scatter2D` (`DataItem3D` + `ColormapMixIn` +
/// `SymbolMixIn` + `ScatterVisualizationMixIn`). The LINES/SOLID modes triangulate
/// `(x, y)` with [`delaunay`] (silx's matplotlib `Triangulation`); as for
/// [`Scatter3D`] the value→colour mapping is done on the CPU via
/// [`Colormap::color_at`] rather than silx's GPU colormap texture. SOLID uses one
/// flat face normal per triangle (silx's per-triangle normals in height-map mode;
/// the `(0, 0, 1)` plane normal when flat).
#[derive(Clone, Debug)]
pub struct Scatter2D {
    x: Vec<f64>,
    y: Vec<f64>,
    values: Vec<f64>,
    colormap: Colormap,
    marker: PointMarker,
    size: f32,
    line_width: f32,
    height_map: bool,
    visualization: Scatter2DVisualization,
    transform: Item3DTransform,
}

impl Default for Scatter2D {
    fn default() -> Self {
        Self::new()
    }
}

impl Scatter2D {
    /// An empty scatter with silx defaults: the gray colormap over `[0, 1]`,
    /// circle markers at [`DEFAULT_SCATTER3D_SIZE`], POINTS visualization, flat
    /// (not height-map).
    pub fn new() -> Self {
        Self {
            x: Vec::new(),
            y: Vec::new(),
            values: Vec::new(),
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            marker: PointMarker::Circle,
            size: DEFAULT_SCATTER3D_SIZE,
            line_width: DEFAULT_SCATTER2D_LINE_WIDTH,
            height_map: false,
            visualization: Scatter2DVisualization::Points,
            transform: Item3DTransform::default(),
        }
    }

    /// Replace the point data (silx `Scatter2D.setData`). The three arrays must be
    /// the same length; on a length mismatch the data is left unchanged and
    /// `false` is returned (silx asserts equal lengths).
    pub fn set_data(&mut self, x: &[f64], y: &[f64], values: &[f64]) -> bool {
        let n = x.len();
        if y.len() != n || values.len() != n {
            return false;
        }
        self.x = x.to_vec();
        self.y = y.to_vec();
        self.values = values.to_vec();
        true
    }

    /// Builder form of [`set_data`](Self::set_data); a length mismatch leaves the
    /// data empty.
    pub fn with_data(mut self, x: &[f64], y: &[f64], values: &[f64]) -> Self {
        self.set_data(x, y, values);
        self
    }

    /// Set the colormap (silx `ColormapMixIn.setColormap`).
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
    }

    /// Builder form of [`set_colormap`](Self::set_colormap).
    pub fn with_colormap(mut self, colormap: Colormap) -> Self {
        self.colormap = colormap;
        self
    }

    /// Read-only access to the colormap.
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Mutable access to the colormap (e.g. to set the value range directly).
    pub fn colormap_mut(&mut self) -> &mut Colormap {
        &mut self.colormap
    }

    /// Fit the colormap's value range to the current data with `mode`, returning
    /// the new `(vmin, vmax)` (as [`Scatter3D::autoscale_colormap`]).
    pub fn autoscale_colormap(&mut self, mode: AutoscaleMode) -> (f64, f64) {
        let (vmin, vmax) = self.colormap.autoscale_range(mode, &self.values);
        self.colormap.vmin = vmin;
        self.colormap.vmax = vmax;
        (vmin, vmax)
    }

    /// Set the visualization mode (silx `ScatterVisualizationMixIn.setVisualization`).
    pub fn set_visualization(&mut self, visualization: Scatter2DVisualization) {
        self.visualization = visualization;
    }

    /// Builder form of [`set_visualization`](Self::set_visualization).
    pub fn with_visualization(mut self, visualization: Scatter2DVisualization) -> Self {
        self.visualization = visualization;
        self
    }

    /// The current visualization mode.
    pub fn visualization(&self) -> Scatter2DVisualization {
        self.visualization
    }

    /// Display the value as the `z` coordinate (silx `Scatter2D.setHeightMap`);
    /// when `false` the points lie on the `z = 0` plane.
    pub fn set_height_map(&mut self, height_map: bool) {
        self.height_map = height_map;
    }

    /// Builder form of [`set_height_map`](Self::set_height_map).
    pub fn with_height_map(mut self, height_map: bool) -> Self {
        self.height_map = height_map;
        self
    }

    /// Whether the value is displayed as a height map (silx `isHeightMap`).
    pub fn is_height_map(&self) -> bool {
        self.height_map
    }

    /// Set the marker shape used in POINTS mode (silx `SymbolMixIn.setSymbol`).
    pub fn set_marker(&mut self, marker: PointMarker) {
        self.marker = marker;
    }

    /// Builder form of [`set_marker`](Self::set_marker).
    pub fn with_marker(mut self, marker: PointMarker) -> Self {
        self.marker = marker;
        self
    }

    /// Set the marker size in pixels used in POINTS mode
    /// (silx `SymbolMixIn.setSymbolSize`), clamped to be non-negative.
    pub fn set_size(&mut self, size: f32) {
        self.size = size.max(0.0);
    }

    /// Builder form of [`set_size`](Self::set_size).
    pub fn with_size(mut self, size: f32) -> Self {
        self.set_size(size);
        self
    }

    /// Set the line width in pixels used in LINES mode (silx
    /// `Scatter2D.setLineWidth`), clamped to silx's `>= 1.0`.
    pub fn set_line_width(&mut self, width: f32) {
        self.line_width = width.max(1.0);
    }

    /// Builder form of [`set_line_width`](Self::set_line_width).
    pub fn with_line_width(mut self, width: f32) -> Self {
        self.set_line_width(width);
        self
    }

    /// The line width used in LINES mode.
    pub fn line_width(&self) -> f32 {
        self.line_width
    }

    /// Number of points.
    pub fn len(&self) -> usize {
        self.x.len()
    }

    /// True when there are no points.
    pub fn is_empty(&self) -> bool {
        self.x.is_empty()
    }

    /// The `z` coordinate of point `i`: its value in height-map mode, else the
    /// `z = 0` plane (silx `_updateScene`: `z = value if heightMap else 0.0`).
    fn z(&self, i: usize) -> f32 {
        if self.height_map {
            self.values[i] as f32
        } else {
            0.0
        }
    }

    /// World position of point `i`.
    fn position(&self, i: usize) -> [f32; 3] {
        [self.x[i] as f32, self.y[i] as f32, self.z(i)]
    }

    /// Linear-premultiplied RGBA of point `i` through the colormap.
    fn color_rgba(&self, i: usize) -> [f32; 4] {
        let [r, g, b, a] = self.colormap.color_at(self.values[i]);
        egui::Rgba::from(Color32::from_rgba_unmultiplied(r, g, b, a)).to_array()
    }

    /// Raw (untransformed) data bounds over the points.
    fn raw_bounds(&self) -> Option<(Vec3, Vec3)> {
        if self.is_empty() {
            return None;
        }
        let positions: Vec<[f32; 3]> = (0..self.len()).map(|i| self.position(i)).collect();
        positions_bounds(&positions)
    }

    /// Axis-aligned scene bounds `(min, max)` over the points, through the
    /// item's transform (silx `DataItem3D` bounds, `transformed=True`), or
    /// `None` when empty. In flat mode raw `z` collapses to `[0, 0]`; in
    /// height-map mode it spans the value range.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(self.raw_bounds())
    }

    /// Append this scatter's geometry to `geometry` per its visualization mode,
    /// ready to upload via [`crate::render::gpu_scene3d::set_scene3d`]. LINES and
    /// SOLID triangulate `(x, y)`; a degenerate input (fewer than 3 points or all
    /// collinear) yields an empty triangulation and so draws nothing, matching
    /// silx skipping the renderer when the Delaunay tesselation fails.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        if self.is_empty() {
            return;
        }
        append_with_transform(&self.transform, self.raw_bounds(), geometry, |g| {
            self.append_raw(g)
        });
    }

    /// Build the raw (untransformed) geometry — see [`Self::append_to`].
    fn append_raw(&self, geometry: &mut Scene3dGeometry) {
        match self.visualization {
            Scatter2DVisualization::Points => {
                for i in 0..self.len() {
                    let [r, g, b, a] = self.colormap.color_at(self.values[i]);
                    geometry.add_point(
                        self.position(i),
                        Color32::from_rgba_unmultiplied(r, g, b, a),
                        self.size,
                        self.marker,
                    );
                }
            }
            Scatter2DVisualization::Lines => {
                let tri = delaunay(&self.x, &self.y);
                // Unique undirected edges of the triangulation (silx
                // `triangleToLineIndices(unicity=True)`), sorted for determinism.
                let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
                for &[i0, i1, i2] in &tri.triangles {
                    for (a, b) in [(i0, i1), (i1, i2), (i2, i0)] {
                        edges.insert((a.min(b), a.max(b)));
                    }
                }
                for (a, b) in edges {
                    geometry.add_line_gradient(
                        self.position(a),
                        self.position(b),
                        self.color_rgba(a),
                        self.color_rgba(b),
                    );
                }
                // silx picks LINES mode at the data points (5 px square), not
                // along the segments (items/scatter.py:509-511 → _pickPoints
                // threshold=5.0); anchor every data point for the pick path.
                for i in 0..self.len() {
                    geometry.add_line_pick_anchor(self.position(i));
                }
            }
            Scatter2DVisualization::Solid => {
                let tri = delaunay(&self.x, &self.y);
                for &[i0, i1, i2] in &tri.triangles {
                    let p = [self.position(i0), self.position(i1), self.position(i2)];
                    let rgba = [
                        self.color_rgba(i0),
                        self.color_rgba(i1),
                        self.color_rgba(i2),
                    ];
                    let normal = flat_normal(p[0], p[1], p[2]);
                    geometry.add_mesh_triangle_rgba(p, rgba, [normal; 3]);
                }
            }
        }
    }
}

impl_item3d_transform!(Scatter2D);

/// How a mesh's flat vertex stream is grouped into triangles (silx
/// `Mesh.setData` `mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MeshDrawMode {
    /// Independent triangles: vertices `(0,1,2), (3,4,5), …`.
    #[default]
    Triangles,
    /// Triangle strip: vertices `(0,1,2), (1,2,3), (2,3,4), …`.
    TriangleStrip,
    /// Triangle fan: vertices `(0,1,2), (0,2,3), (0,3,4), …`.
    Fan,
}

/// Mesh vertex colouring (silx accepts a single colour or one colour per vertex).
#[derive(Clone, Debug)]
pub enum MeshColor {
    /// One colour shared by every vertex.
    Uniform(Color32),
    /// One colour per vertex (must match the vertex count).
    PerVertex(Vec<Color32>),
}

/// Expand a draw mode into triangles of *vertex indices*. When `indices` is given
/// the vertex stream is `indices` (unindexed); otherwise it is `0..n_vertices` in
/// order. Mirrors silx `utils.unindexArrays` + the per-mode reshape/expand in
/// `_MeshBase._pickFull` (triangle `i` uses stream `i, i+1, i+2` for strips; the
/// shared apex `0` plus `i, i+1` for fans). The single owner of mesh topology so
/// [`Mesh3D`] and [`ColormapMesh3D`] expand identically.
fn expand_triangles(
    mode: MeshDrawMode,
    n_vertices: usize,
    indices: Option<&[u32]>,
) -> Vec<[usize; 3]> {
    let stream: Vec<usize> = match indices {
        Some(idx) => idx.iter().map(|&i| i as usize).collect(),
        None => (0..n_vertices).collect(),
    };
    let n = stream.len();
    let mut tris = Vec::new();
    match mode {
        MeshDrawMode::Triangles => {
            for c in stream.chunks_exact(3) {
                tris.push([c[0], c[1], c[2]]);
            }
        }
        MeshDrawMode::TriangleStrip => {
            for i in 0..n.saturating_sub(2) {
                tris.push([stream[i], stream[i + 1], stream[i + 2]]);
            }
        }
        MeshDrawMode::Fan => {
            for i in 1..n.saturating_sub(1) {
                tris.push([stream[0], stream[i], stream[i + 1]]);
            }
        }
    }
    tris
}

/// Common length/range validation for mesh `setData`: per-vertex `normals` (if
/// any) must match the vertex count, and every `index` (if any) must be in range.
fn mesh_attrs_valid(n: usize, normals: Option<&[[f32; 3]]>, indices: Option<&[u32]>) -> bool {
    if let Some(ns) = normals
        && ns.len() != n
    {
        return false;
    }
    if let Some(idx) = indices
        && idx.iter().any(|&i| i as usize >= n)
    {
        return false;
    }
    true
}

/// Axis-aligned bounds `(min, max)` over a `(N, 3)` position array, or `None`
/// when empty (silx `DataItem3D.getBounds`).
fn positions_bounds(positions: &[[f32; 3]]) -> Option<(Vec3, Vec3)> {
    if positions.is_empty() {
        return None;
    }
    let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
    let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &[px, py, pz] in positions {
        min.x = min.x.min(px);
        min.y = min.y.min(py);
        min.z = min.z.min(pz);
        max.x = max.x.max(px);
        max.y = max.y.max(py);
        max.z = max.z.max(pz);
    }
    Some((min, max))
}

/// A triangle mesh with solid (per-vertex or uniform) vertex colours.
///
/// Port of silx `plot3d.items.Mesh` (a `DataItem3D` wrapping a
/// `scene.primitives.Mesh3D`). Vertices carry positions, colours and optional
/// normals; when no normals are supplied the geometric flat face normal is used
/// per triangle (via `flat_normal`), so the headlight still shades the surface.
/// Strips and fans are expanded to a triangle list on the CPU since the GPU path
/// is `TriangleList` only.
#[derive(Clone, Debug)]
pub struct Mesh3D {
    positions: Vec<[f32; 3]>,
    colors: MeshColor,
    normals: Option<Vec<[f32; 3]>>,
    mode: MeshDrawMode,
    indices: Option<Vec<u32>>,
    transform: Item3DTransform,
}

impl Default for Mesh3D {
    fn default() -> Self {
        Self::new()
    }
}

impl Mesh3D {
    /// An empty mesh (white, `Triangles` mode).
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
            colors: MeshColor::Uniform(Color32::WHITE),
            normals: None,
            mode: MeshDrawMode::Triangles,
            indices: None,
            transform: Item3DTransform::default(),
        }
    }

    /// Set the mesh geometry (silx `Mesh.setData`). Returns `false` (leaving the
    /// mesh unchanged) when the attributes are inconsistent: per-vertex colours or
    /// normals not matching the vertex count, or an out-of-range index. An empty
    /// `positions` clears the mesh and returns `true` (silx treats it as no mesh).
    pub fn set_data(
        &mut self,
        positions: &[[f32; 3]],
        colors: MeshColor,
        normals: Option<&[[f32; 3]]>,
        mode: MeshDrawMode,
        indices: Option<&[u32]>,
    ) -> bool {
        let n = positions.len();
        if let MeshColor::PerVertex(cs) = &colors
            && cs.len() != n
        {
            return false;
        }
        if !mesh_attrs_valid(n, normals, indices) {
            return false;
        }
        self.positions = positions.to_vec();
        self.colors = colors;
        self.normals = normals.map(<[[f32; 3]]>::to_vec);
        self.mode = mode;
        self.indices = indices.map(<[u32]>::to_vec);
        true
    }

    /// Builder form of [`set_data`](Self::set_data); inconsistent attributes leave
    /// the mesh empty.
    pub fn with_data(
        mut self,
        positions: &[[f32; 3]],
        colors: MeshColor,
        normals: Option<&[[f32; 3]]>,
        mode: MeshDrawMode,
        indices: Option<&[u32]>,
    ) -> Self {
        self.set_data(positions, colors, normals, mode, indices);
        self
    }

    /// The drawing mode.
    pub fn mode(&self) -> MeshDrawMode {
        self.mode
    }

    /// Number of vertices.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// True when there are no vertices.
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Axis-aligned scene bounds `(min, max)` through the item's transform
    /// (silx `DataItem3D` bounds, `transformed=True`), or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform
            .transform_bounds(positions_bounds(&self.positions))
    }

    /// Append this mesh's triangles to `geometry` for upload via
    /// [`crate::render::gpu_scene3d::set_scene3d`].
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        append_with_transform(
            &self.transform,
            positions_bounds(&self.positions),
            geometry,
            |g| {
                for [i0, i1, i2] in
                    expand_triangles(self.mode, self.positions.len(), self.indices.as_deref())
                {
                    let p = [self.positions[i0], self.positions[i1], self.positions[i2]];
                    let normals = match &self.normals {
                        Some(ns) => [ns[i0], ns[i1], ns[i2]],
                        None => [flat_normal(p[0], p[1], p[2]); 3],
                    };
                    let rgba = match &self.colors {
                        MeshColor::Uniform(c) => [egui::Rgba::from(*c).to_array(); 3],
                        MeshColor::PerVertex(cs) => [
                            egui::Rgba::from(cs[i0]).to_array(),
                            egui::Rgba::from(cs[i1]).to_array(),
                            egui::Rgba::from(cs[i2]).to_array(),
                        ],
                    };
                    g.add_mesh_triangle_rgba(p, rgba, normals);
                }
            },
        );
    }
}

impl_item3d_transform!(Mesh3D);

/// A triangle mesh whose vertex colours come from a per-vertex scalar `value`
/// mapped through a [`Colormap`].
///
/// Port of silx `plot3d.items.ColormapMesh` (`_MeshBase` + `ColormapMixIn`,
/// wrapping a `scene.primitives.ColormapMesh3D`). silx maps values to colours on
/// the GPU from a colormap texture; here the mapping is done on the CPU via
/// [`Colormap::color_at`] when building the geometry (as for [`Scatter3D`]).
#[derive(Clone, Debug)]
pub struct ColormapMesh3D {
    positions: Vec<[f32; 3]>,
    values: Vec<f64>,
    normals: Option<Vec<[f32; 3]>>,
    mode: MeshDrawMode,
    indices: Option<Vec<u32>>,
    colormap: Colormap,
    transform: Item3DTransform,
}

impl Default for ColormapMesh3D {
    fn default() -> Self {
        Self::new()
    }
}

impl ColormapMesh3D {
    /// An empty colormap mesh with silx defaults: the gray colormap over
    /// `[0, 1]`, `Triangles` mode.
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
            values: Vec::new(),
            normals: None,
            mode: MeshDrawMode::Triangles,
            indices: None,
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            transform: Item3DTransform::default(),
        }
    }

    /// Set the mesh geometry (silx `ColormapMesh.setData`). Returns `false`
    /// (leaving the mesh unchanged) when `values`, per-vertex `normals`, or
    /// `indices` are inconsistent with the vertex count. An empty `positions`
    /// clears the mesh and returns `true`.
    pub fn set_data(
        &mut self,
        positions: &[[f32; 3]],
        values: &[f64],
        normals: Option<&[[f32; 3]]>,
        mode: MeshDrawMode,
        indices: Option<&[u32]>,
    ) -> bool {
        let n = positions.len();
        if values.len() != n {
            return false;
        }
        if !mesh_attrs_valid(n, normals, indices) {
            return false;
        }
        self.positions = positions.to_vec();
        self.values = values.to_vec();
        self.normals = normals.map(<[[f32; 3]]>::to_vec);
        self.mode = mode;
        self.indices = indices.map(<[u32]>::to_vec);
        true
    }

    /// Builder form of [`set_data`](Self::set_data); inconsistent attributes leave
    /// the mesh empty.
    pub fn with_data(
        mut self,
        positions: &[[f32; 3]],
        values: &[f64],
        normals: Option<&[[f32; 3]]>,
        mode: MeshDrawMode,
        indices: Option<&[u32]>,
    ) -> Self {
        self.set_data(positions, values, normals, mode, indices);
        self
    }

    /// Set the colormap (silx `ColormapMixIn.setColormap`).
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
    }

    /// Builder form of [`set_colormap`](Self::set_colormap).
    pub fn with_colormap(mut self, colormap: Colormap) -> Self {
        self.colormap = colormap;
        self
    }

    /// Read-only access to the colormap.
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Mutable access to the colormap.
    pub fn colormap_mut(&mut self) -> &mut Colormap {
        &mut self.colormap
    }

    /// Fit the colormap's value range to the current data with `mode`, returning
    /// the new `(vmin, vmax)` (as [`Scatter3D::autoscale_colormap`]).
    pub fn autoscale_colormap(&mut self, mode: AutoscaleMode) -> (f64, f64) {
        let (vmin, vmax) = self.colormap.autoscale_range(mode, &self.values);
        self.colormap.vmin = vmin;
        self.colormap.vmax = vmax;
        (vmin, vmax)
    }

    /// The drawing mode.
    pub fn mode(&self) -> MeshDrawMode {
        self.mode
    }

    /// Number of vertices.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// True when there are no vertices.
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Axis-aligned scene bounds `(min, max)` through the item's transform
    /// (silx `DataItem3D` bounds, `transformed=True`), or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform
            .transform_bounds(positions_bounds(&self.positions))
    }

    /// Append this mesh's triangles (coloured through the colormap) to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        append_with_transform(
            &self.transform,
            positions_bounds(&self.positions),
            geometry,
            |g| {
                let rgba_at = |i: usize| {
                    let [r, gr, b, a] = self.colormap.color_at(self.values[i]);
                    egui::Rgba::from(Color32::from_rgba_unmultiplied(r, gr, b, a)).to_array()
                };
                for [i0, i1, i2] in
                    expand_triangles(self.mode, self.positions.len(), self.indices.as_deref())
                {
                    let p = [self.positions[i0], self.positions[i1], self.positions[i2]];
                    let normals = match &self.normals {
                        Some(ns) => [ns[i0], ns[i1], ns[i2]],
                        None => [flat_normal(p[0], p[1], p[2]); 3],
                    };
                    g.add_mesh_triangle_rgba(p, [rgba_at(i0), rgba_at(i1), rgba_at(i2)], normals);
                }
            },
        );
    }
}

impl_item3d_transform!(ColormapMesh3D);

/// Build the rotation matrix for a silx `Rotate(angle_deg, x, y, z)`: degrees →
/// radians about the normalized axis. A zero angle or zero axis is the identity
/// (silx's default `(0, (0,0,0))`).
fn rotation_matrix(angle_deg: f32, axis: [f32; 3]) -> Mat4 {
    let a = Vec3::from_array(axis);
    let len = a.length();
    if angle_deg == 0.0 || len == 0.0 {
        return Mat4::IDENTITY;
    }
    let n = a * (1.0 / len);
    mat4_rotate(angle_deg.to_radians(), n.x, n.y, n.z)
}

/// `numpy.linspace(0, 2π, n_seg + 1)`: `n_seg` equal angular segments closing the
/// full turn (the edge angles of a [`_cylindrical_volume_mesh`]).
fn linspace_angles(n_seg: usize) -> Vec<f32> {
    (0..=n_seg)
        .map(|i| std::f32::consts::TAU * i as f32 / n_seg as f32)
        .collect()
}

/// Build the triangle mesh of a rotational volume swept around z — the port of
/// silx `items.mesh._CylindricalVolume._setData`.
///
/// For each angular segment `[angles[i], angles[i+1]]` a 12-vertex / 4-triangle
/// wedge is built (bottom cap, two side triangles, top cap) from the six corners
/// `c1..c6` (centres ±h/2 and the two radial edge points top & bottom), each
/// passed through `rotation`. With `flat_faces` every vertex gets its triangle's
/// geometric normal (faceted, for Box/Hexagon); otherwise the side vertices get
/// radial normals (smooth, for Cylinder) while the caps stay faceted. The wedge
/// set is then replicated and translated to each centre `position`; `color` is
/// one shared colour (`len == 1`) or one per position. Vertex normals reproduce
/// silx's expressions; silx's one degenerate term `(c6−c5)×(c5−c5)` is written as
/// the zero vector it always evaluates to (`c5−c5 = 0`).
fn cylindrical_volume_mesh(
    positions: &[[f32; 3]],
    radius: f32,
    height: f32,
    angles: &[f32],
    color: &[Color32],
    flat_faces: bool,
    rotation: Mat4,
) -> Mesh3D {
    if positions.is_empty() || angles.len() < 2 {
        return Mesh3D::new();
    }
    let n_seg = angles.len() - 1;
    let hz = height / 2.0;
    let edge = |r: f32, a: f32, z: f32| {
        rotation.transform_point(Vec3::new(r * a.cos(), r * a.sin(), z), false)
    };

    // One wedge set (shared by every position), as in silx's `volume`/`normal`.
    let mut wedge_verts: Vec<Vec3> = Vec::with_capacity(n_seg * 12);
    let mut wedge_normals: Vec<Vec3> = Vec::with_capacity(n_seg * 12);
    for i in 0..n_seg {
        let (a0, a1) = (angles[i], angles[i + 1]);
        let c1 = rotation.transform_point(Vec3::new(0.0, 0.0, -hz), false);
        let c2 = edge(radius, a0, -hz);
        let c3 = edge(radius, a1, -hz);
        let c4 = edge(radius, a0, hz);
        let c5 = edge(radius, a1, hz);
        let c6 = rotation.transform_point(Vec3::new(0.0, 0.0, hz), false);
        wedge_verts.extend_from_slice(&[c1, c3, c2, c2, c3, c4, c3, c5, c4, c4, c5, c6]);
        if flat_faces {
            wedge_normals.extend_from_slice(&[
                (c3 - c1).cross(c2 - c1),
                (c2 - c3).cross(c1 - c3),
                (c1 - c2).cross(c3 - c2),
                (c3 - c2).cross(c4 - c2),
                (c4 - c3).cross(c2 - c3),
                (c2 - c4).cross(c3 - c4),
                (c5 - c3).cross(c4 - c3),
                (c4 - c5).cross(c3 - c5),
                (c3 - c4).cross(c5 - c4),
                (c5 - c4).cross(c6 - c4),
                Vec3::new(0.0, 0.0, 0.0), // silx `cross(c6-c5, c5-c5)` ≡ 0
                (c4 - c6).cross(c5 - c6),
            ]);
        } else {
            wedge_normals.extend_from_slice(&[
                (c3 - c1).cross(c2 - c1),
                (c2 - c3).cross(c1 - c3),
                (c1 - c2).cross(c3 - c2),
                c2 - c1,
                c3 - c1,
                c4 - c6,
                c3 - c1,
                c5 - c6,
                c4 - c6,
                (c5 - c4).cross(c6 - c4),
                Vec3::new(0.0, 0.0, 0.0), // silx `cross(c6-c5, c5-c5)` ≡ 0
                (c4 - c6).cross(c5 - c6),
            ]);
        }
    }

    let total = wedge_verts.len() * positions.len();
    let mut out_pos = Vec::with_capacity(total);
    let mut out_norm = Vec::with_capacity(total);
    let mut out_color = Vec::with_capacity(total);
    for (k, &p) in positions.iter().enumerate() {
        let pv = Vec3::from_array(p);
        let color_k = if color.len() == 1 { color[0] } else { color[k] };
        for (v, n) in wedge_verts.iter().zip(&wedge_normals) {
            out_pos.push((*v + pv).to_array());
            out_norm.push(n.to_array());
            out_color.push(color_k);
        }
    }

    Mesh3D::new().with_data(
        &out_pos,
        MeshColor::PerVertex(out_color),
        Some(&out_norm),
        MeshDrawMode::Triangles,
        None,
    )
}

/// True when `color` is one shared colour or exactly one per position (silx
/// asserts `ndim(color) == 1 or len(color) == len(position)`).
fn volume_color_valid(color: &[Color32], n_positions: usize) -> bool {
    color.len() == 1 || color.len() == n_positions
}

/// One or many axis-aligned boxes (silx `items.mesh.Box`), a four-segment
/// `cylindrical_volume_mesh` with faceted faces.
#[derive(Clone, Debug)]
pub struct Box3D {
    size: [f32; 3],
    colors: Vec<Color32>,
    positions: Vec<[f32; 3]>,
    mesh: Mesh3D,
}

impl Default for Box3D {
    fn default() -> Self {
        Self::new()
    }
}

impl Box3D {
    /// A single unit box at the origin, white (silx `Box` defaults).
    pub fn new() -> Self {
        let mut b = Self {
            size: [1.0, 1.0, 1.0],
            colors: vec![Color32::WHITE],
            positions: vec![[0.0, 0.0, 0.0]],
            mesh: Mesh3D::new(),
        };
        b.rebuild((0.0, [0.0, 0.0, 0.0]));
        b
    }

    /// Set box geometry (silx `Box.setData`): `size` (dx, dy, dz), `color` (one
    /// shared or one per box), `positions` (box centres), and `rotation`
    /// `(angle_degrees, axis)`. Returns `false` (unchanged) on an invalid colour
    /// count.
    pub fn set_data(
        &mut self,
        size: [f32; 3],
        color: &[Color32],
        positions: &[[f32; 3]],
        rotation: (f32, [f32; 3]),
    ) -> bool {
        if !volume_color_valid(color, positions.len()) {
            return false;
        }
        self.size = size;
        self.colors = color.to_vec();
        self.positions = positions.to_vec();
        self.rebuild(rotation);
        true
    }

    fn rebuild(&mut self, rotation: (f32, [f32; 3])) {
        let [dx, dy, dz] = self.size;
        // silx Box.setData: four side faces whose edge angles are derived from the
        // box aspect ratio, then shifted by −α/2 so a face aligns with +x.
        let diagonal = (dx * dx + dy * dy).sqrt();
        let alpha = 2.0 * (dy / diagonal).asin();
        let beta = 2.0 * (dx / diagonal).asin();
        let angles: Vec<f32> = [
            0.0,
            alpha,
            alpha + beta,
            alpha + beta + alpha,
            std::f32::consts::TAU,
        ]
        .iter()
        .map(|a| a - 0.5 * alpha)
        .collect();
        // The item transform lives on the inner mesh; carry it across rebuilds.
        let transform = *self.mesh.transform();
        self.mesh = cylindrical_volume_mesh(
            &self.positions,
            diagonal / 2.0,
            dz,
            &angles,
            &self.colors,
            true,
            rotation_matrix(rotation.0, rotation.1),
        );
        *self.mesh.transform_mut() = transform;
    }

    /// Box centre position(s).
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Box size (dx, dy, dz).
    pub fn size(&self) -> [f32; 3] {
        self.size
    }

    /// Box colour(s).
    pub fn colors(&self) -> &[Color32] {
        &self.colors
    }

    /// Axis-aligned bounds `(min, max)` of the box mesh (through the item's
    /// transform), or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.mesh.bounds()
    }

    /// Append the box triangles to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.mesh.append_to(geometry);
    }

    /// The item's transform stack (silx `DataItem3D` transforms,
    /// `items/core.py:288-315`), delegated to the underlying mesh so bounds
    /// and geometry follow it by construction.
    pub fn transform(&self) -> &Item3DTransform {
        self.mesh.transform()
    }

    /// Mutable access to the transform stack (silx setters,
    /// `items/core.py:335-485`).
    pub fn transform_mut(&mut self) -> &mut Item3DTransform {
        self.mesh.transform_mut()
    }
}

/// One or many cylinders (silx `items.mesh.Cylinder`), an `nb_faces`-segment
/// `cylindrical_volume_mesh` with smooth (radial-normal) sides.
#[derive(Clone, Debug)]
pub struct Cylinder3D {
    radius: f32,
    height: f32,
    nb_faces: usize,
    colors: Vec<Color32>,
    positions: Vec<[f32; 3]>,
    mesh: Mesh3D,
}

impl Default for Cylinder3D {
    fn default() -> Self {
        Self::new()
    }
}

impl Cylinder3D {
    /// A single unit cylinder at the origin (radius 1, height 1, 20 faces, white).
    pub fn new() -> Self {
        let mut c = Self {
            radius: 1.0,
            height: 1.0,
            nb_faces: 20,
            colors: vec![Color32::WHITE],
            positions: vec![[0.0, 0.0, 0.0]],
            mesh: Mesh3D::new(),
        };
        c.rebuild((0.0, [0.0, 0.0, 0.0]));
        c
    }

    /// Set cylinder geometry (silx `Cylinder.setData`): `radius`, `height`,
    /// `color` (one shared or one per cylinder), `nb_faces` (≥3 for a closed
    /// surface), `positions` (centres), `rotation` `(angle_degrees, axis)`.
    /// Returns `false` (unchanged) on an invalid colour count.
    pub fn set_data(
        &mut self,
        radius: f32,
        height: f32,
        color: &[Color32],
        nb_faces: usize,
        positions: &[[f32; 3]],
        rotation: (f32, [f32; 3]),
    ) -> bool {
        if !volume_color_valid(color, positions.len()) {
            return false;
        }
        self.radius = radius;
        self.height = height;
        self.nb_faces = nb_faces;
        self.colors = color.to_vec();
        self.positions = positions.to_vec();
        self.rebuild(rotation);
        true
    }

    fn rebuild(&mut self, rotation: (f32, [f32; 3])) {
        let angles = linspace_angles(self.nb_faces);
        // The item transform lives on the inner mesh; carry it across rebuilds.
        let transform = *self.mesh.transform();
        self.mesh = cylindrical_volume_mesh(
            &self.positions,
            self.radius,
            self.height,
            &angles,
            &self.colors,
            false,
            rotation_matrix(rotation.0, rotation.1),
        );
        *self.mesh.transform_mut() = transform;
    }

    /// Cylinder centre position(s).
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Cylinder radius.
    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// Cylinder height.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Cylinder colour(s).
    pub fn colors(&self) -> &[Color32] {
        &self.colors
    }

    /// Axis-aligned bounds `(min, max)` of the cylinder mesh (through the
    /// item's transform), or `None` if empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.mesh.bounds()
    }

    /// Append the cylinder triangles to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.mesh.append_to(geometry);
    }

    /// The item's transform stack (silx `DataItem3D` transforms,
    /// `items/core.py:288-315`), delegated to the underlying mesh so bounds
    /// and geometry follow it by construction.
    pub fn transform(&self) -> &Item3DTransform {
        self.mesh.transform()
    }

    /// Mutable access to the transform stack (silx setters,
    /// `items/core.py:335-485`).
    pub fn transform_mut(&mut self) -> &mut Item3DTransform {
        self.mesh.transform_mut()
    }
}

/// One or many uniform hexagonal prisms (silx `items.mesh.Hexagon`), a
/// six-segment `cylindrical_volume_mesh` with faceted faces.
#[derive(Clone, Debug)]
pub struct Hexagon3D {
    radius: f32,
    height: f32,
    colors: Vec<Color32>,
    positions: Vec<[f32; 3]>,
    mesh: Mesh3D,
}

impl Default for Hexagon3D {
    fn default() -> Self {
        Self::new()
    }
}

impl Hexagon3D {
    /// A single unit hexagonal prism at the origin (radius 1, height 1, white).
    pub fn new() -> Self {
        let mut h = Self {
            radius: 1.0,
            height: 1.0,
            colors: vec![Color32::WHITE],
            positions: vec![[0.0, 0.0, 0.0]],
            mesh: Mesh3D::new(),
        };
        h.rebuild((0.0, [0.0, 0.0, 0.0]));
        h
    }

    /// Set hexagonal-prism geometry (silx `Hexagon.setData`): external `radius`,
    /// `height`, `color` (one shared or one per prism), `positions` (centres),
    /// `rotation` `(angle_degrees, axis)`. Returns `false` (unchanged) on an
    /// invalid colour count.
    pub fn set_data(
        &mut self,
        radius: f32,
        height: f32,
        color: &[Color32],
        positions: &[[f32; 3]],
        rotation: (f32, [f32; 3]),
    ) -> bool {
        if !volume_color_valid(color, positions.len()) {
            return false;
        }
        self.radius = radius;
        self.height = height;
        self.colors = color.to_vec();
        self.positions = positions.to_vec();
        self.rebuild(rotation);
        true
    }

    fn rebuild(&mut self, rotation: (f32, [f32; 3])) {
        // silx Hexagon.setData: angles = linspace(0, 2π, 7) → six faces.
        let angles = linspace_angles(6);
        // The item transform lives on the inner mesh; carry it across rebuilds.
        let transform = *self.mesh.transform();
        self.mesh = cylindrical_volume_mesh(
            &self.positions,
            self.radius,
            self.height,
            &angles,
            &self.colors,
            true,
            rotation_matrix(rotation.0, rotation.1),
        );
        *self.mesh.transform_mut() = transform;
    }

    /// Prism centre position(s).
    pub fn positions(&self) -> &[[f32; 3]] {
        &self.positions
    }

    /// Prism external radius.
    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// Prism height.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Prism colour(s).
    pub fn colors(&self) -> &[Color32] {
        &self.colors
    }

    /// Axis-aligned bounds `(min, max)` of the prism mesh (through the item's
    /// transform), or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.mesh.bounds()
    }

    /// Append the prism triangles to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.mesh.append_to(geometry);
    }

    /// The item's transform stack (silx `DataItem3D` transforms,
    /// `items/core.py:288-315`), delegated to the underlying mesh so bounds
    /// and geometry follow it by construction.
    pub fn transform(&self) -> &Item3DTransform {
        self.mesh.transform()
    }

    /// Mutable access to the transform stack (silx setters,
    /// `items/core.py:335-485`).
    pub fn transform_mut(&mut self) -> &mut Item3DTransform {
        self.mesh.transform_mut()
    }
}

/// Premultiplied-linear RGBA8 for a [`Color32`] — the image-layer pixel format
/// (same linear/premultiplied convention as the geometry colour path, so an
/// image's sampled colour matches a triangle of the same `Color32`).
fn premul_linear_rgba8(c: Color32) -> [u8; 4] {
    let [r, g, b, a] = egui::Rgba::from(c).to_array();
    [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
        (a * 255.0).round() as u8,
    ]
}

/// World bounds `(min, max)` of an image quad of `width × height` pixels at
/// `origin` with per-pixel `scale`, in the `z = origin.z` plane, or `None` when
/// empty.
fn image_bounds(
    width: usize,
    height: usize,
    origin: [f32; 3],
    scale: [f32; 2],
) -> Option<(Vec3, Vec3)> {
    if width == 0 || height == 0 {
        return None;
    }
    let min = Vec3::from_array(origin);
    let max = Vec3::new(
        origin[0] + width as f32 * scale[0],
        origin[1] + height as f32 * scale[1],
        origin[2],
    );
    Some((min, max))
}

/// A 2D scalar image displayed as a flat colormapped quad (silx
/// `plot3d.items.ImageData`). The data is a row-major `width × height` array;
/// each pixel is coloured through a [`Colormap`] (CPU [`Colormap::color_at`], as
/// for the other colormapped 3D items) into one image-layer texture.
#[derive(Clone, Debug)]
pub struct ImageData3D {
    data: Vec<f64>,
    width: usize,
    height: usize,
    colormap: Colormap,
    origin: [f32; 3],
    scale: [f32; 2],
    interpolation: ImageInterpolation,
    transform: Item3DTransform,
}

impl Default for ImageData3D {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageData3D {
    /// An empty image with silx defaults: gray over `[0, 1]`, origin `(0,0,0)`,
    /// unit pixel scale, nearest sampling.
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            width: 0,
            height: 0,
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            origin: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0],
            interpolation: ImageInterpolation::Nearest,
            transform: Item3DTransform::default(),
        }
    }

    /// Set the scalar image data (silx `ImageData.setData`), row-major. Returns
    /// `false` (unchanged) when `data.len() != width * height`.
    pub fn set_data(&mut self, data: &[f64], width: usize, height: usize) -> bool {
        if data.len() != width * height {
            return false;
        }
        self.data = data.to_vec();
        self.width = width;
        self.height = height;
        true
    }

    /// Builder form of [`set_data`](Self::set_data).
    pub fn with_data(mut self, data: &[f64], width: usize, height: usize) -> Self {
        self.set_data(data, width, height);
        self
    }

    /// Set the colormap.
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
    }

    /// Builder form of [`set_colormap`](Self::set_colormap).
    pub fn with_colormap(mut self, colormap: Colormap) -> Self {
        self.colormap = colormap;
        self
    }

    /// Read-only access to the colormap.
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Mutable access to the colormap.
    pub fn colormap_mut(&mut self) -> &mut Colormap {
        &mut self.colormap
    }

    /// Fit the colormap's value range to the current data with `mode`, returning
    /// the new `(vmin, vmax)`.
    pub fn autoscale_colormap(&mut self, mode: AutoscaleMode) -> (f64, f64) {
        let (vmin, vmax) = self.colormap.autoscale_range(mode, &self.data);
        self.colormap.vmin = vmin;
        self.colormap.vmax = vmax;
        (vmin, vmax)
    }

    /// Set the world position of pixel-corner `(0, 0)`.
    pub fn set_origin(&mut self, origin: [f32; 3]) {
        self.origin = origin;
    }

    /// Builder form of [`set_origin`](Self::set_origin).
    pub fn with_origin(mut self, origin: [f32; 3]) -> Self {
        self.origin = origin;
        self
    }

    /// Set the world size of one pixel along x and y.
    pub fn set_scale(&mut self, scale: [f32; 2]) {
        self.scale = scale;
    }

    /// Builder form of [`set_scale`](Self::set_scale).
    pub fn with_scale(mut self, scale: [f32; 2]) -> Self {
        self.scale = scale;
        self
    }

    /// Set the texture filtering.
    pub fn set_interpolation(&mut self, interpolation: ImageInterpolation) {
        self.interpolation = interpolation;
    }

    /// Builder form of [`set_interpolation`](Self::set_interpolation).
    pub fn with_interpolation(mut self, interpolation: ImageInterpolation) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// Image dimensions `(width, height)` in pixels.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// True when there is no image data.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// World bounds `(min, max)` of the image quad through the item's
    /// transform (silx `DataItem3D` bounds, `transformed=True`), or `None`
    /// when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(image_bounds(
            self.width,
            self.height,
            self.origin,
            self.scale,
        ))
    }

    /// Append this image as a colormapped layer to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        if self.is_empty() {
            return;
        }
        let raw_bounds = image_bounds(self.width, self.height, self.origin, self.scale);
        append_with_transform(&self.transform, raw_bounds, geometry, |g| {
            let mut pixels = Vec::with_capacity(self.data.len() * 4);
            for &v in &self.data {
                let [r, gr, b, a] = self.colormap.color_at(v);
                pixels.extend_from_slice(&premul_linear_rgba8(Color32::from_rgba_unmultiplied(
                    r, gr, b, a,
                )));
            }
            g.add_image_layer(Scene3dImageLayer {
                pixels,
                width: self.width as u32,
                height: self.height as u32,
                origin: self.origin,
                scale: self.scale,
                interpolation: self.interpolation,
            });
        });
    }
}

impl_item3d_transform!(ImageData3D);

/// A 2D RGB(A) image displayed as a flat quad (silx `plot3d.items.ImageRgba`).
/// Pixels are given directly as [`Color32`] (row-major); no colormap.
#[derive(Clone, Debug)]
pub struct ImageRgba3D {
    pixels: Vec<Color32>,
    width: usize,
    height: usize,
    origin: [f32; 3],
    scale: [f32; 2],
    interpolation: ImageInterpolation,
    transform: Item3DTransform,
}

impl Default for ImageRgba3D {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageRgba3D {
    /// An empty RGBA image with silx defaults: origin `(0,0,0)`, unit pixel scale,
    /// nearest sampling.
    pub fn new() -> Self {
        Self {
            pixels: Vec::new(),
            width: 0,
            height: 0,
            origin: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0],
            interpolation: ImageInterpolation::Nearest,
            transform: Item3DTransform::default(),
        }
    }

    /// Set the RGBA image data (silx `ImageRgba.setData`), row-major. Returns
    /// `false` (unchanged) when `pixels.len() != width * height`.
    pub fn set_data(&mut self, pixels: &[Color32], width: usize, height: usize) -> bool {
        if pixels.len() != width * height {
            return false;
        }
        self.pixels = pixels.to_vec();
        self.width = width;
        self.height = height;
        true
    }

    /// Builder form of [`set_data`](Self::set_data).
    pub fn with_data(mut self, pixels: &[Color32], width: usize, height: usize) -> Self {
        self.set_data(pixels, width, height);
        self
    }

    /// Set the world position of pixel-corner `(0, 0)`.
    pub fn set_origin(&mut self, origin: [f32; 3]) {
        self.origin = origin;
    }

    /// Builder form of [`set_origin`](Self::set_origin).
    pub fn with_origin(mut self, origin: [f32; 3]) -> Self {
        self.origin = origin;
        self
    }

    /// Set the world size of one pixel along x and y.
    pub fn set_scale(&mut self, scale: [f32; 2]) {
        self.scale = scale;
    }

    /// Builder form of [`set_scale`](Self::set_scale).
    pub fn with_scale(mut self, scale: [f32; 2]) -> Self {
        self.scale = scale;
        self
    }

    /// Set the texture filtering.
    pub fn set_interpolation(&mut self, interpolation: ImageInterpolation) {
        self.interpolation = interpolation;
    }

    /// Builder form of [`set_interpolation`](Self::set_interpolation).
    pub fn with_interpolation(mut self, interpolation: ImageInterpolation) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// Image dimensions `(width, height)` in pixels.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// True when there is no image data.
    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// World bounds `(min, max)` of the image quad through the item's
    /// transform (silx `DataItem3D` bounds, `transformed=True`), or `None`
    /// when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(image_bounds(
            self.width,
            self.height,
            self.origin,
            self.scale,
        ))
    }

    /// Append this image as an RGBA layer to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        if self.is_empty() {
            return;
        }
        let raw_bounds = image_bounds(self.width, self.height, self.origin, self.scale);
        append_with_transform(&self.transform, raw_bounds, geometry, |g| {
            let mut pixels = Vec::with_capacity(self.pixels.len() * 4);
            for &c in &self.pixels {
                pixels.extend_from_slice(&premul_linear_rgba8(c));
            }
            g.add_image_layer(Scene3dImageLayer {
                pixels,
                width: self.width as u32,
                height: self.height as u32,
                origin: self.origin,
                scale: self.scale,
                interpolation: self.interpolation,
            });
        });
    }
}

impl_item3d_transform!(ImageRgba3D);

/// Nearest-neighbour source index for destination index `i` of `dst_len`, onto a
/// source axis of `src_len` (the silx height-map resample, `floor(i·src/dst)`),
/// clamped into range.
fn nearest_src_index(i: usize, dst_len: usize, src_len: usize) -> usize {
    ((i as f64 * src_len as f64 / dst_len as f64).floor() as usize).min(src_len.saturating_sub(1))
}

/// World bounds `(min, max)` of a height-field point grid: x ∈ [0, width−1],
/// y ∈ [0, height−1], z over the height values. `None` when empty.
fn height_grid_bounds(heights: &[f32], width: usize, height: usize) -> Option<(Vec3, Vec3)> {
    if heights.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let mut zmin = f32::INFINITY;
    let mut zmax = f32::NEG_INFINITY;
    for &z in heights {
        zmin = zmin.min(z);
        zmax = zmax.max(z);
    }
    Some((
        Vec3::new(0.0, 0.0, zmin),
        Vec3::new((width - 1) as f32, (height - 1) as f32, zmax),
    ))
}

/// A 2D height field coloured by a colormapped dataset (silx
/// `plot3d.items.HeightMapData`). Each height-field pixel `(row, col)` becomes a
/// square point at world `(col, row, height)`, coloured through a [`Colormap`]
/// over the (separately set) `colormapped` data — silx renders height maps as a
/// set of size-1 `'s'` points, so this reuses the point-sprite path directly.
///
/// When the colormapped data and the height field differ in size the data is
/// nearest-neighbour resampled to the height grid. (silx's resample indexes the
/// *column* axis by the field *height* — image.py:318 — which mis-samples
/// non-square data; this port indexes the column by the field *width*, the
/// evident intent. For equal-sized data the two agree.)
#[derive(Clone, Debug)]
pub struct HeightMapData {
    heights: Vec<f32>,
    h_width: usize,
    h_height: usize,
    values: Vec<f64>,
    v_width: usize,
    v_height: usize,
    colormap: Colormap,
    transform: Item3DTransform,
}

impl Default for HeightMapData {
    fn default() -> Self {
        Self::new()
    }
}

impl HeightMapData {
    /// An empty height map with gray over `[0, 1]`.
    pub fn new() -> Self {
        Self {
            heights: Vec::new(),
            h_width: 0,
            h_height: 0,
            values: Vec::new(),
            v_width: 0,
            v_height: 0,
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            transform: Item3DTransform::default(),
        }
    }

    /// Set the height field (silx `_HeightMap.setData`), row-major. Returns `false`
    /// (unchanged) when `heights.len() != width * height`.
    pub fn set_data(&mut self, heights: &[f32], width: usize, height: usize) -> bool {
        if heights.len() != width * height {
            return false;
        }
        self.heights = heights.to_vec();
        self.h_width = width;
        self.h_height = height;
        true
    }

    /// Builder form of [`set_data`](Self::set_data).
    pub fn with_data(mut self, heights: &[f32], width: usize, height: usize) -> Self {
        self.set_data(heights, width, height);
        self
    }

    /// Set the colormapped data (silx `HeightMapData.setColormappedData`),
    /// row-major. May differ in size from the height field (nearest-neighbour
    /// resampled). Returns `false` when `data.len() != width * height`.
    pub fn set_colormapped_data(&mut self, data: &[f64], width: usize, height: usize) -> bool {
        if data.len() != width * height {
            return false;
        }
        self.values = data.to_vec();
        self.v_width = width;
        self.v_height = height;
        true
    }

    /// Builder form of [`set_colormapped_data`](Self::set_colormapped_data).
    pub fn with_colormapped_data(mut self, data: &[f64], width: usize, height: usize) -> Self {
        self.set_colormapped_data(data, width, height);
        self
    }

    /// Set the colormap.
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
    }

    /// Builder form of [`set_colormap`](Self::set_colormap).
    pub fn with_colormap(mut self, colormap: Colormap) -> Self {
        self.colormap = colormap;
        self
    }

    /// Read-only access to the colormap.
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Mutable access to the colormap.
    pub fn colormap_mut(&mut self) -> &mut Colormap {
        &mut self.colormap
    }

    /// Fit the colormap's value range to the colormapped data with `mode`.
    pub fn autoscale_colormap(&mut self, mode: AutoscaleMode) -> (f64, f64) {
        let (vmin, vmax) = self.colormap.autoscale_range(mode, &self.values);
        self.colormap.vmin = vmin;
        self.colormap.vmax = vmax;
        (vmin, vmax)
    }

    /// Height-field dimensions `(width, height)`.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.h_width, self.h_height)
    }

    /// True when nothing would be drawn (no height field or no colour data).
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty() || self.values.is_empty()
    }

    /// World bounds `(min, max)` of the height-field point grid through the
    /// item's transform (silx `DataItem3D` bounds, `transformed=True`), or
    /// `None` when the height field is empty (independent of whether colour
    /// data is set).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(height_grid_bounds(
            &self.heights,
            self.h_width,
            self.h_height,
        ))
    }

    /// Append the height field as colormapped square points to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        if self.is_empty() {
            return;
        }
        let raw_bounds = height_grid_bounds(&self.heights, self.h_width, self.h_height);
        append_with_transform(&self.transform, raw_bounds, geometry, |g| {
            for row in 0..self.h_height {
                let vr = nearest_src_index(row, self.h_height, self.v_height);
                for col in 0..self.h_width {
                    let vc = nearest_src_index(col, self.h_width, self.v_width);
                    let z = self.heights[row * self.h_width + col];
                    let [r, gr, b, a] = self.colormap.color_at(self.values[vr * self.v_width + vc]);
                    g.add_point(
                        [col as f32, row as f32, z],
                        Color32::from_rgba_unmultiplied(r, gr, b, a),
                        1.0,
                        PointMarker::Square,
                    );
                }
            }
        });
    }
}

impl_item3d_transform!(HeightMapData);

/// A 2D height field coloured by an RGB(A) image (silx
/// `plot3d.items.HeightMapRGBA`). Like [`HeightMapData`] but each square point is
/// coloured directly by the (separately set, nearest-neighbour resampled) image
/// pixel rather than through a colormap.
#[derive(Clone, Debug)]
pub struct HeightMapRGBA {
    heights: Vec<f32>,
    h_width: usize,
    h_height: usize,
    colors: Vec<Color32>,
    c_width: usize,
    c_height: usize,
    transform: Item3DTransform,
}

impl Default for HeightMapRGBA {
    fn default() -> Self {
        Self::new()
    }
}

impl HeightMapRGBA {
    /// An empty RGBA height map.
    pub fn new() -> Self {
        Self {
            heights: Vec::new(),
            h_width: 0,
            h_height: 0,
            colors: Vec::new(),
            c_width: 0,
            c_height: 0,
            transform: Item3DTransform::default(),
        }
    }

    /// Set the height field (silx `_HeightMap.setData`), row-major. Returns `false`
    /// (unchanged) when `heights.len() != width * height`.
    pub fn set_data(&mut self, heights: &[f32], width: usize, height: usize) -> bool {
        if heights.len() != width * height {
            return false;
        }
        self.heights = heights.to_vec();
        self.h_width = width;
        self.h_height = height;
        true
    }

    /// Builder form of [`set_data`](Self::set_data).
    pub fn with_data(mut self, heights: &[f32], width: usize, height: usize) -> Self {
        self.set_data(heights, width, height);
        self
    }

    /// Set the RGB(A) image (silx `HeightMapRGBA.setColorData`), row-major. May
    /// differ in size from the height field (nearest-neighbour resampled, by width
    /// for the column axis — see [`HeightMapData`]). Returns `false` when
    /// `colors.len() != width * height`.
    pub fn set_color_data(&mut self, colors: &[Color32], width: usize, height: usize) -> bool {
        if colors.len() != width * height {
            return false;
        }
        self.colors = colors.to_vec();
        self.c_width = width;
        self.c_height = height;
        true
    }

    /// Builder form of [`set_color_data`](Self::set_color_data).
    pub fn with_color_data(mut self, colors: &[Color32], width: usize, height: usize) -> Self {
        self.set_color_data(colors, width, height);
        self
    }

    /// Height-field dimensions `(width, height)`.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.h_width, self.h_height)
    }

    /// True when nothing would be drawn (no height field or no colour image).
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty() || self.colors.is_empty()
    }

    /// World bounds `(min, max)` of the height-field point grid through the
    /// item's transform (silx `DataItem3D` bounds, `transformed=True`), or
    /// `None` when the height field is empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(height_grid_bounds(
            &self.heights,
            self.h_width,
            self.h_height,
        ))
    }

    /// Append the height field as RGBA square points to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        if self.is_empty() {
            return;
        }
        let raw_bounds = height_grid_bounds(&self.heights, self.h_width, self.h_height);
        append_with_transform(&self.transform, raw_bounds, geometry, |g| {
            for row in 0..self.h_height {
                let cr = nearest_src_index(row, self.h_height, self.c_height);
                for col in 0..self.h_width {
                    let cc = nearest_src_index(col, self.h_width, self.c_width);
                    let z = self.heights[row * self.h_width + col];
                    let color = self.colors[cr * self.c_width + cc];
                    g.add_point([col as f32, row as f32, z], color, 1.0, PointMarker::Square);
                }
            }
        });
    }
}

impl_item3d_transform!(HeightMapRGBA);

/// silx's default isosurface colour `#FFD700FF` (gold), `Isosurface.__init__`.
pub const DEFAULT_ISOSURFACE_COLOR: Color32 = Color32::from_rgb(0xFF, 0xD7, 0x00);

/// silx's documented default auto-level: `mean(data) + std(data)` over the finite
/// samples (`volume.py` `setAutoLevelFunction` example, the value
/// `ScalarFieldView` seeds its first isosurface with). Returns NaN when there are
/// no finite samples.
pub fn mean_plus_std(data: &[f32]) -> f32 {
    let finite: Vec<f64> = data
        .iter()
        .filter(|v| v.is_finite())
        .map(|&v| v as f64)
        .collect();
    if finite.is_empty() {
        return f32::NAN;
    }
    let n = finite.len() as f64;
    let mean = finite.iter().sum::<f64>() / n;
    let var = finite.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    (mean + var.sqrt()) as f32
}

/// One iso-surface of a [`ScalarField3D`]: an iso-level and a solid colour.
///
/// Port of silx `plot3d.items.volume.Isosurface`. The level is either a fixed
/// value or computed from the parent field by an auto-level function (silx
/// `setAutoLevelFunction`; e.g. [`mean_plus_std`]); the resolved value is stored
/// in `level` and refreshed by the owning [`ScalarField3D`] whenever the data
/// changes. The surface itself is built and emitted by the parent (the data lives
/// there), as a lit solid-colour mesh through the P1.2 mesh path.
#[derive(Clone, Debug)]
pub struct Isosurface {
    level: f32,
    auto: Option<fn(&[f32]) -> f32>,
    color: Color32,
}

impl Isosurface {
    /// A fixed-level iso-surface in the given colour.
    pub fn new(level: f32, color: Color32) -> Self {
        Self {
            level,
            auto: None,
            color,
        }
    }

    /// An auto-level iso-surface: `level` is recomputed by `auto(data)` each time
    /// the parent field changes (silx `setAutoLevelFunction`).
    pub fn new_auto(auto: fn(&[f32]) -> f32, color: Color32) -> Self {
        Self {
            level: f32::NAN,
            auto: Some(auto),
            color,
        }
    }

    /// The resolved iso-level (NaN if an auto-level has not yet been computed
    /// against data).
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Set a fixed iso-level, clearing any auto-level function (silx `setLevel`).
    pub fn set_level(&mut self, level: f32) {
        self.level = level;
        self.auto = None;
    }

    /// Set the auto-level function (silx `setAutoLevelFunction`); takes effect on
    /// the next parent data update.
    pub fn set_auto_level(&mut self, auto: fn(&[f32]) -> f32) {
        self.auto = Some(auto);
    }

    /// True when the level is computed by an auto-level function.
    pub fn is_auto_level(&self) -> bool {
        self.auto.is_some()
    }

    /// The iso-surface colour.
    pub fn color(&self) -> Color32 {
        self.color
    }

    /// Set the iso-surface colour (silx `setColor`).
    pub fn set_color(&mut self, color: Color32) {
        self.color = color;
    }

    /// Re-resolve an auto-level against `data` (called by the parent on data
    /// change). Fixed levels are left unchanged.
    fn resolve(&mut self, data: &[f32]) {
        if let Some(f) = self.auto {
            self.level = f(data);
        }
    }
}

/// Default cut-plane grid resolution: the slice is rasterised onto a
/// `resolution × resolution` texture (see [`CutPlane`]).
pub const DEFAULT_CUT_PLANE_RESOLUTION: usize = 256;

/// A colormapped cutting plane through a [`ScalarField3D`] (silx
/// `plot3d.items.volume.CutPlane`). It carries only presentation state — the
/// plane geometry, the [`Colormap`], the sampling [`ImageInterpolation`], and a
/// visibility flag — and reads the field samples from its owning `ScalarField3D`
/// (silx wires the data with `copy=False`; the data has one owner). Hidden by
/// default, matching silx (`ScalarField3D` creates its cut plane with
/// `setVisible(False)`).
///
/// Rendering (built by the owner in [`ScalarField3D::append_to`]): the plane is
/// intersected with the volume box `(0,0,0)..(width,height,depth)` to get the
/// contour polygon ([`box_plane_intersect`]); the slice is sampled on a
/// `resolution × resolution` grid in the plane, each sample coloured through the
/// colormap (CPU [`Colormap::color_at`], as the other 3D items), and the polygon
/// is fan-triangulated and emitted as one [`Scene3dTexturedMesh`].
///
/// Documented simplification: silx samples the 3D data texture per fragment
/// (continuous); this port rasterises the slice onto a 2D grid texture, so the
/// slice sharpness is bounded by `resolution` (the same CPU-colormap deviation as
/// P1.1–P2.1). The CPU sampler matches silx's texture convention — voxel centre
/// `(ix,iy,iz)` sits at world `(ix+0.5, iy+0.5, iz+0.5)` — with clamp-to-edge
/// outside the box; `interpolation` selects nearest vs trilinear sampling and is
/// also applied to the 2D texture.
#[derive(Clone, Debug)]
pub struct CutPlane {
    plane: Plane,
    colormap: Colormap,
    interpolation: ImageInterpolation,
    resolution: usize,
    visible: bool,
    /// Colour of the plane/box intersection contour (silx `PlaneInGroup` line
    /// colour, `primitives.py:1008` — default white `(1., 1., 1., 1.)`).
    stroke_color: Color32,
    /// Whether the contour is drawn (silx `PlaneInGroup.strokeVisible`,
    /// default `True`, `primitives.py:1010`).
    stroke_visible: bool,
}

impl Default for CutPlane {
    fn default() -> Self {
        Self::new()
    }
}

impl CutPlane {
    /// A hidden cut plane with silx defaults: normal `(0, 1, 0)` through the
    /// origin, the gray colormap over `[0, 1]`, linear interpolation, and a
    /// visible white contour stroke.
    pub fn new() -> Self {
        Self {
            plane: Plane::new(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)),
            colormap: Colormap::new(ColormapName::Gray, 0.0, 1.0),
            interpolation: ImageInterpolation::Linear,
            resolution: DEFAULT_CUT_PLANE_RESOLUTION,
            visible: false,
            stroke_color: Color32::WHITE,
            stroke_visible: true,
        }
    }

    /// The cutting plane (point + unit normal).
    pub fn plane(&self) -> &Plane {
        &self.plane
    }

    /// Mutable access to the cutting plane (point/normal setters).
    pub fn plane_mut(&mut self) -> &mut Plane {
        &mut self.plane
    }

    /// Set a point the plane passes through (silx `PlaneMixIn.setPoint`).
    pub fn set_point(&mut self, point: Vec3) {
        self.plane.set_point(point);
    }

    /// Set the plane normal; the zero vector leaves the plane unoriented
    /// (silx `PlaneMixIn.setNormal`).
    pub fn set_normal(&mut self, normal: Vec3) {
        self.plane.set_normal(normal);
    }

    /// Read-only access to the colormap.
    pub fn colormap(&self) -> &Colormap {
        &self.colormap
    }

    /// Mutable access to the colormap (e.g. to set its value range directly).
    pub fn colormap_mut(&mut self) -> &mut Colormap {
        &mut self.colormap
    }

    /// Set the colormap (silx `ColormapMixIn.setColormap`).
    pub fn set_colormap(&mut self, colormap: Colormap) {
        self.colormap = colormap;
    }

    /// Builder form of [`set_colormap`](Self::set_colormap).
    pub fn with_colormap(mut self, colormap: Colormap) -> Self {
        self.colormap = colormap;
        self
    }

    /// The texture interpolation (silx `InterpolationMixIn`).
    pub fn interpolation(&self) -> ImageInterpolation {
        self.interpolation
    }

    /// Set the texture interpolation (silx `setInterpolation`).
    pub fn set_interpolation(&mut self, interpolation: ImageInterpolation) {
        self.interpolation = interpolation;
    }

    /// The grid resolution (texels per axis of the slice texture).
    pub fn resolution(&self) -> usize {
        self.resolution
    }

    /// Set the grid resolution (clamped to ≥ 1).
    pub fn set_resolution(&mut self, resolution: usize) {
        self.resolution = resolution.max(1);
    }

    /// Whether the cut plane is drawn (silx `setVisible`).
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Show or hide the cut plane (silx `setVisible`).
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// The colour of the plane's border stroke — the plane/box intersection
    /// contour (silx `ScalarFieldView.getStrokeColor`,
    /// `ScalarFieldView.py:555-557`).
    pub fn stroke_color(&self) -> Color32 {
        self.stroke_color
    }

    /// Set the colour of the plane's border stroke (silx
    /// `ScalarFieldView.setStrokeColor`, `ScalarFieldView.py:559-570`).
    pub fn set_stroke_color(&mut self, color: Color32) {
        self.stroke_color = color;
    }

    /// Whether the border stroke is drawn (silx `PlaneInGroup.strokeVisible`,
    /// `primitives.py:1047-1050`).
    pub fn is_stroke_visible(&self) -> bool {
        self.stroke_visible
    }

    /// Show or hide the border stroke (silx `PlaneInGroup.strokeVisible`
    /// setter, `primitives.py:1052-1056`).
    pub fn set_stroke_visible(&mut self, visible: bool) {
        self.stroke_visible = visible;
    }
}

/// An orthonormal in-plane basis `(e1, e2)` for the plane with unit `normal`:
/// `e1 ⟂ normal`, `e2 = normal × e1`. The seed axis is whichever of x/y is least
/// aligned with `normal`, so the cross product never collapses.
fn plane_basis(normal: Vec3) -> (Vec3, Vec3) {
    let seed = if normal.x.abs() < 0.9 {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        Vec3::new(0.0, 1.0, 0.0)
    };
    let e1 = normal.cross(seed).normalized();
    let e2 = normal.cross(e1).normalized();
    (e1, e2)
}

/// Sample the `(depth, height, width)` field (`zyx`, `width` contiguous) at world
/// point `p`, following silx's texture convention: voxel centre `(ix,iy,iz)` is
/// at world `(ix+0.5, iy+0.5, iz+0.5)`, and coordinates clamp to the edge voxel
/// outside the box. `Nearest` rounds to the nearest voxel; `Linear` trilinearly
/// interpolates the eight surrounding voxels.
fn sample_field_value(
    data: &[f32],
    depth: usize,
    height: usize,
    width: usize,
    p: Vec3,
    interpolation: ImageInterpolation,
) -> f32 {
    let idx = |ix: usize, iy: usize, iz: usize| data[(iz * height + iy) * width + ix];
    // World → continuous voxel coordinate (voxel centre at integer position).
    let (fx, fy, fz) = (p.x - 0.5, p.y - 0.5, p.z - 0.5);
    match interpolation {
        ImageInterpolation::Nearest => {
            let clamp = |f: f32, n: usize| (f.round().max(0.0) as usize).min(n - 1);
            idx(clamp(fx, width), clamp(fy, height), clamp(fz, depth))
        }
        ImageInterpolation::Linear => {
            // Clamp the centre coordinate to [0, n-1] (clamp-to-edge), then
            // interpolate towards the next voxel.
            let lo = |f: f32, n: usize| -> (usize, usize, f32) {
                let c = f.clamp(0.0, (n - 1) as f32);
                let i0 = c.floor() as usize;
                let i1 = (i0 + 1).min(n - 1);
                (i0, i1, c - i0 as f32)
            };
            let (x0, x1, dx) = lo(fx, width);
            let (y0, y1, dy) = lo(fy, height);
            let (z0, z1, dz) = lo(fz, depth);
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let c00 = lerp(idx(x0, y0, z0), idx(x1, y0, z0), dx);
            let c10 = lerp(idx(x0, y1, z0), idx(x1, y1, z0), dx);
            let c01 = lerp(idx(x0, y0, z1), idx(x1, y0, z1), dx);
            let c11 = lerp(idx(x0, y1, z1), idx(x1, y1, z1), dx);
            lerp(lerp(c00, c10, dy), lerp(c01, c11, dy), dz)
        }
    }
}

/// Build the cut-plane textured mesh for `cut_plane` over the `(depth, height,
/// width)` field, or `None` when the plane does not slice the volume (fewer than
/// three contour vertices) or the field is empty. The single owner of the
/// cut-plane geometry, called from [`ScalarField3D::append_to`].
fn build_cut_plane_mesh(
    data: &[f32],
    depth: usize,
    height: usize,
    width: usize,
    cut_plane: &CutPlane,
) -> Option<Scene3dTexturedMesh> {
    if data.is_empty() {
        return None;
    }
    let normal = cut_plane.plane.normal();
    let bounds = (
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(width as f32, height as f32, depth as f32),
    );
    let contour = box_plane_intersect(bounds, normal, cut_plane.plane.point());
    if contour.len() < 3 {
        return None;
    }

    // Plane-space coordinates (s along e1, t along e2) of every contour vertex,
    // measured from the first vertex.
    let (e1, e2) = plane_basis(normal);
    let origin = contour[0];
    let st: Vec<(f32, f32)> = contour
        .iter()
        .map(|&v| {
            let d = v - origin;
            (d.dot(e1), d.dot(e2))
        })
        .collect();
    let (mut smin, mut smax) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut tmin, mut tmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for &(s, t) in &st {
        smin = smin.min(s);
        smax = smax.max(s);
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    let sspan = (smax - smin).max(f32::MIN_POSITIVE);
    let tspan = (tmax - tmin).max(f32::MIN_POSITIVE);

    // Rasterise the slice onto a res×res grid (row-major, row 0 = t at tmin),
    // colouring each sample through the colormap → premultiplied-linear RGBA8.
    let res = cut_plane.resolution.max(1);
    let mut pixels = Vec::with_capacity(res * res * 4);
    for j in 0..res {
        let t = tmin + (j as f32 + 0.5) / res as f32 * tspan;
        for i in 0..res {
            let s = smin + (i as f32 + 0.5) / res as f32 * sspan;
            let p = origin + e1 * s + e2 * t;
            let value = sample_field_value(data, depth, height, width, p, cut_plane.interpolation);
            let [r, g, b, a] = cut_plane.colormap.color_at(value as f64);
            pixels.extend_from_slice(&premul_linear_rgba8(Color32::from_rgba_unmultiplied(
                r, g, b, a,
            )));
        }
    }

    // Fan-triangulate the contour; each vertex's UV is its plane coordinate
    // normalised to the grid's bounding rect.
    let uv = |k: usize| [(st[k].0 - smin) / sspan, (st[k].1 - tmin) / tspan];
    let mut vertices = Vec::with_capacity((contour.len() - 2) * 3);
    let mut uvs = Vec::with_capacity((contour.len() - 2) * 3);
    for k in 1..contour.len() - 1 {
        for &idx in &[0usize, k, k + 1] {
            vertices.push(contour[idx].to_array());
            uvs.push(uv(idx));
        }
    }
    Some(Scene3dTexturedMesh {
        pixels,
        width: res as u32,
        height: res as u32,
        vertices,
        uvs,
        interpolation: cut_plane.interpolation,
    })
}

/// A 3D scalar field on a regular grid, rendered as marching-cubes iso-surfaces.
///
/// Port of silx `plot3d.items.volume.ScalarField3D`. Holds the `(depth, height,
/// width)` field (`zyx`, `width` contiguous) and a list of [`Isosurface`]s. Each
/// iso-surface is extracted with [marching cubes](marching_cubes_isosurface) and
/// emitted as a lit solid-colour mesh; the marching-cubes `(z,y,x)` vertices are
/// mapped to world `(x+0.5, y+0.5, z+0.5)` (and normals `(nz,ny,nx)→(nx,ny,nz)`),
/// reproducing silx's `_isogroup` swap matrix + `Translate(0.5,0.5,0.5)`. The
/// field bounds are the full volume box `(0,0,0)..(width,height,depth)` (silx
/// `BoundedGroup`), independent of any iso-surface extent.
///
/// It also owns one [`CutPlane`] (silx `ScalarField3D` owns a single cut plane),
/// hidden by default; when visible, [`append_to`](Self::append_to) builds its
/// colormapped slice from the field data (the data has one owner, as silx wires
/// the plane with `copy=False`).
#[derive(Clone, Debug)]
pub struct ScalarField3D {
    data: Vec<f32>,
    depth: usize,
    height: usize,
    width: usize,
    data_range: Option<(f32, f32, f32)>,
    isosurfaces: Vec<Isosurface>,
    cut_plane: CutPlane,
    transform: Item3DTransform,
}

impl Default for ScalarField3D {
    fn default() -> Self {
        Self::new()
    }
}

impl ScalarField3D {
    /// An empty scalar field with no iso-surfaces and a hidden cut plane.
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            depth: 0,
            height: 0,
            width: 0,
            data_range: None,
            isosurfaces: Vec::new(),
            cut_plane: CutPlane::new(),
            transform: Item3DTransform::default(),
        }
    }

    /// Set the 3D scalar field, `data` row-major as `(depth, height, width)` with
    /// `width` contiguous. Returns `false` (leaving the field unchanged) when
    /// `data.len() != depth*height*width` or any dimension is `< 2` (silx asserts
    /// `min(shape) >= 2`). Setting data re-resolves every auto-level iso-surface.
    pub fn set_data(&mut self, data: &[f32], depth: usize, height: usize, width: usize) -> bool {
        if depth < 2 || height < 2 || width < 2 || data.len() != depth * height * width {
            return false;
        }
        self.data = data.to_vec();
        self.depth = depth;
        self.height = height;
        self.width = width;
        self.data_range = compute_data_range(&self.data);
        let data = std::mem::take(&mut self.data);
        for iso in &mut self.isosurfaces {
            iso.resolve(&data);
        }
        self.data = data;
        true
    }

    /// Builder form of [`set_data`](Self::set_data); inconsistent data leaves the
    /// field empty.
    pub fn with_data(mut self, data: &[f32], depth: usize, height: usize, width: usize) -> Self {
        self.set_data(data, depth, height, width);
        self
    }

    /// Field dimensions `(depth, height, width)`.
    pub fn dimensions(&self) -> (usize, usize, usize) {
        (self.depth, self.height, self.width)
    }

    /// Read-only access to the field samples (`zyx`, `width` contiguous).
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// The data range as `(min, min_positive, max)` over finite samples, or
    /// `None` when empty / all non-finite (silx `getDataRange`; `min_positive` is
    /// NaN when no sample is positive).
    pub fn data_range(&self) -> Option<(f32, f32, f32)> {
        self.data_range
    }

    /// True when no field data is set.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Add a fixed-level iso-surface, returning its index (silx `addIsosurface`).
    pub fn add_isosurface(&mut self, level: f32, color: Color32) -> usize {
        self.isosurfaces.push(Isosurface::new(level, color));
        self.isosurfaces.len() - 1
    }

    /// Add an auto-level iso-surface (silx `addIsosurface` with a callable),
    /// resolving the level against the current data immediately. Returns its
    /// index.
    pub fn add_auto_isosurface(&mut self, auto: fn(&[f32]) -> f32, color: Color32) -> usize {
        let mut iso = Isosurface::new_auto(auto, color);
        if !self.data.is_empty() {
            iso.resolve(&self.data);
        }
        self.isosurfaces.push(iso);
        self.isosurfaces.len() - 1
    }

    /// All iso-surfaces, in insertion order.
    pub fn isosurfaces(&self) -> &[Isosurface] {
        &self.isosurfaces
    }

    /// Mutable access to one iso-surface (e.g. to change its level or colour).
    pub fn isosurface_mut(&mut self, index: usize) -> Option<&mut Isosurface> {
        self.isosurfaces.get_mut(index)
    }

    /// Remove the iso-surface at `index` (silx `removeIsosurface`); out-of-range
    /// is a no-op returning `false`.
    pub fn remove_isosurface(&mut self, index: usize) -> bool {
        if index < self.isosurfaces.len() {
            self.isosurfaces.remove(index);
            true
        } else {
            false
        }
    }

    /// Remove all iso-surfaces (silx `clearIsosurfaces`).
    pub fn clear_isosurfaces(&mut self) {
        self.isosurfaces.clear();
    }

    /// Read-only access to the cut plane (silx `getCutPlanes()[0]`).
    pub fn cut_plane(&self) -> &CutPlane {
        &self.cut_plane
    }

    /// Mutable access to the cut plane — set its position/normal, colormap,
    /// interpolation, resolution, or visibility.
    pub fn cut_plane_mut(&mut self) -> &mut CutPlane {
        &mut self.cut_plane
    }

    /// Fit the cut plane's colormap range to the field with `mode` (silx
    /// autoscales the cut-plane colormap over the volume data), returning the new
    /// `(vmin, vmax)`. A no-op leaving the range unchanged when the field is
    /// empty.
    pub fn autoscale_cut_plane_colormap(&mut self, mode: AutoscaleMode) -> (f64, f64) {
        if self.data.is_empty() {
            let cm = &self.cut_plane.colormap;
            return (cm.vmin, cm.vmax);
        }
        let values: Vec<f64> = self.data.iter().map(|&v| v as f64).collect();
        let (vmin, vmax) = self.cut_plane.colormap.autoscale_range(mode, &values);
        self.cut_plane.colormap.vmin = vmin;
        self.cut_plane.colormap.vmax = vmax;
        (vmin, vmax)
    }

    /// The raw volume box `(0,0,0)..(width,height,depth)` in the object (voxel)
    /// frame, or `None` when no data is set.
    fn raw_bounds(&self) -> Option<(Vec3, Vec3)> {
        if self.data.is_empty() {
            return None;
        }
        Some((
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(self.width as f32, self.height as f32, self.depth as f32),
        ))
    }

    /// The volume bounding box through the item's transform (silx
    /// `BoundedGroup` data bounds under the `DataItem3D` transform stack), or
    /// `None` when no data is set.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.transform.transform_bounds(self.raw_bounds())
    }

    /// Map a scene-frame position into the object (voxel) frame: identity fast
    /// path, else through the inverse of the composed item transform. `None`
    /// when the composed matrix is singular (e.g. a zero scale) — nothing is
    /// pickable then.
    fn scene_to_object(&self, p: Vec3) -> Option<Vec3> {
        if self.transform.is_identity() {
            return Some(p);
        }
        let inv = self
            .transform
            .composed_matrix(self.raw_bounds())
            .inverse()?;
        Some(inv.transform_point(p, false))
    }

    /// [`value_at`](Self::value_at) with `obj` already in the object (voxel)
    /// frame — the single box test + sampler both pick paths share.
    fn value_at_object(&self, obj: Vec3) -> Option<f32> {
        let (min, max) = self.raw_bounds()?;
        if obj.x < min.x
            || obj.y < min.y
            || obj.z < min.z
            || obj.x > max.x
            || obj.y > max.y
            || obj.z > max.z
        {
            return None;
        }
        Some(sample_field_value(
            &self.data,
            self.depth,
            self.height,
            self.width,
            obj,
            self.cut_plane.interpolation(),
        ))
    }

    /// Sample the field value at scene position `world` (inverse-mapped through
    /// the item transform into the voxel frame), or `None` when the field is
    /// empty or the position lies outside the volume box. Uses the cut plane's
    /// interpolation (nearest vs trilinear) so a picked value matches the slice
    /// the user sees. The single field sampler (`sample_field_value`, the same
    /// owner the cut-plane raster uses), with an explicit box test so a point
    /// past the edge reads `None` rather than the clamped edge voxel.
    pub fn value_at(&self, world: Vec3) -> Option<f32> {
        self.value_at_object(self.scene_to_object(world)?)
    }

    /// Intersect the picking `segment` (`(near, far)` in scene space) with the
    /// cut plane, returning the scene position of the hit when the cut plane is
    /// **visible** and the hit lies inside the volume box, else `None`. Port of
    /// silx `items.volume.CutPlane._pickFull` (segment/plane intersection, then
    /// a data-bounds test — silx converts the pick ray into each object's frame
    /// first, so the segment is mapped through the inverse item transform and
    /// the hit mapped back). Pair with [`value_at`](Self::value_at) for the
    /// sampled value at the hit — the value the colormapped slice shows there.
    pub fn pick_cut_plane(&self, segment: (Vec3, Vec3)) -> Option<Vec3> {
        if !self.cut_plane.is_visible() || self.data.is_empty() {
            return None;
        }
        let a = self.scene_to_object(segment.0)?;
        let b = self.scene_to_object(segment.1)?;
        let plane = self.cut_plane.plane();
        let hit = segment_plane_intersect(a, b, plane.normal(), plane.point())
            .into_iter()
            .find(|&hit| self.value_at_object(hit).is_some())?;
        Some(if self.transform.is_identity() {
            hit
        } else {
            self.transform
                .composed_matrix(self.raw_bounds())
                .transform_point(hit, false)
        })
    }

    /// Append every iso-surface's triangles to `geometry`. Iso-surfaces are
    /// emitted from highest level to lowest (silx `_updateIsosurfaces` sorts by
    /// `-level`); a non-finite level or an empty surface is skipped.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        if self.data.is_empty() {
            return;
        }
        append_with_transform(&self.transform, self.raw_bounds(), geometry, |g| {
            self.append_raw(g)
        });
    }

    /// Build the raw (voxel-frame) geometry — see [`Self::append_to`].
    fn append_raw(&self, geometry: &mut Scene3dGeometry) {
        let mut order: Vec<usize> = (0..self.isosurfaces.len()).collect();
        order.sort_by(|&a, &b| {
            self.isosurfaces[b]
                .level
                .total_cmp(&self.isosurfaces[a].level)
        });
        for i in order {
            let iso = &self.isosurfaces[i];
            if !iso.level.is_finite() {
                continue;
            }
            let Some((vertices, normals, indices)) = marching_cubes_isosurface(
                &self.data,
                self.depth,
                self.height,
                self.width,
                iso.level,
                true,
            ) else {
                continue;
            };
            // zyx → xyz swap + 0.5 cell-centre offset (silx _isogroup transform).
            for tri in indices.chunks_exact(3) {
                let p = [0usize, 1, 2].map(|k| {
                    let v = vertices[tri[k] as usize];
                    [v[2] + 0.5, v[1] + 0.5, v[0] + 0.5]
                });
                let n = [0usize, 1, 2].map(|k| {
                    let nm = normals[tri[k] as usize];
                    [nm[2], nm[1], nm[0]]
                });
                geometry.add_mesh_triangle(p, iso.color, n);
            }
        }
        // The cut plane (when visible): a colormapped slice of the volume.
        if self.cut_plane.visible {
            if let Some(mesh) = build_cut_plane_mesh(
                &self.data,
                self.depth,
                self.height,
                self.width,
                &self.cut_plane,
            ) {
                geometry.add_textured_mesh(mesh);
            }
            // Border stroke: the plane/box intersection contour as a closed
            // line loop (silx PlaneInGroup.contourVertices,
            // primitives.py:1082-1101 `boxPlaneIntersect` over the parent data
            // bounds → `Lines(contourVertices, mode="loop")` :1126). The stroke
            // is white by default, silx width 2.0 — drawn 1px here like the
            // rest of the line chrome, the box wireframe included. Raw bounds:
            // the contour is built in the voxel frame like the slice itself.
            if self.cut_plane.stroke_visible
                && let Some(bounds) = self.raw_bounds()
            {
                let contour = box_plane_intersect(
                    bounds,
                    self.cut_plane.plane.normal(),
                    self.cut_plane.plane.point(),
                );
                if contour.len() >= 3 {
                    for (i, &a) in contour.iter().enumerate() {
                        let b = contour[(i + 1) % contour.len()];
                        geometry.add_line(a.to_array(), b.to_array(), self.cut_plane.stroke_color);
                    }
                }
            }
        }
    }
}

impl_item3d_transform!(ScalarField3D);

/// Compute `(min, min_positive, max)` over the finite samples (silx
/// `ScalarField3D._computeRangeFromData` via `min_max(..., min_positive=True,
/// finite=True)`). `min_positive` is NaN when no sample is `> 0`; returns `None`
/// when there are no finite samples.
fn compute_data_range(data: &[f32]) -> Option<(f32, f32, f32)> {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut min_pos = f32::INFINITY;
    let mut any = false;
    for &v in data {
        if !v.is_finite() {
            continue;
        }
        any = true;
        min = min.min(v);
        max = max.max(v);
        if v > 0.0 {
            min_pos = min_pos.min(v);
        }
    }
    if !any {
        return None;
    }
    let min_pos = if min_pos.is_finite() {
        min_pos
    } else {
        f32::NAN
    };
    Some((min, min_pos, max))
}

/// A 3D complex field on a regular grid, visualised by projecting each sample to
/// a real scalar (the [`ComplexMode`]) and then reusing the [`ScalarField3D`]
/// machinery — marching-cubes iso-surfaces and the colormapped cut plane.
///
/// Port of silx `plot3d.items.volume.ComplexField3D` (+ `ComplexMixIn`). Holds
/// the `(depth, height, width)` complex field as parallel real/imaginary arrays
/// (`zyx`, `width` contiguous) and an inner `ScalarField3D` carrying the current
/// projection. [`set_complex_mode`](Self::set_complex_mode) reprojects and, as in
/// silx, clears the iso-surfaces (their levels were tied to the old mode's
/// range); the cut plane persists across a mode change. Iso-surface and cut-plane
/// management is reached through [`field`](Self::field) / [`field_mut`](Self::field_mut).
///
/// The mode is the shared silx `ComplexMode` ([`crate::core::complex`]); the six
/// scalar modes (`Absolute`, `Phase`, `Real`, `Imaginary`, `SquareAmplitude`,
/// `Log10Amplitude`) project to a real field. Documented simplification (matches
/// the rest of the port): silx's two hue-display modes (`AmplitudePhase`,
/// `Log10AmplitudePhase`) colour an iso-surface by phase rather than extract a
/// scalar; they are not ported and project to an all-zero field
/// ([`ComplexMode::to_scalar`] returns `0.0` for them).
#[derive(Clone, Debug)]
pub struct ComplexField3D {
    re: Vec<f32>,
    im: Vec<f32>,
    depth: usize,
    height: usize,
    width: usize,
    mode: ComplexMode,
    field: ScalarField3D,
}

impl Default for ComplexField3D {
    fn default() -> Self {
        Self::new()
    }
}

impl ComplexField3D {
    /// An empty complex field with the default mode (amplitude, silx
    /// `ABSOLUTE`) and a hidden cut plane.
    pub fn new() -> Self {
        Self {
            re: Vec::new(),
            im: Vec::new(),
            mode: ComplexMode::Absolute,
            depth: 0,
            height: 0,
            width: 0,
            field: ScalarField3D::new(),
        }
    }

    /// Set the complex field from parallel real/imaginary arrays, both row-major
    /// `(depth, height, width)` with `width` contiguous. Returns `false` (leaving
    /// the field unchanged) when the lengths disagree, `re.len() !=
    /// depth*height*width`, or any dimension is `< 2` (silx asserts
    /// `min(shape) >= 2`). The current mode's projection is pushed into the inner
    /// field (re-resolving auto-level iso-surfaces).
    pub fn set_data(
        &mut self,
        re: &[f32],
        im: &[f32],
        depth: usize,
        height: usize,
        width: usize,
    ) -> bool {
        if depth < 2 || height < 2 || width < 2 {
            return false;
        }
        let n = depth * height * width;
        if re.len() != n || im.len() != n {
            return false;
        }
        self.re = re.to_vec();
        self.im = im.to_vec();
        self.depth = depth;
        self.height = height;
        self.width = width;
        self.reproject();
        true
    }

    /// Builder form of [`set_data`](Self::set_data); inconsistent data leaves the
    /// field empty.
    pub fn with_data(
        mut self,
        re: &[f32],
        im: &[f32],
        depth: usize,
        height: usize,
        width: usize,
    ) -> Self {
        self.set_data(re, im, depth, height, width);
        self
    }

    /// The current complex visualisation mode.
    pub fn complex_mode(&self) -> ComplexMode {
        self.mode
    }

    /// Set the complex visualisation mode (silx `setComplexMode`). Changing it
    /// clears the iso-surfaces (their levels were tied to the previous mode's
    /// value range) and reprojects the field; the cut plane is kept. A no-op when
    /// the mode is unchanged.
    pub fn set_complex_mode(&mut self, mode: ComplexMode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.field.clear_isosurfaces();
        self.reproject();
    }

    /// The projected real field of `mode` (silx `getData(mode=…)`), or `None`
    /// when no data is set.
    pub fn projected_data(&self, mode: ComplexMode) -> Option<Vec<f32>> {
        if self.re.is_empty() {
            return None;
        }
        Some(
            self.re
                .iter()
                .zip(&self.im)
                .map(|(&r, &i)| mode.to_scalar(r, i))
                .collect(),
        )
    }

    /// The `(min, min_positive, max)` range of `mode`'s projection over finite
    /// samples (silx `getDataRange(mode=…)`), or `None` when empty / all
    /// non-finite.
    pub fn data_range_for(&self, mode: ComplexMode) -> Option<(f32, f32, f32)> {
        let data = self.projected_data(mode)?;
        compute_data_range(&data)
    }

    /// Field dimensions `(depth, height, width)`.
    pub fn dimensions(&self) -> (usize, usize, usize) {
        (self.depth, self.height, self.width)
    }

    /// True when no field data is set.
    pub fn is_empty(&self) -> bool {
        self.re.is_empty()
    }

    /// Read-only access to the inner scalar field (the current projection, its
    /// iso-surfaces, and the cut plane).
    pub fn field(&self) -> &ScalarField3D {
        &self.field
    }

    /// Mutable access to the inner scalar field — add/remove iso-surfaces, set the
    /// cut plane. The field data itself is owned here and refreshed on
    /// `set_data`/`set_complex_mode`; do not call its `set_data` directly.
    pub fn field_mut(&mut self) -> &mut ScalarField3D {
        &mut self.field
    }

    /// The volume bounding box through the item's transform, or `None` when no
    /// data is set.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.field.bounds()
    }

    /// Append the projected field's iso-surfaces and cut plane to `geometry`
    /// (delegates to the inner [`ScalarField3D`]).
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.field.append_to(geometry);
    }

    /// The item's transform stack (silx `DataItem3D` transforms,
    /// `items/core.py:288-315`), delegated to the inner [`ScalarField3D`] so
    /// bounds, geometry, and the cut-plane pick follow it by construction.
    pub fn transform(&self) -> &Item3DTransform {
        self.field.transform()
    }

    /// Mutable access to the transform stack (silx setters,
    /// `items/core.py:335-485`).
    pub fn transform_mut(&mut self) -> &mut Item3DTransform {
        self.field.transform_mut()
    }

    /// Push the current mode's projection into the inner scalar field.
    fn reproject(&mut self) {
        let data: Vec<f32> = self
            .re
            .iter()
            .zip(&self.im)
            .map(|(&r, &i)| self.mode.to_scalar(r, i))
            .collect();
        self.field
            .set_data(&data, self.depth, self.height, self.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3×3×3 ramp field whose value equals its `z` index (so a known world
    /// position has a predictable sample): `data[z][y][x] = z`.
    fn ramp_field() -> ScalarField3D {
        let (d, h, w) = (3usize, 3usize, 3usize);
        let mut data = vec![0.0f32; d * h * w];
        for z in 0..d {
            for y in 0..h {
                for x in 0..w {
                    data[(z * h + y) * w + x] = z as f32;
                }
            }
        }
        ScalarField3D::new().with_data(&data, d, h, w)
    }

    #[test]
    fn value_at_samples_inside_and_rejects_outside_the_box() {
        let field = ramp_field();
        // Voxel-centre convention: world (·,·,1.5) is exactly z-index 1 → value 1.0
        // (the cut plane's default interpolation is trilinear).
        let v = field
            .value_at(Vec3::new(1.5, 1.5, 1.5))
            .expect("inside the box");
        assert!((v - 1.0).abs() < 1e-5, "sampled {v}");
        // World z = 2.0 is half-way between z-index 1 (=1) and 2 (=2) → 1.5.
        let mid = field.value_at(Vec3::new(1.5, 1.5, 2.0)).expect("inside");
        assert!((mid - 1.5).abs() < 1e-5, "sampled {mid}");
        // The voxel centre at z-index 2 reads exactly 2.0.
        let top = field.value_at(Vec3::new(1.5, 1.5, 2.5)).expect("inside");
        assert!((top - 2.0).abs() < 1e-5, "sampled {top}");
        // Outside the (0,0,0)..(3,3,3) box → None (no edge-clamp leak).
        assert!(field.value_at(Vec3::new(3.5, 1.0, 1.0)).is_none());
        assert!(field.value_at(Vec3::new(-0.1, 1.0, 1.0)).is_none());
        // Empty field → None.
        assert!(
            ScalarField3D::new()
                .value_at(Vec3::new(1.0, 1.0, 1.0))
                .is_none()
        );
    }

    #[test]
    fn pick_cut_plane_hidden_is_none_visible_hits_inside_box() {
        let mut field = ramp_field();
        // Default cut plane: normal (0,1,0) through the origin — through y = 0.
        // Move it to y = 1.5 (mid-volume) so a ray along -Y crosses it inside the box.
        field.cut_plane_mut().set_point(Vec3::new(0.0, 1.5, 0.0));

        // A segment piercing the plane at world (1.5, 1.5, 1.5).
        let seg = (Vec3::new(1.5, 3.0, 1.5), Vec3::new(1.5, 0.0, 1.5));

        // Hidden by default → no pick.
        assert!(field.pick_cut_plane(seg).is_none());

        field.cut_plane_mut().set_visible(true);
        let hit = field
            .pick_cut_plane(seg)
            .expect("visible plane is crossed inside the box");
        assert!((hit.y - 1.5).abs() < 1e-5, "hit on the plane: {hit:?}");
        // The sampled value there matches value_at: world z = 1.5 is z-index 1 → 1.0.
        let value = field.value_at(hit).expect("hit is inside the box");
        assert!((value - 1.0).abs() < 1e-5, "value {value}");
    }

    #[test]
    fn pick_cut_plane_outside_box_is_none() {
        let mut field = ramp_field();
        field.cut_plane_mut().set_visible(true);
        // Plane at y = 1.5, but the ray crosses it far outside the x/z box extent.
        field.cut_plane_mut().set_point(Vec3::new(0.0, 1.5, 0.0));
        let seg = (Vec3::new(9.0, 3.0, 9.0), Vec3::new(9.0, 0.0, 9.0));
        assert!(field.pick_cut_plane(seg).is_none());
    }

    #[test]
    fn field_transform_moves_bounds_value_and_cut_plane_pick_together() {
        // A translated field (silx DataItem3D setTranslation): bounds, the
        // scene-frame value sampler, and the cut-plane pick must all read the
        // same composed transform — one owner, no per-path drift.
        let mut field = ramp_field();
        field.transform_mut().set_translation(10.0, 0.0, 0.0);

        // Bounds follow the transform.
        let (lo, hi) = field.bounds().expect("has data");
        assert_eq!(lo, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(hi, Vec3::new(13.0, 3.0, 3.0));

        // value_at takes scene coordinates: the voxel-frame sample (1.5,1.5,1.5)
        // (= value 1.0) now lives at x + 10; the raw location reads None.
        let v = field
            .value_at(Vec3::new(11.5, 1.5, 1.5))
            .expect("inside the translated box");
        assert!((v - 1.0).abs() < 1e-5, "sampled {v}");
        assert!(field.value_at(Vec3::new(1.5, 1.5, 1.5)).is_none());

        // Cut plane at voxel-frame y = 1.5: a scene-frame segment over the
        // translated box hits it, and the hit comes back in scene coordinates.
        field.cut_plane_mut().set_visible(true);
        field.cut_plane_mut().set_point(Vec3::new(0.0, 1.5, 0.0));
        let seg = (Vec3::new(11.5, 3.0, 1.5), Vec3::new(11.5, 0.0, 1.5));
        let hit = field.pick_cut_plane(seg).expect("crosses inside the box");
        assert!(
            (hit.x - 11.5).abs() < 1e-4 && (hit.y - 1.5).abs() < 1e-4,
            "scene-frame hit: {hit:?}"
        );
        // Feeding the returned scene position back into value_at agrees.
        let value = field.value_at(hit).expect("hit inside the box");
        assert!((value - 1.0).abs() < 1e-5, "value {value}");

        // The same segment placed over the RAW (untransformed) box misses.
        let raw_seg = (Vec3::new(1.5, 3.0, 1.5), Vec3::new(1.5, 0.0, 1.5));
        assert!(field.pick_cut_plane(raw_seg).is_none());
    }

    #[test]
    fn set_data_rejects_length_mismatch() {
        let mut s = Scatter3D::new();
        assert!(!s.set_data(&[0.0, 1.0], &[0.0], &[0.0, 1.0], &[0.0, 1.0]));
        assert!(s.is_empty(), "rejected data must not be partially stored");
        assert!(s.set_data(&[0.0, 1.0], &[2.0, 3.0], &[4.0, 5.0], &[6.0, 7.0]));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn append_to_colours_each_point_through_the_colormap() {
        // A ramp colormap over [0, 4]: value 0 → LUT index 0, value 4 → index 255.
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 4.0);
        let s = Scatter3D::new()
            .with_colormap(cmap.clone())
            .with_marker(PointMarker::Square)
            .with_size(8.0)
            .with_data(
                &[0.0, 1.0, 2.0],
                &[0.0, 0.0, 0.0],
                &[0.0, 0.0, 0.0],
                &[0.0, 2.0, 4.0],
            );

        let mut g = Scene3dGeometry::new();
        s.append_to(&mut g);

        // One point per datum, each at its position, all square, all size 8.
        assert_eq!(g.points.len(), 3);
        assert_eq!(g.points[1].pos, [1.0, 0.0, 0.0]);
        for p in &g.points {
            assert_eq!(p.size, 8.0);
            assert_eq!(p.marker, PointMarker::Square.id());
        }

        // Colors match the colormap LUT lookup (premultiplied at upload).
        let expect = |v: f64| {
            let [r, gg, b, a] = cmap.color_at(v);
            egui::Rgba::from(Color32::from_rgba_unmultiplied(r, gg, b, a)).to_array()
        };
        assert_eq!(g.points[0].color, expect(0.0));
        assert_eq!(g.points[2].color, expect(4.0));
        // The endpoints differ (the value actually drives the color).
        assert_ne!(g.points[0].color, g.points[2].color);
    }

    #[test]
    fn autoscale_colormap_fits_value_range() {
        let mut s =
            Scatter3D::new().with_data(&[0.0, 1.0, 2.0], &[0.0; 3], &[0.0; 3], &[-5.0, 0.0, 10.0]);
        let (vmin, vmax) = s.autoscale_colormap(AutoscaleMode::MinMax);
        assert_eq!((vmin, vmax), (-5.0, 10.0));
        assert_eq!(s.colormap().vmin, -5.0);
        assert_eq!(s.colormap().vmax, 10.0);
    }

    #[test]
    fn bounds_brackets_the_points() {
        assert!(Scatter3D::new().bounds().is_none());
        let s = Scatter3D::new().with_data(
            &[-1.0, 2.0, 0.5],
            &[3.0, -2.0, 1.0],
            &[0.0, 4.0, -1.0],
            &[0.0; 3],
        );
        let (min, max) = s.bounds().expect("non-empty bounds");
        assert_eq!((min.x, min.y, min.z), (-1.0, -2.0, -1.0));
        assert_eq!((max.x, max.y, max.z), (2.0, 3.0, 4.0));
    }

    #[test]
    fn scatter2d_set_data_rejects_length_mismatch() {
        let mut s = Scatter2D::new();
        assert!(!s.set_data(&[0.0, 1.0], &[0.0], &[0.0, 1.0]));
        assert!(s.is_empty(), "rejected data must not be partially stored");
        assert!(s.set_data(&[0.0, 1.0], &[2.0, 3.0], &[4.0, 5.0]));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn scatter2d_points_mode_lies_on_z0_plane_or_lifts_to_value() {
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 4.0);
        // Flat (default): z = 0 for every point.
        let flat = Scatter2D::new()
            .with_colormap(cmap.clone())
            .with_marker(PointMarker::Square)
            .with_size(8.0)
            .with_data(&[0.0, 1.0, 2.0], &[0.0, 0.0, 0.0], &[0.0, 2.0, 4.0]);
        let mut g = Scene3dGeometry::new();
        flat.append_to(&mut g);
        assert_eq!(g.points.len(), 3);
        for p in &g.points {
            assert_eq!(p.pos[2], 0.0, "flat scatter sits on z=0");
            assert_eq!(p.size, 8.0);
            assert_eq!(p.marker, PointMarker::Square.id());
        }
        // Colour is driven by value (same CPU colormap lookup as Scatter3D).
        let expect = |v: f64| {
            let [r, gg, b, a] = cmap.color_at(v);
            egui::Rgba::from(Color32::from_rgba_unmultiplied(r, gg, b, a)).to_array()
        };
        assert_eq!(g.points[0].color, expect(0.0));
        assert_eq!(g.points[2].color, expect(4.0));
        assert_ne!(g.points[0].color, g.points[2].color);

        // Height-map mode: z = value.
        let hm = Scatter2D::new()
            .with_data(&[0.0, 1.0, 2.0], &[0.0, 0.0, 0.0], &[0.0, 2.0, 4.0])
            .with_height_map(true);
        let mut g2 = Scene3dGeometry::new();
        hm.append_to(&mut g2);
        assert_eq!(g2.points[0].pos[2], 0.0);
        assert_eq!(g2.points[1].pos[2], 2.0);
        assert_eq!(g2.points[2].pos[2], 4.0);
    }

    #[test]
    fn scatter2d_lines_mode_emits_unique_triangulation_edges() {
        // Unit square → 2 Delaunay triangles sharing a diagonal → 5 unique edges.
        let s = Scatter2D::new()
            .with_data(
                &[0.0, 1.0, 0.0, 1.0],
                &[0.0, 0.0, 1.0, 1.0],
                &[0.0, 1.0, 2.0, 3.0],
            )
            .with_visualization(Scatter2DVisualization::Lines);
        let mut g = Scene3dGeometry::new();
        s.append_to(&mut g);
        // 5 segments, two vertices each; all flat on z = 0.
        assert_eq!(g.lines.len(), 10);
        for v in &g.lines {
            assert_eq!(v.pos[2], 0.0);
        }
        // The first edge is (index 0, index 1); its endpoints carry their own
        // colormap colour, so the segment gradients (values 0 vs 1 differ).
        assert_ne!(g.lines[0].color, g.lines[1].color);
        // Nothing emitted to the point / mesh channels.
        assert!(g.points.is_empty());
        assert!(g.meshes.is_empty());
    }

    #[test]
    fn scatter2d_solid_mode_fills_triangles_coloured_by_value() {
        let s = Scatter2D::new()
            .with_data(
                &[0.0, 1.0, 0.0, 1.0],
                &[0.0, 0.0, 1.0, 1.0],
                &[0.0, 1.0, 2.0, 3.0],
            )
            .with_visualization(Scatter2DVisualization::Solid);
        let mut g = Scene3dGeometry::new();
        s.append_to(&mut g);
        // 2 triangles × 3 vertices.
        assert_eq!(g.meshes.len(), 6);
        for v in &g.meshes {
            assert_eq!(v.pos[2], 0.0, "flat solid sits on z=0");
            // Flat z=0 triangles → the ±Z plane normal (winding-agnostic check).
            assert!(
                (v.normal[2].abs() - 1.0).abs() < 1e-5,
                "expected plane normal, got {:?}",
                v.normal
            );
        }
        assert!(g.points.is_empty());
        assert!(g.lines.is_empty());
    }

    #[test]
    fn scatter2d_degenerate_input_draws_nothing_in_triangulated_modes() {
        // Collinear points → empty Delaunay → no edges / triangles (silx skips the
        // renderer when the tesselation fails).
        let collinear =
            Scatter2D::new().with_data(&[0.0, 1.0, 2.0], &[0.0, 1.0, 2.0], &[0.0, 1.0, 2.0]);
        let mut g = Scene3dGeometry::new();
        collinear
            .clone()
            .with_visualization(Scatter2DVisualization::Lines)
            .append_to(&mut g);
        assert!(g.lines.is_empty());
        let mut g2 = Scene3dGeometry::new();
        collinear
            .with_visualization(Scatter2DVisualization::Solid)
            .append_to(&mut g2);
        assert!(g2.meshes.is_empty());
    }

    #[test]
    fn scatter2d_bounds_flat_collapses_z_height_map_spans_value() {
        assert!(Scatter2D::new().bounds().is_none());
        // Flat: the z bracket collapses to [0, 0].
        let flat = Scatter2D::new().with_data(&[-1.0, 2.0], &[3.0, -2.0], &[5.0, 10.0]);
        let (min, max) = flat.bounds().expect("non-empty");
        assert_eq!((min.x, min.y, min.z), (-1.0, -2.0, 0.0));
        assert_eq!((max.x, max.y, max.z), (2.0, 3.0, 0.0));
        // Height map: z spans the value range.
        let (min, max) = flat.with_height_map(true).bounds().expect("non-empty");
        assert_eq!((min.z, max.z), (5.0, 10.0));
    }

    // A flat, camera-facing triangle in the z=0 plane (CCW seen from +z).
    fn flat_tri() -> [[f32; 3]; 3] {
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
    }

    #[test]
    fn mesh_triangles_mode_emits_one_triangle_with_flat_normal() {
        let [a, b, c] = flat_tri();
        let mut m = Mesh3D::new();
        assert!(m.set_data(
            &[a, b, c],
            MeshColor::Uniform(Color32::from_rgb(255, 0, 0)),
            None,
            MeshDrawMode::Triangles,
            None,
        ));

        let mut g = Scene3dGeometry::new();
        m.append_to(&mut g);

        // Three mesh vertices (one triangle).
        assert_eq!(g.meshes.len(), 3);
        // No normals supplied → geometric flat normal (b−a)×(c−a) = +z, unit.
        for v in &g.meshes {
            assert_eq!(v.normal, [0.0, 0.0, 1.0]);
            assert_eq!(
                v.color,
                egui::Rgba::from(Color32::from_rgb(255, 0, 0)).to_array()
            );
        }
        assert_eq!(g.meshes[1].pos, b);
    }

    #[test]
    fn mesh_set_data_rejects_inconsistent_attributes() {
        let [a, b, c] = flat_tri();
        let mut m = Mesh3D::new();
        // Per-vertex colours shorter than the vertices.
        assert!(!m.set_data(
            &[a, b, c],
            MeshColor::PerVertex(vec![Color32::RED, Color32::GREEN]),
            None,
            MeshDrawMode::Triangles,
            None,
        ));
        // Normals not matching the vertex count.
        assert!(!m.set_data(
            &[a, b, c],
            MeshColor::Uniform(Color32::WHITE),
            Some(&[[0.0, 0.0, 1.0]]),
            MeshDrawMode::Triangles,
            None,
        ));
        // Index out of range.
        assert!(!m.set_data(
            &[a, b, c],
            MeshColor::Uniform(Color32::WHITE),
            None,
            MeshDrawMode::Triangles,
            Some(&[0, 1, 3]),
        ));
        assert!(m.is_empty(), "rejected data must not be partially stored");
        // A consistent per-vertex set is accepted.
        assert!(m.set_data(
            &[a, b, c],
            MeshColor::PerVertex(vec![Color32::RED, Color32::GREEN, Color32::BLUE]),
            Some(&[[0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]),
            MeshDrawMode::Triangles,
            Some(&[0, 1, 2]),
        ));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn mesh_strip_and_fan_expand_to_triangle_lists() {
        // Four collinear-in-index vertices; strip → 2 tris, fan → 2 tris.
        let p = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];

        let mut strip = Scene3dGeometry::new();
        Mesh3D::new()
            .with_data(
                &p,
                MeshColor::Uniform(Color32::WHITE),
                None,
                MeshDrawMode::TriangleStrip,
                None,
            )
            .append_to(&mut strip);
        // strip over 4 verts → (0,1,2),(1,2,3) → 2 triangles → 6 vertices.
        assert_eq!(strip.meshes.len(), 6);
        // Second triangle is vertices 1,2,3.
        assert_eq!(strip.meshes[3].pos, p[1]);
        assert_eq!(strip.meshes[4].pos, p[2]);
        assert_eq!(strip.meshes[5].pos, p[3]);

        let mut fan = Scene3dGeometry::new();
        Mesh3D::new()
            .with_data(
                &p,
                MeshColor::Uniform(Color32::WHITE),
                None,
                MeshDrawMode::Fan,
                None,
            )
            .append_to(&mut fan);
        // fan over 4 verts → (0,1,2),(0,2,3) → 2 triangles → 6 vertices.
        assert_eq!(fan.meshes.len(), 6);
        assert_eq!(fan.meshes[3].pos, p[0]); // shared apex
        assert_eq!(fan.meshes[4].pos, p[2]);
        assert_eq!(fan.meshes[5].pos, p[3]);
    }

    #[test]
    fn mesh_indices_unindex_before_expansion() {
        // Two stored vertices reused by indices to form one triangle.
        let p = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        let mut g = Scene3dGeometry::new();
        Mesh3D::new()
            .with_data(
                &p,
                MeshColor::Uniform(Color32::WHITE),
                None,
                MeshDrawMode::Triangles,
                Some(&[0, 1, 0]),
            )
            .append_to(&mut g);
        assert_eq!(g.meshes.len(), 3);
        assert_eq!(g.meshes[0].pos, p[0]);
        assert_eq!(g.meshes[1].pos, p[1]);
        assert_eq!(g.meshes[2].pos, p[0]);
    }

    #[test]
    fn colormap_mesh_colours_vertices_through_the_colormap() {
        let [a, b, c] = flat_tri();
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 2.0);
        let mut m = ColormapMesh3D::new().with_colormap(cmap.clone());
        assert!(m.set_data(
            &[a, b, c],
            &[0.0, 1.0, 2.0],
            None,
            MeshDrawMode::Triangles,
            None
        ));

        let mut g = Scene3dGeometry::new();
        m.append_to(&mut g);
        assert_eq!(g.meshes.len(), 3);

        let expect = |v: f64| {
            let [r, gg, bb, al] = cmap.color_at(v);
            egui::Rgba::from(Color32::from_rgba_unmultiplied(r, gg, bb, al)).to_array()
        };
        assert_eq!(g.meshes[0].color, expect(0.0));
        assert_eq!(g.meshes[2].color, expect(2.0));
        assert_ne!(g.meshes[0].color, g.meshes[2].color);
        // No normals → flat +z normal for the camera-facing triangle.
        assert_eq!(g.meshes[0].normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn colormap_mesh_rejects_value_length_mismatch_and_autoscales() {
        let [a, b, c] = flat_tri();
        let mut m = ColormapMesh3D::new();
        assert!(!m.set_data(&[a, b, c], &[0.0, 1.0], None, MeshDrawMode::Triangles, None));
        assert!(m.is_empty());
        assert!(m.set_data(
            &[a, b, c],
            &[-3.0, 0.0, 7.0],
            None,
            MeshDrawMode::Triangles,
            None
        ));
        let (vmin, vmax) = m.autoscale_colormap(AutoscaleMode::MinMax);
        assert_eq!((vmin, vmax), (-3.0, 7.0));
    }

    fn bounds_close(got: (Vec3, Vec3), min: [f32; 3], max: [f32; 3]) {
        let eps = 1e-4;
        let (g_min, g_max) = got;
        for (a, b) in [(g_min.x, min[0]), (g_min.y, min[1]), (g_min.z, min[2])] {
            assert!((a - b).abs() < eps, "min {a} vs {b}");
        }
        for (a, b) in [(g_max.x, max[0]), (g_max.y, max[1]), (g_max.z, max[2])] {
            assert!((a - b).abs() < eps, "max {a} vs {b}");
        }
    }

    #[test]
    fn box3d_default_is_a_centred_unit_cube() {
        let b = Box3D::new();
        let mut g = Scene3dGeometry::new();
        b.append_to(&mut g);
        // 4 side segments × 12 vertices = 48 vertices (16 triangles).
        assert_eq!(g.meshes.len(), 48);
        // A unit box centred at the origin spans ±0.5 on each axis.
        bounds_close(
            b.bounds().expect("box bounds"),
            [-0.5, -0.5, -0.5],
            [0.5, 0.5, 0.5],
        );
        assert_eq!(b.size(), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn box3d_rejects_bad_colour_count_and_tiles_per_position() {
        let mut b = Box3D::new();
        // Two positions but three colours → invalid.
        assert!(!b.set_data(
            [1.0, 1.0, 1.0],
            &[Color32::RED, Color32::GREEN, Color32::BLUE],
            &[[0.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
            (0.0, [0.0, 0.0, 0.0]),
        ));
        // One colour shared across two boxes → valid, doubles the vertex count.
        assert!(b.set_data(
            [1.0, 1.0, 1.0],
            &[Color32::RED],
            &[[0.0, 0.0, 0.0], [3.0, 0.0, 0.0]],
            (0.0, [0.0, 0.0, 0.0]),
        ));
        let mut g = Scene3dGeometry::new();
        b.append_to(&mut g);
        assert_eq!(g.meshes.len(), 96);
        // The two boxes span x from −0.5 (first box) to 3.5 (second centre +0.5).
        bounds_close(
            b.bounds().expect("bounds"),
            [-0.5, -0.5, -0.5],
            [3.5, 0.5, 0.5],
        );
    }

    #[test]
    fn cylinder3d_default_has_radial_side_normals() {
        let c = Cylinder3D::new();
        let mut g = Scene3dGeometry::new();
        c.append_to(&mut g);
        // 20 faces × 12 vertices = 240.
        assert_eq!(g.meshes.len(), 240);
        bounds_close(
            c.bounds().expect("cyl bounds"),
            [-1.0, -1.0, -0.5],
            [1.0, 1.0, 0.5],
        );
        // Smooth sides: the first side vertex (wedge index 3, segment 0) gets the
        // radial normal c2−c1 = (radius, 0, 0) = (1, 0, 0), not a faceted normal.
        assert_eq!(g.meshes[3].normal, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn hexagon3d_default_spans_its_hexagonal_footprint() {
        let h = Hexagon3D::new();
        let mut g = Scene3dGeometry::new();
        h.append_to(&mut g);
        // 6 faces × 12 vertices = 72.
        assert_eq!(g.meshes.len(), 72);
        // Vertices at 0°,60°,…,300°: x ∈ [−1, 1], y ∈ [−sin60°, sin60°].
        let s60 = (std::f32::consts::TAU / 6.0).sin();
        bounds_close(
            h.bounds().expect("hex bounds"),
            [-1.0, -s60, -0.5],
            [1.0, s60, 0.5],
        );
        assert_eq!((h.radius(), h.height()), (1.0, 1.0));
    }

    #[test]
    fn cylinder3d_face_count_controls_resolution() {
        let mut c = Cylinder3D::new();
        assert!(c.set_data(
            2.0,
            4.0,
            &[Color32::WHITE],
            8,
            &[[0.0, 0.0, 0.0]],
            (0.0, [0.0, 0.0, 0.0]),
        ));
        let mut g = Scene3dGeometry::new();
        c.append_to(&mut g);
        assert_eq!(g.meshes.len(), 8 * 12);
        bounds_close(
            c.bounds().expect("bounds"),
            [-2.0, -2.0, -2.0],
            [2.0, 2.0, 2.0],
        );
    }

    #[test]
    fn image_data3d_builds_a_colormapped_layer() {
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 3.0);
        let mut img = ImageData3D::new().with_colormap(cmap.clone());
        // 2×2 image, row-major.
        assert!(img.set_data(&[0.0, 1.0, 2.0, 3.0], 2, 2));
        assert_eq!(img.dimensions(), (2, 2));

        let mut g = Scene3dGeometry::new();
        img.append_to(&mut g);
        assert_eq!(g.images.len(), 1);
        let layer = &g.images[0];
        assert_eq!((layer.width, layer.height), (2, 2));
        assert_eq!(layer.pixels.len(), 2 * 2 * 4);

        // Each pixel is the colormap lookup, premultiplied-linear.
        let expect = |v: f64| {
            let [r, gg, b, a] = cmap.color_at(v);
            premul_linear_rgba8(Color32::from_rgba_unmultiplied(r, gg, b, a))
        };
        assert_eq!(&layer.pixels[0..4], &expect(0.0)); // (row0,col0)
        assert_eq!(&layer.pixels[12..16], &expect(3.0)); // (row1,col1)
        assert_ne!(&layer.pixels[0..4], &layer.pixels[12..16]);
    }

    #[test]
    fn image_data3d_rejects_size_mismatch_and_bounds_follow_origin_scale() {
        let mut img = ImageData3D::new();
        assert!(!img.set_data(&[0.0, 1.0, 2.0], 2, 2));
        assert!(img.is_empty());
        assert!(img.bounds().is_none());

        let img = ImageData3D::new()
            .with_data(&[0.0; 6], 3, 2)
            .with_origin([10.0, 20.0, -1.0])
            .with_scale([2.0, 5.0]);
        // Quad spans origin → origin + (w·sx, h·sy) at z = origin.z.
        let (min, max) = img.bounds().expect("bounds");
        assert_eq!((min.x, min.y, min.z), (10.0, 20.0, -1.0));
        assert_eq!(
            (max.x, max.y, max.z),
            (10.0 + 3.0 * 2.0, 20.0 + 2.0 * 5.0, -1.0)
        );
    }

    #[test]
    fn image_rgba3d_passes_pixels_through_premultiplied() {
        let cols = [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE];
        let mut img = ImageRgba3D::new();
        assert!(img.set_data(&cols, 2, 2));

        let mut g = Scene3dGeometry::new();
        img.append_to(&mut g);
        assert_eq!(g.images.len(), 1);
        let layer = &g.images[0];
        assert_eq!((layer.width, layer.height), (2, 2));
        for (i, &c) in cols.iter().enumerate() {
            assert_eq!(&layer.pixels[i * 4..i * 4 + 4], &premul_linear_rgba8(c));
        }
    }

    #[test]
    fn image_rgba3d_rejects_size_mismatch() {
        let mut img = ImageRgba3D::new();
        assert!(!img.set_data(&[Color32::RED, Color32::GREEN], 2, 2));
        assert!(img.is_empty());
        assert!(img.set_data(&[Color32::RED; 4], 2, 2));
        assert_eq!(img.dimensions(), (2, 2));
    }

    #[test]
    fn height_map_data_emits_one_square_point_per_pixel() {
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 3.0);
        let heights = [0.0_f32, 1.0, 2.0, 3.0]; // 2×2 field
        let mut hm = HeightMapData::new().with_colormap(cmap.clone());
        assert!(hm.set_data(&heights, 2, 2));
        assert!(hm.set_colormapped_data(&[0.0, 1.0, 2.0, 3.0], 2, 2));

        let mut g = Scene3dGeometry::new();
        hm.append_to(&mut g);
        assert_eq!(g.points.len(), 4);
        for p in &g.points {
            assert_eq!(p.size, 1.0);
            assert_eq!(p.marker, PointMarker::Square.id());
        }
        // Point (row=1, col=1) — index row*width+col = 3 — sits at world (1, 1, 3).
        let p11 = &g.points[3];
        assert_eq!(p11.pos, [1.0, 1.0, 3.0]);
        let expect = |v: f64| {
            let [r, gg, b, a] = cmap.color_at(v);
            egui::Rgba::from(Color32::from_rgba_unmultiplied(r, gg, b, a)).to_array()
        };
        assert_eq!(g.points[0].color, expect(0.0));
        assert_eq!(p11.color, expect(3.0));
    }

    #[test]
    fn height_map_data_empty_without_both_fields_and_bounds_from_heights() {
        let mut hm = HeightMapData::new();
        assert!(hm.set_data(&[0.0, 5.0, 2.0, 1.0], 2, 2));
        // Height field set, no colour data → draws nothing, but has spatial bounds.
        assert!(hm.is_empty());
        let mut g = Scene3dGeometry::new();
        hm.append_to(&mut g);
        assert!(g.points.is_empty());
        let (min, max) = hm.bounds().expect("bounds from heights");
        assert_eq!((min.x, min.y, min.z), (0.0, 0.0, 0.0)); // z min = 0.0
        assert_eq!((max.x, max.y, max.z), (1.0, 1.0, 5.0)); // grid 0..1, z max = 5.0
    }

    #[test]
    fn height_map_data_resamples_columns_by_width() {
        // 4×2 height field, 2×2 colour data: columns 0,1 → colour col 0; 2,3 → col 1.
        // This distinguishes width-based resample (correct) from silx's
        // height-based column indexing.
        let cmap = Colormap::new(ColormapName::Viridis, 0.0, 1.0);
        let heights = [0.0_f32; 8]; // 4 wide × 2 tall
        // colour data 2×2: col 0 = value 0.0, col 1 = value 1.0 (both rows).
        let values = [0.0, 1.0, 0.0, 1.0];
        let hm = HeightMapData::new()
            .with_colormap(cmap.clone())
            .with_data(&heights, 4, 2)
            .with_colormapped_data(&values, 2, 2);

        let mut g = Scene3dGeometry::new();
        hm.append_to(&mut g);
        assert_eq!(g.points.len(), 8);

        let c0 = egui::Rgba::from({
            let [r, gg, b, a] = cmap.color_at(0.0);
            Color32::from_rgba_unmultiplied(r, gg, b, a)
        })
        .to_array();
        // Row 0: cols 0,1 sample value-col 0 (0.0); cols 2,3 sample value-col 1.
        assert_eq!(g.points[0].color, c0); // col 0
        assert_eq!(g.points[1].color, c0); // col 1 → still value-col 0 (width-based)
        assert_ne!(g.points[2].color, c0); // col 2 → value-col 1
    }

    #[test]
    fn height_map_rgba_colours_points_directly() {
        let heights = [0.0_f32, 1.0, 2.0, 3.0];
        let cols = [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE];
        let mut hm = HeightMapRGBA::new();
        assert!(hm.set_data(&heights, 2, 2));
        assert!(hm.set_color_data(&cols, 2, 2));

        let mut g = Scene3dGeometry::new();
        hm.append_to(&mut g);
        assert_eq!(g.points.len(), 4);
        for (i, &c) in cols.iter().enumerate() {
            assert_eq!(g.points[i].color, egui::Rgba::from(c).to_array());
            assert_eq!(g.points[i].marker, PointMarker::Square.id());
        }
        assert_eq!(g.points[3].pos, [1.0, 1.0, 3.0]);
    }

    // --- ScalarField3D / Isosurface (P2.1b) ---

    /// A central high block in a 5×5×5 field at level 0.5 (rest 0).
    fn blob_field() -> (Vec<f32>, usize, usize, usize) {
        let (d, h, w) = (5usize, 5usize, 5usize);
        let mut data = vec![0.0f32; d * h * w];
        for z in 1..4 {
            for y in 1..4 {
                for x in 1..4 {
                    data[(z * h + y) * w + x] = 1.0;
                }
            }
        }
        (data, d, h, w)
    }

    #[test]
    fn scalar_field_rejects_bad_shape() {
        let mut sf = ScalarField3D::new();
        // Wrong length.
        assert!(!sf.set_data(&[0.0; 7], 2, 2, 2));
        // A dimension < 2 (silx asserts min(shape) >= 2).
        assert!(!sf.set_data(&[0.0; 2], 1, 2, 1));
        assert!(sf.is_empty());
        // Valid.
        assert!(sf.set_data(&[0.0; 8], 2, 2, 2));
        assert_eq!(sf.dimensions(), (2, 2, 2));
    }

    #[test]
    fn scalar_field_data_range_and_bounds() {
        let (data, d, h, w) = blob_field();
        let sf = ScalarField3D::new().with_data(&data, d, h, w);
        let (min, min_pos, max) = sf.data_range().expect("range");
        assert_eq!(min, 0.0);
        assert_eq!(max, 1.0);
        assert_eq!(min_pos, 1.0, "smallest positive sample is 1.0");
        // Volume box (0,0,0)..(width,height,depth).
        let (lo, hi) = sf.bounds().expect("bounds");
        assert_eq!(lo.to_array(), [0.0, 0.0, 0.0]);
        assert_eq!(hi.to_array(), [5.0, 5.0, 5.0]);
    }

    #[test]
    fn data_range_min_positive_nan_when_no_positive() {
        let sf = ScalarField3D::new().with_data(&[-1.0; 8], 2, 2, 2);
        let (min, min_pos, max) = sf.data_range().unwrap();
        assert_eq!(min, -1.0);
        assert_eq!(max, -1.0);
        assert!(min_pos.is_nan(), "no positive sample → NaN min positive");
    }

    #[test]
    fn add_remove_clear_isosurfaces() {
        let (data, d, h, w) = blob_field();
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        let i0 = sf.add_isosurface(0.5, Color32::RED);
        let i1 = sf.add_isosurface(0.25, DEFAULT_ISOSURFACE_COLOR);
        assert_eq!((i0, i1), (0, 1));
        assert_eq!(sf.isosurfaces().len(), 2);
        assert_eq!(sf.isosurfaces()[0].level(), 0.5);
        assert_eq!(sf.isosurfaces()[1].color(), DEFAULT_ISOSURFACE_COLOR);

        sf.isosurface_mut(0).unwrap().set_level(0.75);
        assert_eq!(sf.isosurfaces()[0].level(), 0.75);

        assert!(sf.remove_isosurface(0));
        assert!(!sf.remove_isosurface(5));
        assert_eq!(sf.isosurfaces().len(), 1);
        sf.clear_isosurfaces();
        assert!(sf.isosurfaces().is_empty());
    }

    #[test]
    fn auto_level_resolves_on_data_and_on_add() {
        let (data, d, h, w) = blob_field();
        // mean = 27/125 = 0.216; std = sqrt(mean*(1-mean)) for a 0/1 field.
        let expect = mean_plus_std(&data);
        assert!(expect.is_finite() && expect > 0.0);

        // Auto added before data → NaN until data is set, then resolved.
        let mut sf = ScalarField3D::new();
        sf.add_auto_isosurface(mean_plus_std, DEFAULT_ISOSURFACE_COLOR);
        assert!(sf.isosurfaces()[0].level().is_nan());
        assert!(sf.set_data(&data, d, h, w));
        assert!((sf.isosurfaces()[0].level() - expect).abs() < 1e-6);

        // Auto added after data → resolved immediately.
        let mut sf2 = ScalarField3D::new().with_data(&data, d, h, w);
        sf2.add_auto_isosurface(mean_plus_std, DEFAULT_ISOSURFACE_COLOR);
        assert!((sf2.isosurfaces()[0].level() - expect).abs() < 1e-6);
        assert!(sf2.isosurfaces()[0].is_auto_level());
    }

    #[test]
    fn mean_plus_std_ignores_non_finite_and_empty() {
        assert!(mean_plus_std(&[]).is_nan());
        assert!(mean_plus_std(&[f32::NAN, f32::INFINITY]).is_nan());
        // Constant field: std 0 → level == the constant.
        assert!((mean_plus_std(&[2.0, 2.0, 2.0]) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn isosurface_emits_swapped_offset_triangles() {
        let (data, d, h, w) = blob_field();
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        sf.add_isosurface(0.5, DEFAULT_ISOSURFACE_COLOR);

        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);

        // The closed surface of a 3×3×3 block has triangles (3 mesh vertices each).
        assert!(!g.meshes.is_empty(), "isosurface produced triangles");
        assert_eq!(g.meshes.len() % 3, 0, "triangles");

        let gold = egui::Rgba::from(DEFAULT_ISOSURFACE_COLOR).to_array();
        // All vertices: gold colour, inside the volume box, unit normals.
        for v in &g.meshes {
            assert_eq!(v.color, gold);
            for k in 0..3 {
                assert!(
                    v.pos[k] >= 0.0 && v.pos[k] <= 5.0,
                    "inside box: {:?}",
                    v.pos
                );
            }
            let n = v.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-4, "unit normal, got {len}");
        }
        // The block spans index [1,3]; crossings sit at 0.5 and 3.5 → world
        // [1.0, 4.0] after +0.5, so every coordinate is within [1.0, 4.0].
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for v in &g.meshes {
            for k in 0..3 {
                lo = lo.min(v.pos[k]);
                hi = hi.max(v.pos[k]);
            }
        }
        assert!(
            lo >= 1.0 - 1e-4 && hi <= 4.0 + 1e-4,
            "surface in [1,4]: {lo}..{hi}"
        );
    }

    #[test]
    fn non_finite_level_emits_nothing() {
        let (data, d, h, w) = blob_field();
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        sf.add_isosurface(f32::NAN, DEFAULT_ISOSURFACE_COLOR);
        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        assert!(g.meshes.is_empty(), "NaN level → no triangles");
    }

    #[test]
    fn cut_plane_hidden_by_default_emits_nothing() {
        let (data, d, h, w) = blob_field();
        let sf = ScalarField3D::new().with_data(&data, d, h, w);
        assert!(!sf.cut_plane().is_visible(), "cut plane hidden by default");
        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        assert!(g.textured_meshes.is_empty(), "hidden cut plane → no mesh");
    }

    #[test]
    fn cut_plane_config_setters() {
        let mut sf = ScalarField3D::new();
        let cp = sf.cut_plane_mut();
        cp.set_visible(true);
        cp.set_point(Vec3::new(1.0, 2.0, 3.0));
        cp.set_normal(Vec3::new(0.0, 0.0, 2.0)); // normalised to (0,0,1)
        cp.set_interpolation(ImageInterpolation::Nearest);
        cp.set_resolution(0); // clamps to ≥1
        assert!(sf.cut_plane().is_visible());
        assert_eq!(sf.cut_plane().plane().point().to_array(), [1.0, 2.0, 3.0]);
        assert_eq!(sf.cut_plane().plane().normal().to_array(), [0.0, 0.0, 1.0]);
        assert_eq!(sf.cut_plane().interpolation(), ImageInterpolation::Nearest);
        assert_eq!(sf.cut_plane().resolution(), 1);
    }

    #[test]
    fn plane_basis_is_orthonormal() {
        for n in [
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 2.0, 3.0).normalized(),
        ] {
            let (e1, e2) = plane_basis(n);
            assert!((e1.length() - 1.0).abs() < 1e-5, "e1 unit");
            assert!((e2.length() - 1.0).abs() < 1e-5, "e2 unit");
            assert!(e1.dot(n).abs() < 1e-5, "e1 ⟂ n");
            assert!(e2.dot(n).abs() < 1e-5, "e2 ⟂ n");
            assert!(e1.dot(e2).abs() < 1e-5, "e1 ⟂ e2");
        }
    }

    #[test]
    fn sample_field_value_nearest_and_linear() {
        // 2×2×2 field with distinct values: data[(z*2+y)*2+x] = index.
        let data: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let (d, h, w) = (2usize, 2usize, 2usize);
        // Sample exactly at voxel centre (1,0,1) → world (1.5, 0.5, 1.5).
        let v = sample_field_value(
            &data,
            d,
            h,
            w,
            Vec3::new(1.5, 0.5, 1.5),
            ImageInterpolation::Nearest,
        );
        assert_eq!(v, data[h * w + 1]); // (z=1, y=0, x=1) → (1*h+0)*w+1
        // Midway between the two x-voxels at y=0, z=0: world x=1.0 → fx=0.5.
        let v = sample_field_value(
            &data,
            d,
            h,
            w,
            Vec3::new(1.0, 0.5, 0.5),
            ImageInterpolation::Linear,
        );
        assert!((v - 0.5).abs() < 1e-5, "midpoint trilinear, got {v}");
        // Clamp-to-edge: far outside the box → the far-corner voxel (1,1,1).
        let v = sample_field_value(
            &data,
            d,
            h,
            w,
            Vec3::new(99.0, 99.0, 99.0),
            ImageInterpolation::Nearest,
        );
        assert_eq!(v, data[7], "clamps to far-corner voxel");
    }

    #[test]
    fn visible_axis_cut_plane_emits_textured_mesh() {
        let (data, d, h, w) = blob_field(); // 5×5×5, central 3×3×3 block = 1.0
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        sf.autoscale_cut_plane_colormap(AutoscaleMode::MinMax);
        {
            let cp = sf.cut_plane_mut();
            cp.set_normal(Vec3::new(0.0, 0.0, 1.0));
            cp.set_point(Vec3::new(2.5, 2.5, 2.5));
            cp.set_resolution(16);
            cp.set_visible(true);
        }
        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        assert_eq!(g.textured_meshes.len(), 1, "one cut-plane mesh");
        let m = &g.textured_meshes[0];
        // The z=2.5 plane ∩ the box is a square (4 contour verts) → fan = 2
        // triangles = 6 vertices.
        assert_eq!(m.vertices.len(), 6);
        assert_eq!(m.uvs.len(), 6);
        assert_eq!((m.width, m.height), (16, 16));
        assert_eq!(m.pixels.len(), 16 * 16 * 4, "res×res premultiplied RGBA8");
        // Every vertex lies on z=2.5 and the contour spans the full box face.
        let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for v in &m.vertices {
            assert!((v[2] - 2.5).abs() < 1e-4, "on the z=2.5 plane");
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        assert_eq!([lo[0], lo[1]], [0.0, 0.0]);
        assert_eq!([hi[0], hi[1]], [5.0, 5.0]);
    }

    #[test]
    fn autoscale_cut_plane_colormap_fits_data_range() {
        let (data, d, h, w) = blob_field();
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        let (vmin, vmax) = sf.autoscale_cut_plane_colormap(AutoscaleMode::MinMax);
        assert_eq!((vmin, vmax), (0.0, 1.0));
        assert_eq!(sf.cut_plane().colormap().vmin, 0.0);
        assert_eq!(sf.cut_plane().colormap().vmax, 1.0);
    }

    #[test]
    fn cut_plane_not_slicing_the_volume_emits_nothing() {
        let (data, d, h, w) = blob_field();
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        {
            let cp = sf.cut_plane_mut();
            cp.set_normal(Vec3::new(0.0, 0.0, 1.0));
            cp.set_point(Vec3::new(2.5, 2.5, 100.0)); // z=100, outside the box
            cp.set_visible(true);
        }
        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        assert!(
            g.textured_meshes.is_empty(),
            "plane misses the volume → no mesh"
        );
        assert!(g.lines.is_empty(), "plane misses the volume → no stroke");
    }

    #[test]
    fn visible_cut_plane_emits_white_contour_stroke() {
        // silx PlaneInGroup: the visible plane draws its plane/box intersection
        // as a closed white line loop (primitives.py:1082-1126).
        let (data, d, h, w) = blob_field(); // 5×5×5
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        {
            let cp = sf.cut_plane_mut();
            cp.set_normal(Vec3::new(0.0, 0.0, 1.0));
            cp.set_point(Vec3::new(2.5, 2.5, 2.5));
            cp.set_visible(true);
        }
        assert!(sf.cut_plane().is_stroke_visible(), "stroke on by default");
        assert_eq!(sf.cut_plane().stroke_color(), Color32::WHITE);

        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        // z=2.5 ∩ box = a square → 4 contour vertices → 4 loop segments.
        assert_eq!(g.lines.len(), 8, "4 closed-loop segments, 2 verts each");
        let white = egui::Rgba::from(Color32::WHITE).to_array();
        for v in &g.lines {
            assert_eq!(v.color, white, "stroke is white by default");
            assert!((v.pos[2] - 2.5).abs() < 1e-4, "stroke lies on the plane");
        }
        // Closed loop: every endpoint appears exactly twice.
        let mut counts: Vec<([i32; 3], usize)> = Vec::new();
        for v in &g.lines {
            let key = [0, 1, 2].map(|k| (v.pos[k] * 1024.0).round() as i32);
            match counts.iter_mut().find(|(p, _)| *p == key) {
                Some((_, n)) => *n += 1,
                None => counts.push((key, 1)),
            }
        }
        assert_eq!(counts.len(), 4, "four distinct corners");
        assert!(
            counts.iter().all(|&(_, n)| n == 2),
            "each corner shared by two segments (closed loop): {counts:?}"
        );
    }

    #[test]
    fn cut_plane_stroke_color_and_visibility_api() {
        let (data, d, h, w) = blob_field();
        let mut sf = ScalarField3D::new().with_data(&data, d, h, w);
        {
            let cp = sf.cut_plane_mut();
            cp.set_normal(Vec3::new(0.0, 0.0, 1.0));
            cp.set_point(Vec3::new(2.5, 2.5, 2.5));
            cp.set_visible(true);
            cp.set_stroke_color(Color32::RED); // ScalarFieldView.setStrokeColor
        }
        let mut g = Scene3dGeometry::new();
        sf.append_to(&mut g);
        let red = egui::Rgba::from(Color32::RED).to_array();
        assert!(!g.lines.is_empty());
        assert!(g.lines.iter().all(|v| v.color == red));

        // strokeVisible = False suppresses the contour but keeps the slice.
        sf.cut_plane_mut().set_stroke_visible(false);
        let mut g2 = Scene3dGeometry::new();
        sf.append_to(&mut g2);
        assert!(g2.lines.is_empty(), "stroke hidden → no loop");
        assert_eq!(g2.textured_meshes.len(), 1, "slice still drawn");

        // Hidden plane: neither slice nor stroke.
        sf.cut_plane_mut().set_stroke_visible(true);
        sf.cut_plane_mut().set_visible(false);
        let mut g3 = Scene3dGeometry::new();
        sf.append_to(&mut g3);
        assert!(g3.lines.is_empty() && g3.textured_meshes.is_empty());
    }

    /// A 2×2×2 complex field with one distinctive sample (`3 + 4i`) so each
    /// projection is checkable; the rest are zero.
    fn complex_field() -> (Vec<f32>, Vec<f32>, usize, usize, usize) {
        let (d, h, w) = (2usize, 2usize, 2usize);
        let mut re = vec![0.0f32; d * h * w];
        let mut im = vec![0.0f32; d * h * w];
        re[0] = 3.0;
        im[0] = 4.0;
        (re, im, d, h, w)
    }

    #[test]
    fn complex_mode_projections() {
        // 3 + 4i: |z| = 5, |z|² = 25, phase = atan2(4,3), re = 3, im = 4.
        assert_eq!(ComplexMode::Absolute.to_scalar(3.0, 4.0), 5.0);
        assert_eq!(ComplexMode::SquareAmplitude.to_scalar(3.0, 4.0), 25.0);
        assert_eq!(ComplexMode::Real.to_scalar(3.0, 4.0), 3.0);
        assert_eq!(ComplexMode::Imaginary.to_scalar(3.0, 4.0), 4.0);
        assert!((ComplexMode::Phase.to_scalar(3.0, 4.0) - 4.0f32.atan2(3.0)).abs() < 1e-6);
        // The two hue-display modes have no scalar (project to 0.0).
        assert_eq!(ComplexMode::AmplitudePhase.to_scalar(3.0, 4.0), 0.0);
    }

    #[test]
    fn complex_field_rejects_bad_shape() {
        let mut cf = ComplexField3D::new();
        // re/im length disagree.
        assert!(!cf.set_data(&[0.0; 8], &[0.0; 7], 2, 2, 2));
        // Wrong length for the dims.
        assert!(!cf.set_data(&[0.0; 7], &[0.0; 7], 2, 2, 2));
        // A dimension < 2.
        assert!(!cf.set_data(&[0.0; 2], &[0.0; 2], 1, 2, 1));
        assert!(cf.is_empty());
        // Valid.
        assert!(cf.set_data(&[0.0; 8], &[0.0; 8], 2, 2, 2));
        assert_eq!(cf.dimensions(), (2, 2, 2));
    }

    #[test]
    fn complex_field_projects_into_inner_field_per_mode() {
        let (re, im, d, h, w) = complex_field();
        let cf = ComplexField3D::new().with_data(&re, &im, d, h, w);
        // Default amplitude: sample 0 → 5, the rest 0 → range (0, 5, 5).
        assert_eq!(cf.complex_mode(), ComplexMode::Absolute);
        assert_eq!(cf.field().data()[0], 5.0);
        assert_eq!(
            cf.data_range_for(ComplexMode::Absolute),
            Some((0.0, 5.0, 5.0))
        );
        assert_eq!(
            cf.data_range_for(ComplexMode::SquareAmplitude),
            Some((0.0, 25.0, 25.0))
        );
        // projected_data is independent of the current mode.
        assert_eq!(cf.projected_data(ComplexMode::Real).unwrap()[0], 3.0);
        assert_eq!(cf.projected_data(ComplexMode::Imaginary).unwrap()[0], 4.0);
    }

    #[test]
    fn set_complex_mode_reprojects_and_clears_isosurfaces() {
        let (re, im, d, h, w) = complex_field();
        let mut cf = ComplexField3D::new().with_data(&re, &im, d, h, w);
        cf.field_mut().add_isosurface(2.0, DEFAULT_ISOSURFACE_COLOR);
        assert_eq!(cf.field().isosurfaces().len(), 1);
        assert_eq!(cf.field().data()[0], 5.0); // amplitude

        // Switching mode reprojects the inner field and clears iso-surfaces.
        cf.set_complex_mode(ComplexMode::SquareAmplitude);
        assert_eq!(cf.complex_mode(), ComplexMode::SquareAmplitude);
        assert_eq!(cf.field().data()[0], 25.0);
        assert!(
            cf.field().isosurfaces().is_empty(),
            "mode change clears iso-surfaces (silx setComplexMode)"
        );

        // Same-mode set is a no-op (keeps any newly added iso-surfaces).
        cf.field_mut()
            .add_isosurface(10.0, DEFAULT_ISOSURFACE_COLOR);
        cf.set_complex_mode(ComplexMode::SquareAmplitude);
        assert_eq!(
            cf.field().isosurfaces().len(),
            1,
            "unchanged mode is a no-op"
        );
    }

    #[test]
    fn complex_field_cut_plane_persists_across_mode_change() {
        let (re, im, d, h, w) = complex_field();
        let mut cf = ComplexField3D::new().with_data(&re, &im, d, h, w);
        cf.field_mut().cut_plane_mut().set_visible(true);
        cf.set_complex_mode(ComplexMode::Phase);
        assert!(
            cf.field().cut_plane().is_visible(),
            "the cut plane survives a mode change (only iso-surfaces are cleared)"
        );
    }
}
