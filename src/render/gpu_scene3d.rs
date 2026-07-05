//! The 3D scene renderer — wgpu line/triangle pipelines that draw depth-tested
//! geometry into an offscreen color+depth target, then blit that color into
//! egui's (depth-less) paint pass.
//!
//! This is the plot3d analogue of [`crate::render::backend_wgpu`]: persistent
//! GPU state ([`Scene3dResources`]) lives in `egui_wgpu`'s `callback_resources`
//! type map, installed once via [`install_scene3d`]; the egui side re-registers
//! a lightweight `Scene3dCallback` each frame via [`paint_scene3d`].
//!
//! Why offscreen-then-blit: egui's render pass has **no depth attachment**
//! (`doc/plot3d-parity-roadmap.md` Architecture), so depth-tested 3D cannot
//! draw straight into it. Each frame:
//!
//! - `prepare()` sizes an offscreen color+depth texture pair to the widget's
//!   pixel rect, writes the camera MVP uniform, and encodes one depth-tested
//!   pass (clear → triangles → lines) into the offscreen color target.
//! - `paint()` blits that color texture into egui's pass as a viewport-clipped
//!   full-screen triangle.
//!
//! Geometry is uploaded once via [`set_scene3d`] (mirroring `set_curves`); the
//! per-frame camera transform is applied in the shader from the MVP uniform.

use std::collections::HashMap;
use std::num::NonZeroU64;

use egui::Color32;
use egui_wgpu::{RenderState, wgpu};

use crate::core::scene3d::camera::Camera;
use crate::core::scene3d::mat4::{Mat4, Vec3};

/// Scene identity key (mirrors [`crate::core::plot::PlotId`]); lets several
/// independent 3D scenes coexist in one egui app without sharing GPU state.
pub type Scene3dId = u64;

/// Offscreen depth-buffer format. 32-bit float — ample range for the camera's
/// near/far span, and universally supported as a render attachment.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// One scene vertex: world-space position + linear-premultiplied RGBA. Used by
/// both the line and triangle pipelines (shared vertex layout). `repr(C)` so the
/// 28-byte stride matches the WGSL vertex attributes exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Scene3dVertex {
    /// World-space position (the model transform, if any, is folded into the MVP).
    pub pos: [f32; 3],
    /// Linear color space, premultiplied alpha — same convention as the 2D path.
    pub color: [f32; 4],
}

/// Vertex attributes for [`Scene3dVertex`]: position at location 0 (offset 0),
/// color at location 1 (offset 12). Kept as a `'static` const so the
/// [`wgpu::VertexBufferLayout`] can borrow it for pipeline creation.
const SCENE3D_VERTEX_ATTRS: [wgpu::VertexAttribute; 2] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 12,
        shader_location: 1,
    },
];

/// Point-sprite marker shape — the silx `_Points` markers (`SUPPORTED_MARKERS`).
/// The discriminant order matches the `marker` id read by the `alpha_symbol`
/// switch in `scene3d_points.wgsl`; keep the two in lock-step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PointMarker {
    /// `'o'` — filled circle (silx default).
    #[default]
    Circle,
    /// `'d'` — diamond.
    Diamond,
    /// `'s'` — square (the full sprite).
    Square,
    /// `'+'` — plus.
    Plus,
    /// `'x'` — diagonal cross.
    Cross,
    /// `'*'` — asterisk (plus + cross + soft circle edge).
    Asterisk,
    /// `'_'` — horizontal line.
    HLine,
    /// `'|'` — vertical line.
    VLine,
}

impl PointMarker {
    /// The `marker` id handed to the GPU; must match `scene3d_points.wgsl`.
    pub fn id(self) -> u32 {
        match self {
            PointMarker::Circle => 0,
            PointMarker::Diamond => 1,
            PointMarker::Square => 2,
            PointMarker::Plus => 3,
            PointMarker::Cross => 4,
            PointMarker::Asterisk => 5,
            PointMarker::HLine => 6,
            PointMarker::VLine => 7,
        }
    }
}

/// One scatter point (one instance): world-space centre, linear-premultiplied
/// RGBA, pixel diameter, and marker id. `repr(C)` so the 36-byte stride matches
/// `SCENE3D_POINT_ATTRS` and `scene3d_points.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Scene3dPoint {
    /// World-space centre (the model transform, if any, folds into the MVP).
    pub pos: [f32; 3],
    /// Linear color space, premultiplied alpha — same convention as the 2D path.
    pub color: [f32; 4],
    /// Sprite diameter in physical pixels (silx `gl_PointSize`).
    pub size: f32,
    /// Marker shape id (see [`PointMarker::id`]).
    pub marker: u32,
}

/// Per-instance attributes for [`Scene3dPoint`]: pos at location 0 (offset 0),
/// color at 1 (offset 12), size at 2 (offset 28), marker at 3 (offset 32).
const SCENE3D_POINT_ATTRS: [wgpu::VertexAttribute; 4] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 12,
        shader_location: 1,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 28,
        shader_location: 2,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Uint32,
        offset: 32,
        shader_location: 3,
    },
];

/// One shaded-mesh vertex: world-space position, linear-premultiplied RGBA, and
/// a world-space normal for lighting. `repr(C)` so the 40-byte stride matches
/// `SCENE3D_MESH_ATTRS` and `scene3d_mesh.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Scene3dMeshVertex {
    /// World-space position (model is identity; items bake world coordinates).
    pub pos: [f32; 3],
    /// Linear color space, premultiplied alpha — same convention as the 2D path.
    pub color: [f32; 4],
    /// World-space surface normal (need not be unit; the shader normalizes).
    pub normal: [f32; 3],
}

/// Per-vertex attributes for [`Scene3dMeshVertex`]: pos at location 0 (offset 0),
/// color at 1 (offset 12), normal at 2 (offset 28).
const SCENE3D_MESH_ATTRS: [wgpu::VertexAttribute; 3] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x4,
        offset: 12,
        shader_location: 1,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 28,
        shader_location: 2,
    },
];

/// One textured-primitive vertex: world-space position + texture UV. Shared by
/// the image quad and the arbitrary-triangle textured mesh (the cut plane).
/// `repr(C)` so the 20-byte stride matches [`SCENE3D_IMAGE_ATTRS`] and
/// `scene3d_image.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene3dImageVertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

/// Per-vertex attributes for [`Scene3dImageVertex`]: pos at location 0 (offset 0),
/// uv at 1 (offset 12).
const SCENE3D_IMAGE_ATTRS: [wgpu::VertexAttribute; 2] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x2,
        offset: 12,
        shader_location: 1,
    },
];

/// Texture sampling for an image layer (silx `InterpolationMixIn`: 'nearest' vs
/// 'linear').
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ImageInterpolation {
    /// Nearest-neighbour — crisp pixels (silx default for `ImageData`/`ImageRgba`).
    #[default]
    Nearest,
    /// Bilinear — smooth.
    Linear,
}

/// One image layer: a `width × height` premultiplied-linear RGBA8 raster placed
/// as a quad in the scene. The quad spans pixel-corner `origin` to
/// `origin + (width·scale.x, height·scale.y)` in the `z = origin.z` plane (silx
/// `ImageData`/`ImageRgba`: pixel `(col, row)` → world `(x, y)`). `pixels` is
/// row-major (row 0 first), length `width · height · 4`.
#[derive(Clone, Debug)]
pub struct Scene3dImageLayer {
    /// Premultiplied-linear RGBA8, row-major, length `width · height · 4`.
    pub pixels: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// World position of pixel-corner `(0, 0)`.
    pub origin: [f32; 3],
    /// World size of one pixel along x and y.
    pub scale: [f32; 2],
    /// Nearest vs linear sampling.
    pub interpolation: ImageInterpolation,
}

/// One textured arbitrary-triangle mesh: a `width × height` premultiplied-linear
/// RGBA8 raster sampled across a triangle list at explicit world positions. This
/// is the general case of [`Scene3dImageLayer`] (whose geometry is fixed to an
/// axis-aligned quad) — used by the cut plane, which maps a colormapped slice of
/// the volume onto the (arbitrary) plane∩box contour polygon.
///
/// `vertices` is a flat triangle list (every three a triangle, `TriangleList`
/// topology); `uvs` carries the matching per-vertex texture coordinate into
/// `pixels` (same length as `vertices`). `pixels` is row-major (row 0 first),
/// length `width · height · 4`.
#[derive(Clone, Debug)]
pub struct Scene3dTexturedMesh {
    /// Premultiplied-linear RGBA8, row-major, length `width · height · 4`.
    pub pixels: Vec<u8>,
    /// Texture width in pixels.
    pub width: u32,
    /// Texture height in pixels.
    pub height: u32,
    /// World-space triangle-list vertices (length a multiple of 3).
    pub vertices: Vec<[f32; 3]>,
    /// Per-vertex texture UV (same length as `vertices`).
    pub uvs: Vec<[f32; 2]>,
    /// Nearest vs linear sampling.
    pub interpolation: ImageInterpolation,
}

/// Uniform block for `scene3d.wgsl` **and** `scene3d_image.wgsl` (the image
/// pipeline binds the same buffer at group 0): the column-major, clip-corrected
/// MVP plus the linear-fog datum.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene3dParams {
    /// `camera.matrix() × model`, transposed to column-major and depth-corrected
    /// for wgpu z∈[0,1] (see [`crate::core::scene3d::mat4::Mat4::to_gpu_clip_cols`]).
    mvp: [[f32; 4]; 4],
    /// silx `fogExtentInfo` (function.py:135-146): `(scale, near, on, 0)`.
    fog_info: [f32; 4],
    /// silx `fogColor` = viewport background rgb (function.py:148-151); w unused.
    fog_color: [f32; 4],
    /// Row 2 of the view matrix: `dot(view_row_z, (pos, 1))` = camera-space z.
    view_row_z: [f32; 4],
}

/// Uniform block for `scene3d_mesh.wgsl`: the clip MVP, the camera-space
/// normal transform (the view matrix, column-major, no depth correction), the
/// fog datum, and the Phong shininess.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene3dMeshParams {
    mvp: [[f32; 4]; 4],
    normal_mat: [[f32; 4]; 4],
    fog_info: [f32; 4],
    fog_color: [f32; 4],
    /// `(shininess, 0, 0, 0)` — 0 disables the specular term, the silx
    /// `DirectionalLight` default (function.py:296-300).
    light: [f32; 4],
}

/// Uniform block for `scene3d_points.wgsl`: the MVP, the fog datum, plus the
/// offscreen viewport pixel size (the sprite-corner offset is computed in
/// pixels then converted to NDC, so the shader needs the viewport extent).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Scene3dPointParams {
    mvp: [[f32; 4]; 4],
    fog_info: [f32; 4],
    fog_color: [f32; 4],
    view_row_z: [f32; 4],
    viewport: [f32; 2],
    _pad: [f32; 2],
}

/// CPU-side geometry for one scene: a flat line-list and a flat triangle-list,
/// each vertex carrying its own color. Build with [`Scene3dGeometry::add_line`]
/// / [`Scene3dGeometry::add_triangle`], then upload via [`set_scene3d`].
#[derive(Clone, Debug, Default)]
pub struct Scene3dGeometry {
    /// Pairs of vertices, each pair one line segment (`LineList` topology).
    pub(crate) lines: Vec<Scene3dVertex>,
    /// Triples of vertices, each triple one triangle (`TriangleList` topology),
    /// flat-shaded (no lighting) — chrome and simple fills.
    pub(crate) triangles: Vec<Scene3dVertex>,
    /// Scatter points, each drawn as a billboarded marker sprite.
    pub(crate) points: Vec<Scene3dPoint>,
    /// Triples of vertices, each triple one triangle of a **lit** mesh (carries
    /// per-vertex normals; `TriangleList` topology).
    pub(crate) meshes: Vec<Scene3dMeshVertex>,
    /// Textured image quads (one texture each), drawn after the opaque geometry.
    pub(crate) images: Vec<Scene3dImageLayer>,
    /// Textured arbitrary-triangle meshes (one texture each), drawn in the same
    /// alpha-blended textured pass as the image quads. Used by the cut plane.
    pub(crate) textured_meshes: Vec<Scene3dTexturedMesh>,
    /// Pick-only data-point anchors: positions that are hit-testable but draw
    /// nothing. Emitted by `Scatter2D` in LINES mode, where silx picks at the
    /// data points (5 px square) rather than along the segments
    /// (`items/scatter.py:509-511` → `_pickPoints(..., threshold=5.0)`).
    pub(crate) line_pick_anchors: Vec<[f32; 3]>,
}

impl Scene3dGeometry {
    /// An empty geometry.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when there is nothing to draw or pick.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
            && self.triangles.is_empty()
            && self.points.is_empty()
            && self.meshes.is_empty()
            && self.images.is_empty()
            && self.textured_meshes.is_empty()
            && self.line_pick_anchors.is_empty()
    }

    /// Drop all geometry, keeping allocated capacity for reuse.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.triangles.clear();
        self.points.clear();
        self.meshes.clear();
        self.images.clear();
        self.textured_meshes.clear();
        self.line_pick_anchors.clear();
    }

    /// Append a textured image layer (see [`Scene3dImageLayer`]).
    pub fn add_image_layer(&mut self, layer: Scene3dImageLayer) {
        self.images.push(layer);
    }

    /// Append a textured arbitrary-triangle mesh (see [`Scene3dTexturedMesh`]).
    pub fn add_textured_mesh(&mut self, mesh: Scene3dTexturedMesh) {
        self.textured_meshes.push(mesh);
    }

    /// Append all of `other`'s primitives onto this geometry, every channel
    /// (lines, triangles, points, meshes, images, textured meshes). The single
    /// owner of the geometry-merge rule, so a composite (e.g. chrome + data
    /// items) forwards every primitive kind, not a hand-picked subset.
    pub fn extend_from(&mut self, other: &Scene3dGeometry) {
        self.lines.extend_from_slice(&other.lines);
        self.triangles.extend_from_slice(&other.triangles);
        self.points.extend_from_slice(&other.points);
        self.meshes.extend_from_slice(&other.meshes);
        self.images.extend_from_slice(&other.images);
        self.textured_meshes
            .extend_from_slice(&other.textured_meshes);
        self.line_pick_anchors
            .extend_from_slice(&other.line_pick_anchors);
    }

    /// World-space triangles for CPU picking, as `[v0, v1, v2]` triples: the
    /// flat-shaded `triangles` channel followed by the lit `meshes` channel
    /// (iso-surfaces, colormapped meshes, the cylindrical-volume primitives).
    /// Image quads and textured meshes (the cut plane) are excluded — those are
    /// picked as planes/volumes by the field-aware pickers, not as raw triangles.
    /// Used by [`crate::SceneWidget::pick`].
    pub fn pick_triangles(&self) -> Vec<[Vec3; 3]> {
        let mut out = Vec::with_capacity(self.triangles.len() / 3 + self.meshes.len() / 3);
        for tri in self.triangles.chunks_exact(3) {
            out.push([
                Vec3::from_array(tri[0].pos),
                Vec3::from_array(tri[1].pos),
                Vec3::from_array(tri[2].pos),
            ]);
        }
        for tri in self.meshes.chunks_exact(3) {
            out.push([
                Vec3::from_array(tri[0].pos),
                Vec3::from_array(tri[1].pos),
                Vec3::from_array(tri[2].pos),
            ]);
        }
        out
    }

    /// World-space scatter-point positions (the `points` channel), for
    /// threshold picking. Used by [`crate::SceneWidget::pick`].
    pub fn pick_points(&self) -> Vec<Vec3> {
        self.points
            .iter()
            .map(|p| Vec3::from_array(p.pos))
            .collect()
    }

    /// Append a pick-only anchor at `pos`: hit-testable by
    /// [`crate::SceneWidget::pick`] (silx `_pickPoints`, 5 px square) but never
    /// drawn. Used by `Scatter2D` LINES mode, which silx picks at its data
    /// points, not along the segments (`items/scatter.py:509-511`).
    pub fn add_line_pick_anchor(&mut self, pos: [f32; 3]) {
        self.line_pick_anchors.push(pos);
    }

    /// Bake the matrix `m` into every channel, in place — the single owner of
    /// the item-transform application. silx applies the `DataItem3D` transform
    /// stack (`items/core.py:288-315`) in the scene graph at render time; this
    /// port bakes the composed matrix when an item appends its geometry, so
    /// rendering and the CPU pick traversal ([`crate::SceneWidget::pick`])
    /// read the same transformed positions by construction.
    ///
    /// Per channel: line / triangle / point / pick-anchor / textured-mesh
    /// positions map by `m` (point sprite sizes stay in pixels, as silx);
    /// lit-mesh normals map by the inverse-transpose, renormalized (left
    /// untouched when `m` is singular — the shader then shades the raw
    /// normal). Image layers stay axis-aligned layers when `m` is a
    /// translation + positive per-axis scale (the silx `ScalarFieldView`
    /// `_dataScale`/`_dataTranslate` case) and otherwise convert to the
    /// equivalent textured-mesh quad — rendered identically, but no longer
    /// reachable by the image row/column pick.
    pub fn apply_transform(&mut self, m: &Mat4) {
        let map = |pos: &mut [f32; 3]| {
            *pos = m.transform_point(Vec3::from_array(*pos), false).to_array();
        };
        for v in &mut self.lines {
            map(&mut v.pos);
        }
        for v in &mut self.triangles {
            map(&mut v.pos);
        }
        for p in &mut self.points {
            map(&mut p.pos);
        }
        for a in &mut self.line_pick_anchors {
            map(a);
        }
        let inverse = m.inverse();
        for v in &mut self.meshes {
            map(&mut v.pos);
            if let Some(inv) = &inverse {
                // Inverse-transpose: n'ᵢ = Σⱼ inv[j][i]·nⱼ (translation drops).
                let n = v.normal;
                let nt = Vec3::new(
                    inv.rows[0][0] * n[0] + inv.rows[1][0] * n[1] + inv.rows[2][0] * n[2],
                    inv.rows[0][1] * n[0] + inv.rows[1][1] * n[1] + inv.rows[2][1] * n[2],
                    inv.rows[0][2] * n[0] + inv.rows[1][2] * n[1] + inv.rows[2][2] * n[2],
                );
                v.normal = nt.normalized().to_array();
            }
        }
        for mesh in &mut self.textured_meshes {
            for v in &mut mesh.vertices {
                map(v);
            }
        }
        // Image layers: only a translation + positive per-axis scale keeps the
        // axis-aligned origin/scale representation (and with it the image
        // row/column pick); anything else becomes a textured quad.
        let diag = axis_aligned_positive_scale(m);
        if let Some((sx, sy, _)) = diag {
            for layer in &mut self.images {
                let origin = m
                    .transform_point(Vec3::from_array(layer.origin), false)
                    .to_array();
                layer.origin = origin;
                layer.scale = [layer.scale[0] * sx, layer.scale[1] * sy];
            }
        } else {
            for layer in std::mem::take(&mut self.images) {
                self.textured_meshes
                    .push(image_layer_to_textured_mesh(&layer, m));
            }
        }
    }

    /// World-space pick-only anchors (see [`Self::add_line_pick_anchor`]).
    pub fn line_pick_anchors(&self) -> &[[f32; 3]] {
        &self.line_pick_anchors
    }

    /// Append a line segment `a→b` in one solid [`Color32`].
    pub fn add_line(&mut self, a: [f32; 3], b: [f32; 3], color: Color32) {
        let rgba = egui::Rgba::from(color).to_array();
        self.add_line_rgba(a, b, rgba);
    }

    /// Append a line segment `a→b` with explicit linear-premultiplied RGBA.
    pub fn add_line_rgba(&mut self, a: [f32; 3], b: [f32; 3], rgba: [f32; 4]) {
        self.lines.push(Scene3dVertex {
            pos: a,
            color: rgba,
        });
        self.lines.push(Scene3dVertex {
            pos: b,
            color: rgba,
        });
    }

    /// Append a line segment `a→b` whose endpoints carry their own linear-
    /// premultiplied RGBA, so the segment gradients between them. The analogue of
    /// silx colouring line vertices through a colormap (`ColormapMesh3D` with
    /// `mode='lines'`), used by [`Scatter2D`](crate::render::scene3d_items::Scatter2D)'s
    /// LINES visualization.
    pub fn add_line_gradient(
        &mut self,
        a: [f32; 3],
        b: [f32; 3],
        rgba_a: [f32; 4],
        rgba_b: [f32; 4],
    ) {
        self.lines.push(Scene3dVertex {
            pos: a,
            color: rgba_a,
        });
        self.lines.push(Scene3dVertex {
            pos: b,
            color: rgba_b,
        });
    }

    /// Append a triangle `a, b, c` in one solid [`Color32`].
    pub fn add_triangle(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], color: Color32) {
        let rgba = egui::Rgba::from(color).to_array();
        self.add_triangle_rgba(a, b, c, rgba);
    }

    /// Append a triangle `a, b, c` with explicit linear-premultiplied RGBA.
    pub fn add_triangle_rgba(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], rgba: [f32; 4]) {
        for pos in [a, b, c] {
            self.triangles.push(Scene3dVertex { pos, color: rgba });
        }
    }

    /// Append one scatter point at `pos`, drawn as a `marker` sprite `size`
    /// physical pixels across in solid [`Color32`].
    pub fn add_point(&mut self, pos: [f32; 3], color: Color32, size: f32, marker: PointMarker) {
        let rgba = egui::Rgba::from(color).to_array();
        self.add_point_rgba(pos, rgba, size, marker);
    }

    /// Append one scatter point with explicit linear-premultiplied RGBA.
    pub fn add_point_rgba(
        &mut self,
        pos: [f32; 3],
        rgba: [f32; 4],
        size: f32,
        marker: PointMarker,
    ) {
        self.points.push(Scene3dPoint {
            pos,
            color: rgba,
            size,
            marker: marker.id(),
        });
    }

    /// Append one lit-mesh triangle with explicit per-vertex positions, linear-
    /// premultiplied RGBA colors, and world-space normals.
    pub fn add_mesh_triangle_rgba(
        &mut self,
        positions: [[f32; 3]; 3],
        rgba: [[f32; 4]; 3],
        normals: [[f32; 3]; 3],
    ) {
        for i in 0..3 {
            self.meshes.push(Scene3dMeshVertex {
                pos: positions[i],
                color: rgba[i],
                normal: normals[i],
            });
        }
    }

    /// Append one lit-mesh triangle in a single solid [`Color32`] with explicit
    /// per-vertex normals.
    pub fn add_mesh_triangle(
        &mut self,
        positions: [[f32; 3]; 3],
        color: Color32,
        normals: [[f32; 3]; 3],
    ) {
        let rgba = egui::Rgba::from(color).to_array();
        self.add_mesh_triangle_rgba(positions, [rgba; 3], normals);
    }

    /// Append one lit-mesh triangle `a, b, c` in a solid [`Color32`], using the
    /// geometric (flat) face normal `(b−a)×(c−a)` for all three vertices — the
    /// fallback when a mesh provides no per-vertex normals.
    pub fn add_mesh_triangle_flat(
        &mut self,
        a: [f32; 3],
        b: [f32; 3],
        c: [f32; 3],
        color: Color32,
    ) {
        let n = flat_normal(a, b, c);
        self.add_mesh_triangle([a, b, c], color, [n; 3]);
    }

    /// Append the bounding-box wireframe + RGB axes for `bounds`, the scene's
    /// spatial chrome. Port of silx `primitives.BoxWithAxes`: three coloured axis
    /// lines from the min corner (X red, Y green, Z blue, each spanning the box
    /// extent) plus the nine remaining box edges in `box_color` (the three edges
    /// that coincide with the axes are drawn as the axes, not repeated).
    pub fn add_bounding_box_with_axes(&mut self, bounds: (Vec3, Vec3), box_color: Color32) {
        let (min, max) = bounds;
        let size = max - min;
        // Unit-cube coordinate → world (silx scales the unit `_vertices` by size
        // and the GroupBBox transform translates them to the min corner).
        let v = |ux: f32, uy: f32, uz: f32| {
            [
                min.x + size.x * ux,
                min.y + size.y * uy,
                min.z + size.z * uz,
            ]
        };
        // The 13 vertices of silx `BoxWithAxes._vertices` (axes origin+tips, then
        // the box corners not already covered by an axis tip).
        let verts = [
            v(0.0, 0.0, 0.0), // 0 axes origin
            v(1.0, 0.0, 0.0), // 1 X tip
            v(0.0, 0.0, 0.0), // 2 axes origin
            v(0.0, 1.0, 0.0), // 3 Y tip
            v(0.0, 0.0, 0.0), // 4 axes origin
            v(0.0, 0.0, 1.0), // 5 Z tip
            v(1.0, 0.0, 0.0), // 6 box corners, z=0
            v(1.0, 1.0, 0.0), // 7
            v(0.0, 1.0, 0.0), // 8
            v(0.0, 0.0, 1.0), // 9 box corners, z=1
            v(1.0, 0.0, 1.0), // 10
            v(1.0, 1.0, 1.0), // 11
            v(0.0, 1.0, 1.0), // 12
        ];

        // RGB axes (X red, Y green, Z blue).
        self.add_line(verts[0], verts[1], Color32::from_rgb(255, 0, 0));
        self.add_line(verts[2], verts[3], Color32::from_rgb(0, 255, 0));
        self.add_line(verts[4], verts[5], Color32::from_rgb(0, 0, 255));

        // The remaining nine box edges (silx `_lineIndices` minus the three axes).
        const BOX_EDGES: [(usize, usize); 9] = [
            (6, 7),
            (7, 8),
            (6, 10),
            (7, 11),
            (8, 12),
            (9, 10),
            (10, 11),
            (11, 12),
            (12, 9),
        ];
        for &(a, b) in &BOX_EDGES {
            self.add_line(verts[a], verts[b], box_color);
        }
    }
}

/// The shared pipelines + layouts for 3D rendering. Built once in
/// [`Scene3dResources::new`].
struct Scene3dPipeline {
    /// egui's surface format; the offscreen color target uses it too so colors
    /// round-trip through the blit without an extra color-space conversion.
    target_format: wgpu::TextureFormat,
    /// `group(0)` layout for the MVP uniform (vertex stage).
    scene_bgl: wgpu::BindGroupLayout,
    /// Depth-tested `LineList` pipeline.
    line_pipeline: wgpu::RenderPipeline,
    /// Depth-tested `TriangleList` pipeline (no face culling).
    tri_pipeline: wgpu::RenderPipeline,
    /// `group(0)` layout for the point-sprite uniform (MVP + viewport, vertex stage).
    point_bgl: wgpu::BindGroupLayout,
    /// Depth-tested, alpha-blended billboarded point-sprite pipeline.
    point_pipeline: wgpu::RenderPipeline,
    /// `group(0)` layout for the mesh uniform (MVP + normal matrix, vertex stage).
    mesh_bgl: wgpu::BindGroupLayout,
    /// Depth-tested, headlight-shaded `TriangleList` mesh pipeline (no culling).
    mesh_pipeline: wgpu::RenderPipeline,
    /// `group(1)` layout for an image layer (sampled texture + sampler, fragment).
    image_tex_bgl: wgpu::BindGroupLayout,
    /// Depth-tested, alpha-blended textured-quad pipeline (group 0 = scene MVP).
    image_pipeline: wgpu::RenderPipeline,
    /// Nearest-filtering, clamp-to-edge sampler for crisp image pixels.
    image_sampler_nearest: wgpu::Sampler,
    /// Linear-filtering, clamp-to-edge sampler for smooth images.
    image_sampler_linear: wgpu::Sampler,
    /// `group(0)` layout for the blit (sampled texture + sampler, fragment stage).
    blit_bgl: wgpu::BindGroupLayout,
    /// Depth-less full-screen blit pipeline (offscreen color → egui pass).
    blit_pipeline: wgpu::RenderPipeline,
    /// Linear-filtering, clamp-to-edge sampler for the blit.
    sampler: wgpu::Sampler,
}

impl Scene3dPipeline {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let scene_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsplot scene3d"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d.wgsl").into()),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsplot scene3d blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d_blit.wgsl").into()),
        });
        let point_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsplot scene3d points"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d_points.wgsl").into()),
        });
        let mesh_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsplot scene3d mesh"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d_mesh.wgsl").into()),
        });
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rsplot scene3d image"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scene3d_image.wgsl").into()),
        });

        let scene_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsplot scene3d scene bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // The fragment stage reads the fog uniform (silx applies fog
                // per-fragment, viewport.py RenderContext scene_post).
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(std::mem::size_of::<Scene3dParams>() as u64),
                },
                count: None,
            }],
        });

        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsplot scene3d scene layout"),
            bind_group_layouts: &[Some(&scene_bgl)],
            immediate_size: 0,
        });

        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Scene3dVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &SCENE3D_VERTEX_ATTRS,
        }];

        // Lines and triangles differ only in primitive topology; everything else
        // (shader, vertex layout, depth state, target) is shared.
        let make_scene_pipeline = |topology: wgpu::PrimitiveTopology, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&scene_layout),
                vertex: wgpu::VertexState {
                    module: &scene_shader,
                    entry_point: Some("vs_main"),
                    buffers: &vertex_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &scene_shader,
                    entry_point: Some("fs_main"),
                    // silx enables GL_BLEND (SRC_ALPHA/ONE_MINUS_SRC_ALPHA) for the
                    // whole scene (`viewport.py:356-357`); our vertex colors are
                    // linear-premultiplied, so PREMULTIPLIED_ALPHA_BLENDING gives
                    // the same result. Opaque geometry (α=1) still writes fully;
                    // translucent content (e.g. axis tick lines at 0.6 α,
                    // `axes.py:114`) composites. Depth write stays on, as in silx.
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    // No culling: wireframes/axes and double-sided meshes must
                    // show both faces (silx does not cull these).
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    // silx sets `glDepthFunc(GL_LEQUAL)` once for the whole 3D scene
                    // (scene/viewport.py:360); LessEqual lets a fragment at exactly
                    // the stored depth pass, matching silx for coplanar redraws.
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let line_pipeline =
            make_scene_pipeline(wgpu::PrimitiveTopology::LineList, "rsplot scene3d lines");
        let tri_pipeline = make_scene_pipeline(
            wgpu::PrimitiveTopology::TriangleList,
            "rsplot scene3d triangles",
        );

        // Point sprites: their own uniform (MVP + fog + viewport) and an
        // instanced billboard pipeline with premultiplied-alpha blending so the
        // antialiased marker edges composite over the opaque scene behind them.
        let point_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsplot scene3d point bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // Fragment stage reads the fog uniform.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(
                        std::mem::size_of::<Scene3dPointParams>() as u64
                    ),
                },
                count: None,
            }],
        });
        let point_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsplot scene3d point layout"),
            bind_group_layouts: &[Some(&point_bgl)],
            immediate_size: 0,
        });
        let point_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rsplot scene3d points"),
            layout: Some(&point_layout),
            vertex: wgpu::VertexState {
                module: &point_shader,
                entry_point: Some("vs_main"),
                // No vertex buffer: corners come from vertex_index. One instance
                // per point carries pos/color/size/marker.
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Scene3dPoint>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &SCENE3D_POINT_ATTRS,
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &point_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                // silx sets `glDepthFunc(GL_LEQUAL)` once for the whole 3D scene
                // (scene/viewport.py:360); LessEqual lets a fragment at exactly
                // the stored depth pass, matching silx for coplanar redraws.
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Shaded meshes: their own uniform (MVP + normal matrix + fog +
        // shininess) and a depth-tested, premultiplied-alpha-blended, double-sided
        // triangle pipeline with headlight lighting in the fragment shader.
        let mesh_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsplot scene3d mesh bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                // Fragment stage reads the fog + shininess uniforms.
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(
                        std::mem::size_of::<Scene3dMeshParams>() as u64
                    ),
                },
                count: None,
            }],
        });
        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsplot scene3d mesh layout"),
            bind_group_layouts: &[Some(&mesh_bgl)],
            immediate_size: 0,
        });
        let mesh_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rsplot scene3d mesh"),
            layout: Some(&mesh_layout),
            vertex: wgpu::VertexState {
                module: &mesh_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Scene3dMeshVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &SCENE3D_MESH_ATTRS,
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &mesh_shader,
                entry_point: Some("fs_main"),
                // silx blends the whole scene (`viewport.py:356-357`); the mesh
                // shader outputs linear-premultiplied RGBA, so
                // PREMULTIPLIED_ALPHA_BLENDING matches silx's straight SRC_ALPHA
                // over premultiplied color. A translucent iso-surface / Mesh3D now
                // composites (silx `volume.py:659-663` sorts iso-surfaces by
                // -level for this, which `ScalarField3D::append_raw` already does);
                // opaque meshes (α=1) write fully. Depth write stays on, as in silx.
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // No culling: silx lights one-sided but does not cull, so a face
                // seen from behind shows at ambient (its normal faces away).
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                // silx sets `glDepthFunc(GL_LEQUAL)` once for the whole 3D scene
                // (scene/viewport.py:360); LessEqual lets a fragment at exactly
                // the stored depth pass, matching silx for coplanar redraws.
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Textured image quads: group 0 reuses the scene MVP uniform; group 1 is
        // the per-image texture + sampler. Depth-tested, premultiplied-alpha
        // blended (opaque images write fully; RGBA images composite).
        let image_tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsplot scene3d image tex bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let image_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsplot scene3d image layout"),
            bind_group_layouts: &[Some(&scene_bgl), Some(&image_tex_bgl)],
            immediate_size: 0,
        });
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rsplot scene3d image"),
            layout: Some(&image_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Scene3dImageVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &SCENE3D_IMAGE_ATTRS,
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                // silx sets `glDepthFunc(GL_LEQUAL)` once for the whole 3D scene
                // (scene/viewport.py:360); LessEqual lets a fragment at exactly
                // the stored depth pass, matching silx for coplanar redraws.
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let image_sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rsplot scene3d image sampler (nearest)"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let image_sampler_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rsplot scene3d image sampler (linear)"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let blit_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rsplot scene3d blit bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let blit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rsplot scene3d blit layout"),
            bind_group_layouts: &[Some(&blit_bgl)],
            immediate_size: 0,
        });

        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rsplot scene3d blit pipeline"),
            layout: Some(&blit_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                // blend: None → replace; the scene (opaque background) occludes
                // whatever egui drew behind the widget rect.
                targets: &[Some(target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            // egui's pass has no depth attachment, so the blit must not test depth.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rsplot scene3d blit sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            target_format,
            scene_bgl,
            line_pipeline,
            tri_pipeline,
            point_bgl,
            point_pipeline,
            mesh_bgl,
            mesh_pipeline,
            image_tex_bgl,
            image_pipeline,
            image_sampler_nearest,
            image_sampler_linear,
            blit_bgl,
            blit_pipeline,
            sampler,
        }
    }
}

/// One uploaded textured primitive: its vertex buffer (`vertex_count` verts —
/// 6 for an image quad, `3·triangles` for a textured mesh) and the group(1) bind
/// group over its texture + the chosen sampler. Rebuilt on each
/// [`Scene3dGpu::upload`].
struct Scene3dImageGpu {
    vbuf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertex_count: u32,
}

/// Per-scene GPU data: vertex buffers, the MVP uniform, and the offscreen
/// color+depth render target (recreated on size change).
struct Scene3dGpu {
    /// MVP uniform, written each frame in [`Scene3dResources::prepare_scene`].
    params_buf: wgpu::Buffer,
    /// `group(0)` bind group over `params_buf` for the scene pipelines.
    scene_bind_group: wgpu::BindGroup,
    /// Line vertices; `None` while empty (skip the draw).
    line_vbuf: Option<wgpu::Buffer>,
    line_count: u32,
    /// Triangle vertices; `None` while empty (skip the draw).
    tri_vbuf: Option<wgpu::Buffer>,
    tri_count: u32,
    /// Point-sprite uniform (MVP + viewport), written each frame.
    point_params_buf: wgpu::Buffer,
    /// `group(0)` bind group over `point_params_buf` for the point pipeline.
    point_bind_group: wgpu::BindGroup,
    /// Per-instance scatter points; `None` while empty (skip the draw).
    point_vbuf: Option<wgpu::Buffer>,
    point_count: u32,
    /// Mesh uniform (MVP + normal matrix), written each frame.
    mesh_params_buf: wgpu::Buffer,
    /// `group(0)` bind group over `mesh_params_buf` for the mesh pipeline.
    mesh_bind_group: wgpu::BindGroup,
    /// Lit-mesh vertices; `None` while empty (skip the draw).
    mesh_vbuf: Option<wgpu::Buffer>,
    mesh_count: u32,
    /// Uploaded image layers (texture + quad), drawn in order after the meshes.
    images: Vec<Scene3dImageGpu>,
    /// Pixel size of the current offscreen target (`[0, 0]` until first sized).
    size: [u32; 2],
    /// Offscreen color view (target format); the blit samples this.
    color_view: Option<wgpu::TextureView>,
    /// Offscreen depth view (`Depth32Float`), for depth testing.
    depth_view: Option<wgpu::TextureView>,
    /// `group(0)` bind group over the color view + sampler for the blit pipeline.
    blit_bind_group: Option<wgpu::BindGroup>,
}

impl Scene3dGpu {
    fn new(device: &wgpu::Device, pipeline: &Scene3dPipeline) -> Self {
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rsplot scene3d params"),
            size: std::mem::size_of::<Scene3dParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scene_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rsplot scene3d scene bind group"),
            layout: &pipeline.scene_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            }],
        });
        let point_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rsplot scene3d point params"),
            size: std::mem::size_of::<Scene3dPointParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let point_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rsplot scene3d point bind group"),
            layout: &pipeline.point_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: point_params_buf.as_entire_binding(),
            }],
        });
        let mesh_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rsplot scene3d mesh params"),
            size: std::mem::size_of::<Scene3dMeshParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mesh_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rsplot scene3d mesh bind group"),
            layout: &pipeline.mesh_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: mesh_params_buf.as_entire_binding(),
            }],
        });
        Self {
            params_buf,
            scene_bind_group,
            line_vbuf: None,
            line_count: 0,
            tri_vbuf: None,
            tri_count: 0,
            point_params_buf,
            point_bind_group,
            point_vbuf: None,
            point_count: 0,
            mesh_params_buf,
            mesh_bind_group,
            mesh_vbuf: None,
            mesh_count: 0,
            images: Vec::new(),
            size: [0, 0],
            color_view: None,
            depth_view: None,
            blit_bind_group: None,
        }
    }

    /// Replace the line + triangle + point + mesh buffers and the image layers
    /// from `geometry`.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &Scene3dPipeline,
        geometry: &Scene3dGeometry,
    ) {
        self.line_vbuf = make_vertex_buffer(device, queue, &geometry.lines, "rsplot scene3d lines");
        self.line_count = geometry.lines.len() as u32;
        self.tri_vbuf =
            make_vertex_buffer(device, queue, &geometry.triangles, "rsplot scene3d tris");
        self.tri_count = geometry.triangles.len() as u32;
        self.point_vbuf =
            make_vertex_buffer(device, queue, &geometry.points, "rsplot scene3d points");
        self.point_count = geometry.points.len() as u32;
        self.mesh_vbuf =
            make_vertex_buffer(device, queue, &geometry.meshes, "rsplot scene3d meshes");
        self.mesh_count = geometry.meshes.len() as u32;
        // Image quads and textured meshes both upload to a `Scene3dImageGpu`
        // (texture + vertex buffer) and draw through the one textured pipeline;
        // collect them into a single list (quads first, then meshes).
        self.images = geometry
            .images
            .iter()
            .filter_map(|layer| build_image_gpu(device, queue, pipeline, layer))
            .chain(
                geometry
                    .textured_meshes
                    .iter()
                    .filter_map(|mesh| build_textured_mesh_gpu(device, queue, pipeline, mesh)),
            )
            .collect();
    }

    /// Ensure the offscreen color+depth target matches `size` (in physical
    /// pixels), recreating the textures and blit bind group on a size change.
    fn ensure_offscreen(
        &mut self,
        device: &wgpu::Device,
        pipeline: &Scene3dPipeline,
        size: [u32; 2],
    ) {
        let size = [size[0].max(1), size[1].max(1)];
        if self.size == size && self.color_view.is_some() {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rsplot scene3d color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipeline.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rsplot scene3d depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rsplot scene3d blit bind group"),
            layout: &pipeline.blit_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                },
            ],
        });
        self.size = size;
        self.color_view = Some(color_view);
        self.depth_view = Some(depth_view);
        self.blit_bind_group = Some(blit_bind_group);
    }

    /// Encode the offscreen depth-tested pass (clear → triangles → lines) into
    /// `encoder`, targeting `color_view` + `depth_view`. The on-screen path
    /// (`prepare`) passes the persistent blit target; [`Scene3dResources::snapshot_scene`]
    /// passes a transient copyable target — the draw sequence is identical, so
    /// the snapshot is pixel-for-pixel the rendered scene.
    fn encode_offscreen(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &Scene3dPipeline,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        background: [f32; 4],
    ) {
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rsplot scene3d offscreen pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: background[0] as f64,
                        g: background[1] as f64,
                        b: background[2] as f64,
                        a: background[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_bind_group(0, &self.scene_bind_group, &[]);
        if let (Some(buf), true) = (&self.tri_vbuf, self.tri_count > 0) {
            rp.set_pipeline(&pipeline.tri_pipeline);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..self.tri_count, 0..1);
        }
        if let (Some(buf), true) = (&self.line_vbuf, self.line_count > 0) {
            rp.set_pipeline(&pipeline.line_pipeline);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..self.line_count, 0..1);
        }
        // Shaded meshes (own bind group: MVP + normal matrix). Opaque; depth test
        // resolves occlusion against the flat triangles and lines above.
        if let (Some(buf), true) = (&self.mesh_vbuf, self.mesh_count > 0) {
            rp.set_pipeline(&pipeline.mesh_pipeline);
            rp.set_bind_group(0, &self.mesh_bind_group, &[]);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..self.mesh_count, 0..1);
        }
        // Textured primitives — image quads and textured meshes (cut planes) —
        // share one pipeline (group 0 = scene MVP, group 1 = per-primitive
        // texture). Premultiplied-alpha blended and depth-tested; drawn after the
        // opaque geometry, before the point overlays.
        if !self.images.is_empty() {
            rp.set_pipeline(&pipeline.image_pipeline);
            rp.set_bind_group(0, &self.scene_bind_group, &[]);
            for image in &self.images {
                rp.set_bind_group(1, &image.bind_group, &[]);
                rp.set_vertex_buffer(0, image.vbuf.slice(..));
                rp.draw(0..image.vertex_count, 0..1);
            }
        }
        // Point sprites last: alpha-blended billboards over the opaque geometry.
        // Six vertices (two triangles) per instance, one instance per point.
        if let (Some(buf), true) = (&self.point_vbuf, self.point_count > 0) {
            rp.set_pipeline(&pipeline.point_pipeline);
            rp.set_bind_group(0, &self.point_bind_group, &[]);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..6, 0..self.point_count);
        }
    }

    /// Encode the orientation indicator (`self` is the companion overview
    /// scene: disc point sprite + RGB axis lines) into the
    /// [`OVERVIEW_SIZE_PX`]² viewport at `origin` (top-left corner, framebuffer
    /// pixels) of an already-rendered target — silx `_OverviewViewport`
    /// (`Plot3DWidget.py:51-93`), which draws its scene into a second viewport
    /// over the main one.
    ///
    /// Two sub-passes reproduce silx's depth handling: the disc backdrop is a
    /// `GroupNoDepth(mask=True, notest=True)` (`Plot3DWidget.py:66-73`), i.e.
    /// it neither tests nor writes depth, so the axes always draw over it.
    /// Our point pipeline has depth fixed on, so instead the disc is drawn in
    /// its own pass and the depth buffer is re-cleared before the axes pass —
    /// same result: axes on top of the disc, disc on top of the main scene.
    fn encode_overview(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &Scene3dPipeline,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        origin: [u32; 2],
    ) {
        // silx `_OverviewViewport` has `background=None`: the main image is
        // kept (color LoadOp::Load), only depth is cleared.
        fn begin_overview_pass<'e>(
            encoder: &'e mut wgpu::CommandEncoder,
            label: &'static str,
            color_view: &wgpu::TextureView,
            depth_view: &wgpu::TextureView,
            origin: [u32; 2],
        ) -> wgpu::RenderPass<'e> {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_viewport(
                origin[0] as f32,
                origin[1] as f32,
                OVERVIEW_SIZE_PX as f32,
                OVERVIEW_SIZE_PX as f32,
                0.0,
                1.0,
            );
            rp.set_scissor_rect(origin[0], origin[1], OVERVIEW_SIZE_PX, OVERVIEW_SIZE_PX);
            rp
        }
        // Pass 1: the half-transparent disc backdrop (point channel).
        if let (Some(buf), true) = (&self.point_vbuf, self.point_count > 0) {
            let mut rp = begin_overview_pass(
                encoder,
                "rsplot scene3d overview disc pass",
                color_view,
                depth_view,
                origin,
            );
            rp.set_pipeline(&pipeline.point_pipeline);
            rp.set_bind_group(0, &self.point_bind_group, &[]);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..6, 0..self.point_count);
        }
        // Pass 2: the RGB axes (line channel), depth re-cleared so they draw
        // fully over the disc.
        if let (Some(buf), true) = (&self.line_vbuf, self.line_count > 0) {
            let mut rp = begin_overview_pass(
                encoder,
                "rsplot scene3d overview axes pass",
                color_view,
                depth_view,
                origin,
            );
            rp.set_pipeline(&pipeline.line_pipeline);
            rp.set_bind_group(0, &self.scene_bind_group, &[]);
            rp.set_vertex_buffer(0, buf.slice(..));
            rp.draw(0..self.line_count, 0..1);
        }
    }
}

/// The geometric (flat) face normal of triangle `a, b, c`: the normalized cross
/// product `(b−a) × (c−a)`. A degenerate triangle yields a zero vector (the
/// mesh shader's `normalize` then leaves the face at ambient only).
///
/// `pub(crate)` so the item layer ([`crate::render::scene3d_items`]) can compute
/// the same fallback normal when a mesh provides none — one owner of the rule.
pub(crate) fn flat_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let va = Vec3::new(a[0], a[1], a[2]);
    let vb = Vec3::new(b[0], b[1], b[2]);
    let vc = Vec3::new(c[0], c[1], c[2]);
    (vb - va).cross(vc - va).normalized().to_array()
}

/// Create a `VERTEX | COPY_DST` buffer holding `verts`, or `None` when empty.
fn make_vertex_buffer<T: bytemuck::Pod>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    verts: &[T],
    label: &str,
) -> Option<wgpu::Buffer> {
    if verts.is_empty() {
        return None;
    }
    let bytes = bytemuck::cast_slice(verts);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytes);
    Some(buffer)
}

/// Upload a `width × height` premultiplied-linear RGBA8 raster to an
/// `Rgba8Unorm` texture and build the group(1) bind group (texture + the
/// nearest/linear sampler). The single owner of the textured-primitive texture
/// path, shared by [`build_image_gpu`] and [`build_textured_mesh_gpu`]. Returns
/// `None` for zero dimensions or a pixel buffer of the wrong length.
fn build_image_texture_bind_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &Scene3dPipeline,
    pixels: &[u8],
    width: u32,
    height: u32,
    interpolation: ImageInterpolation,
) -> Option<wgpu::BindGroup> {
    if width == 0 || height == 0 || pixels.len() != (width as usize * height as usize * 4) {
        return None;
    }
    let extent = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rsplot scene3d image texture"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Premultiplied-linear RGBA stored verbatim (no sRGB decode), so the
        // sampled colour matches the geometry path's linear convention.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        extent,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = match interpolation {
        ImageInterpolation::Nearest => &pipeline.image_sampler_nearest,
        ImageInterpolation::Linear => &pipeline.image_sampler_linear,
    };
    Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rsplot scene3d image bind group"),
        layout: &pipeline.image_tex_bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    }))
}

/// If `m` is a pure translation + positive per-axis scale, its `(sx, sy, sz)`
/// diagonal; otherwise `None`. Used by [`Scene3dGeometry::apply_transform`] to
/// decide whether an image layer can stay axis-aligned.
fn axis_aligned_positive_scale(m: &Mat4) -> Option<(f32, f32, f32)> {
    let r = &m.rows;
    let off_diagonal_zero = r[0][1] == 0.0
        && r[0][2] == 0.0
        && r[1][0] == 0.0
        && r[1][2] == 0.0
        && r[2][0] == 0.0
        && r[2][1] == 0.0
        && r[3][0] == 0.0
        && r[3][1] == 0.0
        && r[3][2] == 0.0
        && r[3][3] == 1.0;
    (off_diagonal_zero && r[0][0] > 0.0 && r[1][1] > 0.0 && r[2][2] > 0.0)
        .then_some((r[0][0], r[1][1], r[2][2]))
}

/// Convert an axis-aligned image layer into the equivalent textured-mesh quad
/// with its corners mapped through `m` — the same two triangles and corner UVs
/// [`build_image_gpu`] would emit, so the rendered pixels are unchanged.
fn image_layer_to_textured_mesh(layer: &Scene3dImageLayer, m: &Mat4) -> Scene3dTexturedMesh {
    let [ox, oy, oz] = layer.origin;
    let (sx, sy) = (layer.scale[0], layer.scale[1]);
    let (x1, y1) = (ox + layer.width as f32 * sx, oy + layer.height as f32 * sy);
    let corner = |x: f32, y: f32| m.transform_point(Vec3::new(x, y, oz), false).to_array();
    Scene3dTexturedMesh {
        pixels: layer.pixels.clone(),
        width: layer.width,
        height: layer.height,
        vertices: vec![
            corner(ox, oy),
            corner(x1, oy),
            corner(x1, y1),
            corner(ox, oy),
            corner(x1, y1),
            corner(ox, y1),
        ],
        uvs: vec![
            [0.0, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.0, 0.0],
            [1.0, 1.0],
            [0.0, 1.0],
        ],
        interpolation: layer.interpolation,
    }
}

/// Build the per-image GPU state for one [`Scene3dImageLayer`]: its texture bind
/// group plus the six-vertex quad (two triangles) at the layer's world rect with
/// corner UVs. Returns `None` for a degenerate layer (zero dimensions or a pixel
/// buffer of the wrong length).
fn build_image_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &Scene3dPipeline,
    layer: &Scene3dImageLayer,
) -> Option<Scene3dImageGpu> {
    let (w, h) = (layer.width, layer.height);
    let bind_group = build_image_texture_bind_group(
        device,
        queue,
        pipeline,
        &layer.pixels,
        w,
        h,
        layer.interpolation,
    )?;

    // Quad corners in the z = origin.z plane: (0,0) → (w·sx, h·sy). UV (0,0) at
    // the origin corner, (1,1) at the far corner (row 0 first → v increases with
    // y, no flip).
    let [ox, oy, oz] = layer.origin;
    let (sx, sy) = (layer.scale[0], layer.scale[1]);
    let (x1, y1) = (ox + w as f32 * sx, oy + h as f32 * sy);
    let v = |x: f32, y: f32, u: f32, vv: f32| Scene3dImageVertex {
        pos: [x, y, oz],
        uv: [u, vv],
    };
    let verts = [
        v(ox, oy, 0.0, 0.0),
        v(x1, oy, 1.0, 0.0),
        v(x1, y1, 1.0, 1.0),
        v(ox, oy, 0.0, 0.0),
        v(x1, y1, 1.0, 1.0),
        v(ox, y1, 0.0, 1.0),
    ];
    let vbuf = make_vertex_buffer(device, queue, &verts, "rsplot scene3d image quad")?;
    Some(Scene3dImageGpu {
        vbuf,
        bind_group,
        vertex_count: 6,
    })
}

/// Build the per-mesh GPU state for one [`Scene3dTexturedMesh`]: its texture bind
/// group plus the world-space triangle-list vertex buffer (UVs paired in).
/// Returns `None` for a degenerate mesh (empty, vertex/uv length mismatch, a
/// vertex count not a multiple of three, or a bad texture).
fn build_textured_mesh_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &Scene3dPipeline,
    mesh: &Scene3dTexturedMesh,
) -> Option<Scene3dImageGpu> {
    if mesh.vertices.is_empty()
        || mesh.vertices.len() != mesh.uvs.len()
        || !mesh.vertices.len().is_multiple_of(3)
    {
        return None;
    }
    let bind_group = build_image_texture_bind_group(
        device,
        queue,
        pipeline,
        &mesh.pixels,
        mesh.width,
        mesh.height,
        mesh.interpolation,
    )?;
    let verts: Vec<Scene3dImageVertex> = mesh
        .vertices
        .iter()
        .zip(&mesh.uvs)
        .map(|(&pos, &uv)| Scene3dImageVertex { pos, uv })
        .collect();
    let vbuf = make_vertex_buffer(device, queue, &verts, "rsplot scene3d textured mesh")?;
    Some(Scene3dImageGpu {
        vbuf,
        bind_group,
        vertex_count: verts.len() as u32,
    })
}

/// Persistent 3D GPU resources, stored in `egui_wgpu`'s `callback_resources`.
/// Per-scene state is keyed by [`Scene3dId`].
pub struct Scene3dResources {
    pipeline: Scene3dPipeline,
    scenes: HashMap<Scene3dId, Scene3dGpu>,
}

impl Scene3dResources {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        Self {
            pipeline: Scene3dPipeline::new(device, target_format),
            scenes: HashMap::new(),
        }
    }

    /// Size the offscreen target, write the MVP uniform, and encode the
    /// depth-tested offscreen pass for `frame.id` (creating per-scene state if
    /// needed), followed by the orientation-indicator pass when requested.
    fn prepare_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &Scene3dFrame,
    ) {
        let Self { pipeline, scenes } = self;
        scenes
            .entry(frame.id)
            .or_insert_with(|| Scene3dGpu::new(device, pipeline))
            .ensure_offscreen(device, pipeline, frame.size_px);
        // Re-borrow shared so the overview scene can be read alongside.
        let scene = &scenes[&frame.id];
        frame.write_uniforms(queue, scene);
        if let (Some(color_view), Some(depth_view)) =
            (scene.color_view.as_ref(), scene.depth_view.as_ref())
        {
            scene.encode_offscreen(encoder, pipeline, color_view, depth_view, frame.background);
            if let Some((ov, ov_scene, origin)) = overview_pass(scenes, frame) {
                ov.write_uniforms(queue, ov_scene);
                ov_scene.encode_overview(encoder, pipeline, color_view, depth_view, origin);
            }
        }
    }

    /// Render scene `frame.id` into a transient copyable target at `frame.size_px`
    /// and read it back as tightly packed RGBA8 (`width * height * 4`). Returns
    /// `None` if the scene has no uploaded geometry yet or the GPU readback fails.
    ///
    /// The per-scene uniforms are (re)written for `frame`'s camera and size, then
    /// the same [`Scene3dGpu::encode_offscreen`] draw runs into a fresh
    /// `RENDER_ATTACHMENT | COPY_SRC` color target (the persistent blit target is
    /// `TEXTURE_BINDING`-only, so it cannot be copied). Synchronous: it submits and
    /// blocks on the readback, independent of the egui frame loop.
    fn snapshot_scene(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &Scene3dFrame,
    ) -> Option<Vec<u8>> {
        use crate::render::save::{padded_bytes_per_row, rows_to_rgba8};

        let Self { pipeline, scenes } = self;
        let scene = scenes.get(&frame.id)?;
        let (w, h) = (frame.size_px[0].max(1), frame.size_px[1].max(1));

        // Stamp this snapshot's uniforms (same owner as `prepare_scene`). The
        // next on-screen frame rewrites these, so clobbering them is harmless.
        frame.write_uniforms(queue, scene);

        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rsplot scene3d snapshot color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipeline.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rsplot scene3d snapshot depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rsplot scene3d snapshot"),
        });
        scene.encode_offscreen(
            &mut encoder,
            pipeline,
            &color_view,
            &depth_view,
            frame.background,
        );
        // The orientation indicator is part of the rendered image, so the
        // snapshot draws it too (same pass as `prepare_scene`).
        if let Some((ov, ov_scene, origin)) = overview_pass(scenes, frame) {
            ov.write_uniforms(queue, ov_scene);
            ov_scene.encode_overview(&mut encoder, pipeline, &color_view, &depth_view, origin);
        }

        // Copy the target into a readback buffer with a padded row stride.
        let bpr = padded_bytes_per_row(w);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rsplot scene3d snapshot readback"),
            size: (bpr as u64) * (h as u64),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(h),
                },
            },
            extent,
        );
        queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;

        let rgba = {
            let mapped = buffer.slice(..).get_mapped_range();
            rows_to_rgba8(&mapped, w, h, bpr, pipeline.target_format)
        };
        buffer.unmap();
        Some(rgba)
    }
}

/// Resolve `frame`'s orientation-indicator request into a drawable pass: the
/// slaved-camera frame, the uploaded companion scene, and the viewport origin.
/// `None` when no indicator was requested, its scene has no geometry yet, or
/// the target is smaller than the indicator (silx pins the overview viewport
/// at `origin = (width − 100, height − 100)` in GL bottom-left coordinates,
/// `Plot3DWidget.py:387-388` — the top-right corner, which in wgpu's top-left
/// framebuffer coordinates is `(width − 100, 0)`).
fn overview_pass<'s>(
    scenes: &'s HashMap<Scene3dId, Scene3dGpu>,
    frame: &Scene3dFrame,
) -> Option<(Scene3dOverviewFrame, &'s Scene3dGpu, [u32; 2])> {
    let ov = frame.overview?;
    if frame.size_px[0] < OVERVIEW_SIZE_PX || frame.size_px[1] < OVERVIEW_SIZE_PX {
        return None;
    }
    let ov_scene = scenes.get(&ov.id)?;
    Some((ov, ov_scene, [frame.size_px[0] - OVERVIEW_SIZE_PX, 0]))
}

/// Install the 3D scene GPU resources into `render_state` if not already present.
/// Idempotent — safe to call once per app startup (independent of the 2D
/// [`crate::render::backend_wgpu::install`]).
pub fn install_scene3d(render_state: &RenderState) {
    let mut renderer = render_state.renderer.write();
    if renderer
        .callback_resources
        .get::<Scene3dResources>()
        .is_some()
    {
        return;
    }
    let resources = Scene3dResources::new(&render_state.device, render_state.target_format);
    renderer.callback_resources.insert(resources);
}

/// Upload `geometry` as scene `id`'s current geometry (replacing any existing).
/// Requires [`install_scene3d`] to have run first.
pub fn set_scene3d(render_state: &RenderState, id: Scene3dId, geometry: &Scene3dGeometry) {
    let mut renderer = render_state.renderer.write();
    let res: &mut Scene3dResources = renderer
        .callback_resources
        .get_mut()
        .expect("Scene3dResources not installed — call rsplot::install_scene3d() first");
    let Scene3dResources { pipeline, scenes } = res;
    let scene = scenes
        .entry(id)
        .or_insert_with(|| Scene3dGpu::new(&render_state.device, pipeline));
    scene.upload(
        &render_state.device,
        &render_state.queue,
        pipeline,
        geometry,
    );
}

/// Linear-fog datum for one frame — the port of silx `scene/function.py Fog`
/// (`:70-151`). silx computes the camera-space z extent of the scene bounds and
/// fades each fragment's colour toward the viewport background over
/// `0.9 ×` that extent; [`Scene3dFog::linear`] reproduces `Fog.setupProgram`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scene3dFog {
    /// `fogExtentInfo.x`: `0.9 / (far − near)` in camera-space z (`0` when the
    /// extent is zero) — negative, since camera z runs toward `−∞`.
    pub scale: f32,
    /// `fogExtentInfo.y`: the near end of the scene's camera-space z extent
    /// (the corner closest to the camera), where the fog factor is 0.
    pub near: f32,
    /// Fog colour = viewport background rgb (`function.py:148-151`).
    pub color: [f32; 3],
}

impl Scene3dFog {
    /// Compute the linear-fog datum for `camera` over the scene `bounds`,
    /// fading toward `background` — silx `Fog.setupProgram` +
    /// `Fog._zExtentCamera` (`function.py:124-151`): `(far, near)` is the
    /// camera-space z extent of the bounds corners, `scale = 0.9/(far − near)`
    /// (or 0), and the factor at depth z is `clamp(scale · (z − near), 0, 1)`.
    pub fn linear(camera: &Camera, bounds: (Vec3, Vec3), background: Color32) -> Self {
        let view = camera.extrinsic.matrix();
        let (mn, mx) = bounds;
        let mut far = f32::INFINITY; // most negative camera z
        let mut near = f32::NEG_INFINITY;
        for corner in [
            Vec3::new(mn.x, mn.y, mn.z),
            Vec3::new(mx.x, mn.y, mn.z),
            Vec3::new(mn.x, mx.y, mn.z),
            Vec3::new(mx.x, mx.y, mn.z),
            Vec3::new(mn.x, mn.y, mx.z),
            Vec3::new(mx.x, mn.y, mx.z),
            Vec3::new(mn.x, mx.y, mx.z),
            Vec3::new(mx.x, mx.y, mx.z),
        ] {
            let z = view.transform_point(corner, false).z;
            far = far.min(z);
            near = near.max(z);
        }
        let extent = far - near;
        let scale = if extent != 0.0 { 0.9 / extent } else { 0.0 };
        let rgba = egui::Rgba::from(background);
        Scene3dFog {
            scale,
            near,
            color: [rgba.r(), rgba.g(), rgba.b()],
        }
    }

    /// The fog factor at camera-space depth `cam_z` — the CPU mirror of the
    /// WGSL `apply_fog` mix weight, for tests and previews.
    pub fn factor_at(&self, cam_z: f32) -> f32 {
        (self.scale * (cam_z - self.near)).clamp(0.0, 1.0)
    }
}

/// Per-frame shading options shared by every scene pipeline: silx's viewport
/// fog (`Plot3DWidget.setFogMode`) and the directional light's shininess
/// (`viewport.light.shininess`; 0 in `Plot3DWidget`/`SceneWidget`, 32 in
/// `ScalarFieldView`, `ScalarFieldView.py:928`). The default — no fog,
/// shininess 0 — matches the silx `Plot3DWidget` defaults, which is what the
/// plain [`paint_scene3d`] / [`snapshot_scene3d`] entry points use.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scene3dShading {
    /// Linear fog for this frame; `None` = off (silx `FogMode.NONE`).
    pub fog: Option<Scene3dFog>,
    /// Phong shininess exponent for lit meshes; `0` disables specular.
    pub shininess: f32,
}

/// Build the per-frame render request: camera matrices at the target size,
/// plus the shading uniforms. One owner for the maths shared by the paint and
/// snapshot paths.
fn build_frame(
    id: Scene3dId,
    camera: &Camera,
    background: Color32,
    size_px: [u32; 2],
    shading: Scene3dShading,
    overview: Option<Scene3dId>,
) -> Scene3dFrame {
    let mut cam = *camera;
    cam.set_size((size_px[0] as f32, size_px[1] as f32));
    let mvp = cam.matrix().to_gpu_clip_cols();
    // The view matrix (camera-space transform) drives mesh-normal lighting and
    // the fog/specular positions; it carries no projection, so plain
    // column-major, no depth correction.
    let view_mat = cam.extrinsic.matrix();
    let view = view_mat.to_gpu_cols();
    // Row 2 of the (row-major) view matrix: camera-space z as a dot product.
    let view_row_z = view_mat.rows[2];
    let fog = shading.fog.unwrap_or_default();
    Scene3dFrame {
        id,
        mvp,
        view,
        view_row_z,
        size_px,
        background: egui::Rgba::from(background).to_array(),
        fog_info: [
            fog.scale,
            fog.near,
            if shading.fog.is_some() { 1.0 } else { 0.0 },
            0.0,
        ],
        fog_color: [fog.color[0], fog.color[1], fog.color[2], 0.0],
        shininess: shading.shininess,
        overview: overview.map(|ov_id| build_overview_frame(ov_id, &cam)),
    }
}

/// Register the paint callback that renders scene `id` into `rect` from
/// `camera`'s viewpoint, on `background`, with the silx `Plot3DWidget` default
/// shading (no fog, shininess 0). The camera's aspect is taken from `rect`'s
/// pixel size for this frame (the passed `camera` is not mutated).
/// Requires [`install_scene3d`] + [`set_scene3d`].
pub fn paint_scene3d(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: Scene3dId,
    camera: &Camera,
    background: Color32,
) {
    paint_scene3d_with(
        ui,
        rect,
        id,
        camera,
        background,
        Scene3dShading::default(),
        None,
    );
}

/// [`paint_scene3d`] with explicit per-frame [`Scene3dShading`] (fog +
/// shininess) and an optional orientation indicator — the full silx viewport
/// model. When `overview` is `Some(ov_id)`, the scene uploaded under `ov_id`
/// (see [`SceneWidget`](crate::widget::scene_widget::SceneWidget)'s disc +
/// RGB axes) is rendered as a second pass into the top-right
/// [`OVERVIEW_SIZE_PX`]² corner with a camera slaved to `camera`'s
/// orientation (silx `_OverviewViewport`, `Plot3DWidget.py:51-93`); skipped
/// when the target is smaller than the indicator.
pub fn paint_scene3d_with(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: Scene3dId,
    camera: &Camera,
    background: Color32,
    shading: Scene3dShading,
    overview: Option<Scene3dId>,
) {
    let ppp = ui.ctx().pixels_per_point();
    let w = (rect.width() * ppp).round().max(1.0) as u32;
    let h = (rect.height() * ppp).round().max(1.0) as u32;
    let frame = build_frame(id, camera, background, [w, h], shading, overview);
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        Scene3dCallback { frame },
    ));
}

/// Render scene `id` at `size_px` physical pixels from `camera`'s viewpoint on
/// `background` with the default shading (no fog, shininess 0), reading the
/// result back as tightly packed RGBA8 (`width * height * 4`, top row first).
/// Returns `None` if the scene has no uploaded geometry or the GPU readback
/// fails.
///
/// The passed `camera` is not mutated; its aspect is taken from `size_px` for
/// this render, exactly as [`paint_scene3d`] does — so the snapshot matches the
/// on-screen scene at that size. Unlike [`paint_scene3d`], this renders
/// synchronously off the egui frame loop into its own copyable target, suiting a
/// "save scene to image" action (pair with [`crate::render::save::encode_png`]).
///
/// Requires [`install_scene3d`] + [`set_scene3d`].
pub fn snapshot_scene3d(
    render_state: &RenderState,
    id: Scene3dId,
    camera: &Camera,
    background: Color32,
    size_px: (u32, u32),
) -> Option<Vec<u8>> {
    snapshot_scene3d_with(
        render_state,
        id,
        camera,
        background,
        size_px,
        Scene3dShading::default(),
        None,
    )
}

/// [`snapshot_scene3d`] with explicit per-frame [`Scene3dShading`] (fog +
/// shininess) and an optional orientation indicator (see
/// [`paint_scene3d_with`]), so a snapshot matches a widget rendering with the
/// same options — the indicator is part of the rendered image.
pub fn snapshot_scene3d_with(
    render_state: &RenderState,
    id: Scene3dId,
    camera: &Camera,
    background: Color32,
    size_px: (u32, u32),
    shading: Scene3dShading,
    overview: Option<Scene3dId>,
) -> Option<Vec<u8>> {
    let (w, h) = (size_px.0.max(1), size_px.1.max(1));
    let frame = build_frame(id, camera, background, [w, h], shading, overview);
    let renderer = render_state.renderer.read();
    let res: &Scene3dResources = renderer.callback_resources.get()?;
    res.snapshot_scene(&render_state.device, &render_state.queue, &frame)
}

/// The per-frame render request for one scene: which scene, the camera MVP, the
/// target pixel size, the clear color, and the shading uniforms. Grouping these
/// keeps the prepare API to a single owner rather than a long positional
/// argument list.
#[derive(Clone, Copy)]
struct Scene3dFrame {
    id: Scene3dId,
    /// Column-major, clip-corrected MVP for this frame.
    mvp: [[f32; 4]; 4],
    /// Column-major view matrix (no depth correction); the camera-space normal
    /// transform for mesh lighting.
    view: [[f32; 4]; 4],
    /// Row 2 of the (row-major) view matrix — camera-space z for fog.
    view_row_z: [f32; 4],
    /// Offscreen target size in physical pixels.
    size_px: [u32; 2],
    /// Clear color, linear premultiplied.
    background: [f32; 4],
    /// `(scale, near, on, 0)` — see [`Scene3dFog`].
    fog_info: [f32; 4],
    /// Fog rgb + unused w.
    fog_color: [f32; 4],
    /// Phong shininess for lit meshes (0 = no specular).
    shininess: f32,
    /// The orientation indicator's second pass (silx `_OverviewViewport`);
    /// `None` when hidden.
    overview: Option<Scene3dOverviewFrame>,
}

impl Scene3dFrame {
    /// Write this frame's camera + shading uniforms into `scene`'s param
    /// buffers — shared by the on-screen (`prepare_scene`) and snapshot paths
    /// so they cannot drift.
    fn write_uniforms(&self, queue: &wgpu::Queue, scene: &Scene3dGpu) {
        write_scene_uniforms(
            queue,
            scene,
            self.mvp,
            self.view,
            self.view_row_z,
            self.fog_info,
            self.fog_color,
            self.shininess,
            [self.size_px[0] as f32, self.size_px[1] as f32],
        );
    }
}

/// The orientation indicator's per-frame camera state (silx
/// `_OverviewViewport`, `Plot3DWidget.py:51-93`): the companion scene to draw
/// and the slaved camera's matrices, stamped into that scene's uniforms before
/// the corner-viewport pass.
#[derive(Clone, Copy)]
struct Scene3dOverviewFrame {
    id: Scene3dId,
    mvp: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
    view_row_z: [f32; 4],
}

impl Scene3dOverviewFrame {
    /// Stamp the overview camera into the companion scene's uniforms: no fog,
    /// no specular, and the point-sprite viewport is the overview square (so
    /// the size-[`OVERVIEW_SIZE_PX`] disc fills it).
    fn write_uniforms(&self, queue: &wgpu::Queue, scene: &Scene3dGpu) {
        write_scene_uniforms(
            queue,
            scene,
            self.mvp,
            self.view,
            self.view_row_z,
            [0.0; 4],
            [0.0; 4],
            0.0,
            [OVERVIEW_SIZE_PX as f32, OVERVIEW_SIZE_PX as f32],
        );
    }
}

/// Side of the orientation indicator's square corner viewport in physical
/// pixels — silx `_OverviewViewport._SIZE` (`Plot3DWidget.py:57`).
pub const OVERVIEW_SIZE_PX: u32 = 100;

/// Build the orientation indicator's per-frame state from the tracked main
/// camera — silx `_OverviewViewport._cameraChanged` (`Plot3DWidget.py:80-93`):
/// the overview camera shares the main camera's orientation (direction, up)
/// but sits at `−12 · direction`, looking at the origin of the companion
/// scene (the disc + RGB axes), in a square [`OVERVIEW_SIZE_PX`] viewport.
fn build_overview_frame(id: Scene3dId, main_camera: &Camera) -> Scene3dOverviewFrame {
    let direction = main_camera.extrinsic.direction();
    let up = main_camera.extrinsic.up();
    let camera = Camera::new(
        30.0,
        1.0,
        100.0,
        (OVERVIEW_SIZE_PX as f32, OVERVIEW_SIZE_PX as f32),
        direction * -12.0,
        direction,
        up,
    );
    let view_mat = camera.extrinsic.matrix();
    Scene3dOverviewFrame {
        id,
        mvp: camera.matrix().to_gpu_clip_cols(),
        view: view_mat.to_gpu_cols(),
        view_row_z: view_mat.rows[2],
    }
}

/// Write one scene's camera + shading uniforms into its three param buffers —
/// the single owner of the uniform layout, shared by the main frame and the
/// orientation-indicator pass.
#[expect(
    clippy::too_many_arguments,
    reason = "flat uniform fields; grouping them would just mirror Scene3dFrame"
)]
fn write_scene_uniforms(
    queue: &wgpu::Queue,
    scene: &Scene3dGpu,
    mvp: [[f32; 4]; 4],
    view: [[f32; 4]; 4],
    view_row_z: [f32; 4],
    fog_info: [f32; 4],
    fog_color: [f32; 4],
    shininess: f32,
    viewport: [f32; 2],
) {
    let params = Scene3dParams {
        mvp,
        fog_info,
        fog_color,
        view_row_z,
    };
    queue.write_buffer(&scene.params_buf, 0, bytemuck::bytes_of(&params));
    let point_params = Scene3dPointParams {
        mvp,
        fog_info,
        fog_color,
        view_row_z,
        viewport,
        _pad: [0.0, 0.0],
    };
    queue.write_buffer(
        &scene.point_params_buf,
        0,
        bytemuck::bytes_of(&point_params),
    );
    let mesh_params = Scene3dMeshParams {
        mvp,
        normal_mat: view,
        fog_info,
        fog_color,
        light: [shininess, 0.0, 0.0, 0.0],
    };
    queue.write_buffer(&scene.mesh_params_buf, 0, bytemuck::bytes_of(&mesh_params));
}

/// Lightweight per-frame paint callback (the heavy GPU state lives in
/// [`Scene3dResources`]). Renders offscreen in `prepare`, blits in `paint`.
struct Scene3dCallback {
    frame: Scene3dFrame,
}

impl egui_wgpu::CallbackTrait for Scene3dCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let res: &mut Scene3dResources = resources
            .get_mut()
            .expect("Scene3dResources not installed — call rsplot::install_scene3d() at startup");
        res.prepare_scene(device, queue, egui_encoder, &self.frame);
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let res: &Scene3dResources = resources
            .get()
            .expect("Scene3dResources not installed — call rsplot::install_scene3d() at startup");
        if let Some(scene) = res.scenes.get(&self.frame.id)
            && let Some(blit_bind_group) = &scene.blit_bind_group
        {
            render_pass.set_pipeline(&res.pipeline.blit_pipeline);
            render_pass.set_bind_group(0, blit_bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::scene3d::mat4::{mat4_rotate, mat4_scale, mat4_translate};

    #[test]
    fn bounding_box_with_axes_has_twelve_lines_and_rgb_axes() {
        let mut g = Scene3dGeometry::new();
        g.add_bounding_box_with_axes(
            (Vec3::ZERO, Vec3::new(2.0, 3.0, 4.0)),
            Color32::from_rgb(200, 200, 200),
        );

        // 3 axes + 9 box edges = 12 lines = 24 line vertices; no triangles.
        assert_eq!(g.lines.len(), 24);
        assert!(g.triangles.is_empty());

        // X axis: origin → (2,0,0), red.
        assert_eq!(g.lines[0].pos, [0.0, 0.0, 0.0]);
        assert_eq!(g.lines[1].pos, [2.0, 0.0, 0.0]);
        assert_eq!(g.lines[0].color, egui::Rgba::from(Color32::RED).to_array());
        // Y axis tip (0,3,0) green; Z axis tip (0,0,4) blue.
        assert_eq!(g.lines[3].pos, [0.0, 3.0, 0.0]);
        assert_eq!(
            g.lines[2].color,
            egui::Rgba::from(Color32::GREEN).to_array()
        );
        assert_eq!(g.lines[5].pos, [0.0, 0.0, 4.0]);
        assert_eq!(g.lines[4].color, egui::Rgba::from(Color32::BLUE).to_array());

        // Box edges carry the box color, and the far top corner (2,3,4) appears.
        let box_rgba = egui::Rgba::from(Color32::from_rgb(200, 200, 200)).to_array();
        assert_eq!(g.lines[6].color, box_rgba);
        assert!(
            g.lines.iter().any(|v| v.pos == [2.0, 3.0, 4.0]),
            "the far corner (max) should be a box-edge endpoint"
        );
    }

    #[test]
    fn extend_from_forwards_every_channel() {
        // A source geometry carrying one primitive in each of the six channels.
        let mut src = Scene3dGeometry::new();
        src.add_line([0.0; 3], [1.0; 3], Color32::WHITE); // 2 line verts
        src.add_triangle([0.0; 3], [1.0; 3], [2.0; 3], Color32::RED); // 3 tri verts
        src.add_point([0.0; 3], Color32::GREEN, 4.0, PointMarker::Square); // 1 point
        src.add_mesh_triangle_flat([0.0; 3], [1.0; 3], [2.0; 3], Color32::BLUE); // 3 mesh verts
        src.add_image_layer(Scene3dImageLayer {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            origin: [0.0; 3],
            scale: [1.0; 2],
            interpolation: ImageInterpolation::Nearest,
        });
        src.add_textured_mesh(Scene3dTexturedMesh {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            vertices: vec![[0.0; 3], [1.0; 3], [2.0; 3]],
            uvs: vec![[0.0; 2], [1.0, 0.0], [1.0; 2]],
            interpolation: ImageInterpolation::Nearest,
        });
        src.add_line_pick_anchor([0.5; 3]); // 1 pick-only anchor

        let mut dst = Scene3dGeometry::new();
        assert!(dst.is_empty());
        dst.extend_from(&src);

        // Every channel must be forwarded — not a hand-picked subset.
        assert_eq!(dst.lines.len(), 2);
        assert_eq!(dst.triangles.len(), 3);
        assert_eq!(dst.points.len(), 1);
        assert_eq!(dst.meshes.len(), 3);
        assert_eq!(dst.images.len(), 1);
        assert_eq!(dst.textured_meshes.len(), 1);
        assert_eq!(dst.line_pick_anchors.len(), 1);

        // A second extend appends (does not replace).
        dst.extend_from(&src);
        assert_eq!(dst.lines.len(), 4);
        assert_eq!(dst.textured_meshes.len(), 2);
        assert_eq!(dst.line_pick_anchors.len(), 2);
    }

    #[test]
    fn linear_fog_matches_silx_setup_program() {
        // Camera at (0,0,5) looking down -z; unit cube bounds. Corner camera-z
        // spans [-6, -4]: far = -6, near = -4, extent = -2 →
        // scale = 0.9 / -2 = -0.45 (silx Fog.setupProgram, function.py:135-146).
        let camera = Camera::new(
            30.0,
            0.1,
            100.0,
            (100.0, 100.0),
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        let bounds = (Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));
        let fog = Scene3dFog::linear(&camera, bounds, Color32::from_gray(51));

        assert!((fog.scale - (-0.45)).abs() < 1e-5, "scale = {}", fog.scale);
        assert!((fog.near - (-4.0)).abs() < 1e-5, "near = {}", fog.near);
        // Fog colour is the background rgb (linear space, grey 51 → 0.2 sRGB).
        assert!(fog.color.iter().all(|&c| c > 0.0 && c < 1.0));

        // Factor: 0 at the near end, 0.9 at the far end, clamped past it.
        assert_eq!(fog.factor_at(-4.0), 0.0);
        assert!((fog.factor_at(-6.0) - 0.9).abs() < 1e-5);
        assert!((fog.factor_at(-5.0) - 0.45).abs() < 1e-5);
        assert_eq!(fog.factor_at(-3.0), 0.0); // nearer than near → clamp low
        assert_eq!(fog.factor_at(-100.0), 1.0); // far beyond → clamp high

        // Degenerate extent (flat bounds slab facing the camera): scale = 0,
        // silx's `0.9/extent if extent != 0 else 0`.
        let flat = (Vec3::new(-1.0, -1.0, 0.0), Vec3::new(1.0, 1.0, 0.0));
        let camera_front = Camera::new(
            30.0,
            0.1,
            100.0,
            (100.0, 100.0),
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        // A z=0 plane seen face-on still spans x/y, but all corners share
        // camera z = -5 → extent 0.
        let fog_flat = Scene3dFog::linear(&camera_front, flat, Color32::BLACK);
        assert_eq!(fog_flat.scale, 0.0);
    }

    #[test]
    fn apply_transform_maps_positions_and_inverse_transposes_normals() {
        let mut g = Scene3dGeometry::new();
        g.add_line([0.0; 3], [1.0, 0.0, 0.0], Color32::WHITE);
        g.add_triangle([0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], Color32::RED);
        g.add_point([1.0, 2.0, 3.0], Color32::GREEN, 4.0, PointMarker::Circle);
        g.add_line_pick_anchor([1.0, 1.0, 0.0]);
        // A lit triangle whose (unit) normal is (1,1,0)/√2 at every vertex.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        g.add_mesh_triangle(
            [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            Color32::BLUE,
            [[s, s, 0.0]; 3],
        );
        g.add_textured_mesh(Scene3dTexturedMesh {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0]],
            uvs: vec![[0.0; 2], [1.0, 0.0], [1.0; 2]],
            interpolation: ImageInterpolation::Nearest,
        });

        // Bake scale (2,1,1) then translate (10,0,0).
        let m = mat4_translate(10.0, 0.0, 0.0) * mat4_scale(2.0, 1.0, 1.0);
        g.apply_transform(&m);

        assert_eq!(g.lines[0].pos, [10.0, 0.0, 0.0]);
        assert_eq!(g.lines[1].pos, [12.0, 0.0, 0.0]);
        assert_eq!(g.triangles[2].pos, [10.0, 1.0, 0.0]);
        assert_eq!(g.points[0].pos, [12.0, 2.0, 3.0]);
        assert_eq!(g.line_pick_anchors[0], [12.0, 1.0, 0.0]);
        assert_eq!(g.textured_meshes[0].vertices[2], [12.0, 1.0, 0.0]);
        assert_eq!(g.meshes[1].pos, [12.0, 0.0, 0.0]);
        // Normals map by the inverse-transpose (diag(1/2, 1, 1) here), then
        // renormalize: (1,1,0)/√2 → (1,2,0)/√5 — NOT the naive (2,1,0)/√5.
        let n = g.meshes[0].normal;
        let expect = [1.0 / 5.0f32.sqrt(), 2.0 / 5.0f32.sqrt(), 0.0];
        for (got, want) in n.iter().zip(expect) {
            assert!((got - want).abs() < 1e-5, "normal {n:?}, want {expect:?}");
        }
    }

    #[test]
    fn apply_transform_scales_axis_aligned_image_layers_in_place() {
        let mut g = Scene3dGeometry::new();
        g.add_image_layer(Scene3dImageLayer {
            pixels: vec![0; 4],
            width: 1,
            height: 1,
            origin: [1.0, 1.0, 0.0],
            scale: [0.5, 0.5],
            interpolation: ImageInterpolation::Nearest,
        });
        // Positive per-axis scale + translation keeps the layer (and with it
        // the image row/column pick path).
        let m = mat4_translate(1.0, 2.0, 3.0) * mat4_scale(2.0, 3.0, 4.0);
        g.apply_transform(&m);
        assert_eq!(g.images.len(), 1);
        assert!(g.textured_meshes.is_empty());
        assert_eq!(g.images[0].origin, [3.0, 5.0, 3.0]);
        assert_eq!(g.images[0].scale, [1.0, 1.5]);
    }

    #[test]
    fn apply_transform_converts_rotated_image_layers_to_textured_quads() {
        let mut g = Scene3dGeometry::new();
        g.add_image_layer(Scene3dImageLayer {
            pixels: vec![0; 2 * 4],
            width: 2,
            height: 1,
            origin: [0.0; 3],
            scale: [1.0, 1.0],
            interpolation: ImageInterpolation::Linear,
        });
        // 90° about +z is not representable as an axis-aligned layer → the
        // layer becomes the equivalent textured quad (same corners and UVs as
        // `build_image_gpu`).
        let m = mat4_rotate(std::f32::consts::FRAC_PI_2, 0.0, 0.0, 1.0);
        g.apply_transform(&m);
        assert!(g.images.is_empty());
        assert_eq!(g.textured_meshes.len(), 1);
        let quad = &g.textured_meshes[0];
        assert_eq!(quad.vertices.len(), 6);
        assert_eq!(
            quad.uvs,
            vec![
                [0.0, 0.0],
                [1.0, 0.0],
                [1.0, 1.0],
                [0.0, 0.0],
                [1.0, 1.0],
                [0.0, 1.0]
            ]
        );
        // Far corner (2, 1, 0) rotates to (-1, 2, 0).
        let far = quad.vertices[2];
        assert!((far[0] - (-1.0)).abs() < 1e-5 && (far[1] - 2.0).abs() < 1e-5);
        assert_eq!(quad.interpolation, ImageInterpolation::Linear);
    }

    #[test]
    fn overview_frame_slaves_the_camera_orientation() {
        // silx `_OverviewViewport._cameraChanged` (Plot3DWidget.py:80-93): the
        // overview camera copies the tracked camera's direction and up, posed
        // at −12·direction, with its own 30° fovy, near 1, far 100 projection
        // over the 100×100 viewport (`Plot3DWidget.py:59-62`).
        let direction = Vec3::new(1.0, 2.0, -2.0) * (1.0 / 3.0); // unit
        let main = Camera::new(
            45.0,
            0.5,
            2000.0,
            (800.0, 600.0),
            Vec3::new(5.0, 4.0, 3.0),
            direction,
            Vec3::new(0.0, 1.0, 0.0),
        );
        let ov = build_overview_frame(7, &main);
        assert_eq!(ov.id, 7);

        // The frame's matrices are exactly those of the slaved camera: the
        // main position and projection do not leak in.
        let expected = Camera::new(
            30.0,
            1.0,
            100.0,
            (OVERVIEW_SIZE_PX as f32, OVERVIEW_SIZE_PX as f32),
            main.extrinsic.direction() * -12.0,
            main.extrinsic.direction(),
            main.extrinsic.up(),
        );
        assert_eq!(ov.mvp, expected.matrix().to_gpu_clip_cols());
        assert_eq!(ov.view, expected.extrinsic.matrix().to_gpu_cols());

        // Semantics of that pose: the scene origin sits dead centre at
        // camera-space depth 12, inside the [1, 100] frustum...
        let origin_ndc = expected.matrix().transform_point(Vec3::ZERO, true);
        assert!(origin_ndc.x.abs() < 1e-5 && origin_ndc.y.abs() < 1e-5);
        assert!((-1.0..=1.0).contains(&origin_ndc.z));
        // ...and the slaved up vector maps to screen-up (+y in NDC).
        let up_ndc = expected.matrix().transform_point(main.extrinsic.up(), true);
        assert!(up_ndc.y > 0.0 && up_ndc.x.abs() < 1e-5);
    }
}
