//! 3D data items — the `silx.gui.plot3d.items` port.
//!
//! Items hold data plus presentation state (colormap, marker, size) and emit
//! their geometry into a [`Scene3dGeometry`] via [`append_to`](Scatter3D::append_to),
//! the analogue of silx's scene-primitive build. The GPU primitives themselves
//! live in [`crate::render::gpu_scene3d`]; this module is the headless item layer
//! (color mapping + bounds), unit-tested without a GPU.

use egui::Color32;

use crate::core::colormap::{AutoscaleMode, Colormap, ColormapName};
use crate::core::scene3d::mat4::{Mat4, Vec3, mat4_rotate};
use crate::render::gpu_scene3d::{PointMarker, Scene3dGeometry, flat_normal};

/// silx's default plot symbol size in pixels (`_config.DEFAULT_PLOT_SYMBOL_SIZE`).
pub const DEFAULT_SCATTER3D_SIZE: f32 = 6.0;

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
}

impl Default for Scatter3D {
    fn default() -> Self {
        Self::new()
    }
}

impl Scatter3D {
    /// An empty scatter with silx defaults: the viridis colormap over `[0, 1]`,
    /// circle markers at [`DEFAULT_SCATTER3D_SIZE`].
    pub fn new() -> Self {
        Self {
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            values: Vec::new(),
            colormap: Colormap::new(ColormapName::Viridis, 0.0, 1.0),
            marker: PointMarker::Circle,
            size: DEFAULT_SCATTER3D_SIZE,
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
        let (vmin, vmax) = mode.range(&self.values, self.colormap.autoscale_percentiles);
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

    /// Axis-aligned data bounds `(min, max)` over the points (silx
    /// `DataItem3D.getBounds`), or `None` when empty. Useful to frame a
    /// [`crate::widget::scene_widget::SceneWidget`].
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
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

    /// Append this scatter's points (coloured through the colormap) to
    /// `geometry`, ready to upload via [`crate::render::gpu_scene3d::set_scene3d`].
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        for i in 0..self.len() {
            let [r, g, b, a] = self.colormap.color_at(self.values[i]);
            geometry.add_point(
                [self.x[i], self.y[i], self.z[i]],
                Color32::from_rgba_unmultiplied(r, g, b, a),
                self.size,
                self.marker,
            );
        }
    }
}

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
/// per triangle (via [`flat_normal`]), so the headlight still shades the surface.
/// Strips and fans are expanded to a triangle list on the CPU since the GPU path
/// is `TriangleList` only.
#[derive(Clone, Debug)]
pub struct Mesh3D {
    positions: Vec<[f32; 3]>,
    colors: MeshColor,
    normals: Option<Vec<[f32; 3]>>,
    mode: MeshDrawMode,
    indices: Option<Vec<u32>>,
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

    /// Axis-aligned data bounds `(min, max)`, or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        positions_bounds(&self.positions)
    }

    /// Append this mesh's triangles to `geometry` for upload via
    /// [`crate::render::gpu_scene3d::set_scene3d`].
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
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
            geometry.add_mesh_triangle_rgba(p, rgba, normals);
        }
    }
}

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
}

impl Default for ColormapMesh3D {
    fn default() -> Self {
        Self::new()
    }
}

impl ColormapMesh3D {
    /// An empty colormap mesh with silx defaults: the viridis colormap over
    /// `[0, 1]`, `Triangles` mode.
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
            values: Vec::new(),
            normals: None,
            mode: MeshDrawMode::Triangles,
            indices: None,
            colormap: Colormap::new(ColormapName::Viridis, 0.0, 1.0),
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
        let (vmin, vmax) = mode.range(&self.values, self.colormap.autoscale_percentiles);
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

    /// Axis-aligned data bounds `(min, max)`, or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        positions_bounds(&self.positions)
    }

    /// Append this mesh's triangles (coloured through the colormap) to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        let rgba_at = |i: usize| {
            let [r, g, b, a] = self.colormap.color_at(self.values[i]);
            egui::Rgba::from(Color32::from_rgba_unmultiplied(r, g, b, a)).to_array()
        };
        for [i0, i1, i2] in
            expand_triangles(self.mode, self.positions.len(), self.indices.as_deref())
        {
            let p = [self.positions[i0], self.positions[i1], self.positions[i2]];
            let normals = match &self.normals {
                Some(ns) => [ns[i0], ns[i1], ns[i2]],
                None => [flat_normal(p[0], p[1], p[2]); 3],
            };
            geometry.add_mesh_triangle_rgba(p, [rgba_at(i0), rgba_at(i1), rgba_at(i2)], normals);
        }
    }
}

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
/// [`cylindrical_volume_mesh`] with faceted faces.
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
        self.mesh = cylindrical_volume_mesh(
            &self.positions,
            diagonal / 2.0,
            dz,
            &angles,
            &self.colors,
            true,
            rotation_matrix(rotation.0, rotation.1),
        );
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

    /// Axis-aligned bounds `(min, max)` of the box mesh, or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.mesh.bounds()
    }

    /// Append the box triangles to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.mesh.append_to(geometry);
    }
}

/// One or many cylinders (silx `items.mesh.Cylinder`), an `nb_faces`-segment
/// [`cylindrical_volume_mesh`] with smooth (radial-normal) sides.
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
        self.mesh = cylindrical_volume_mesh(
            &self.positions,
            self.radius,
            self.height,
            &angles,
            &self.colors,
            false,
            rotation_matrix(rotation.0, rotation.1),
        );
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

    /// Axis-aligned bounds `(min, max)` of the cylinder mesh, or `None` if empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.mesh.bounds()
    }

    /// Append the cylinder triangles to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.mesh.append_to(geometry);
    }
}

/// One or many uniform hexagonal prisms (silx `items.mesh.Hexagon`), a
/// six-segment [`cylindrical_volume_mesh`] with faceted faces.
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
        self.mesh = cylindrical_volume_mesh(
            &self.positions,
            self.radius,
            self.height,
            &angles,
            &self.colors,
            true,
            rotation_matrix(rotation.0, rotation.1),
        );
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

    /// Axis-aligned bounds `(min, max)` of the prism mesh, or `None` when empty.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        self.mesh.bounds()
    }

    /// Append the prism triangles to `geometry`.
    pub fn append_to(&self, geometry: &mut Scene3dGeometry) {
        self.mesh.append_to(geometry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
