# siplot 3D — `silx.gui.plot3d` parity roadmap

Tracking doc for the port of silx's true-3D scene subsystem
(`silx.gui.plot3d`, ~26k lines / 64 files of OpenGL scene-graph code) onto
siplot's wgpu/egui infrastructure. This is a sibling effort to the 2D
`doc/parity-roadmap.md` (which covers `silx.gui.plot`); the 2D roadmap's scope
line deliberately excluded plot3d, and this doc owns it.

Reference source (consulted, never guessed): `~/codes/silx/src/silx/gui/plot3d/`.

## Scope decision

Full parity target (user sign-off 2026-06-15): port the whole `plot3d` stack —
scene foundation, every 3D item, the `ScalarFieldView` flagship (isosurface +
cut plane), and the surrounding tools/window. Built wave by wave; each wave is
gated (fmt/clippy/nextest per touched crate) and committed one feature at a
time, mirroring the 2D port's cadence.

### N/A (siplot-specific deviations, like the 2D OpenGL/Matplotlib backend split)

- **The Pygfx alternate backend (`*Pygfx` classes).** siplot has one GPU
  backend (wgpu); silx's OpenGL/Pygfx duality has no analogue here.
- **Qt `ParamTreeView` / `_model.py` tree model.** Replaced by an egui
  immediate-mode parameter panel (no retained Qt item model).
- **`QGLWidget`/`OpenGLWidget` plumbing.** Replaced by the egui-wgpu
  `CallbackTrait` offscreen-render-then-blit path (see architecture below).

## Architecture (how 3D lands inside egui-wgpu)

egui's paint render pass is **color-only (no depth attachment)**, so depth-tested
3D cannot draw straight into it. The faithful path mirrors the existing
`ClearCallback`/`ImageCallback` pattern but renders offscreen first:

- A `Scene3dCallback: egui_wgpu::CallbackTrait`:
  - `prepare()` — size an offscreen **color + depth** texture pair to the
    widget's pixel rect, write camera/scene uniforms, and encode a depth-tested
    render pass (clear → draw all 3D primitives) into the offscreen color target.
  - `paint()` — blit the offscreen color texture into egui's pass as a
    viewport-clipped fullscreen triangle.
- Persistent GPU state lives in the same `callback_resources` type-map as the 2D
  renderer, keyed by a scene id.
- **Depth convention:** silx targets OpenGL clip-space z∈[-1,1]; wgpu is z∈[0,1].
  silx's projection matrices are ported verbatim (parity + testability) and a
  fixed [-1,1]→[0,1] clip-correction is applied only at the GPU upload boundary.
- **Matrix storage:** silx numpy matrices are row-major and applied as `M·v`;
  Rust `Mat4` mirrors that row-major layout (line-for-line port, unit-tested
  against silx values) and transposes to column-major only at GPU upload (WGSL
  `mat4x4` is column-major).

## Phases / waves

Legend: ✅ done · ◐ partial · ☐ not started

### Phase 0 — scene foundation (everything depends on it)

| Wave | Item | silx source | Status |
|---|---|---|---|
| P0.1 | `Mat4`/`Vec3` + camera math (look-at, perspective, ortho, rotate, orbit, resetCamera) | scene/transform.py, scene/camera.py | ✅ |
| P0.2 | wgpu line/triangle pipeline + offscreen depth render + blit callback | (siplot infra) | ✅ |
| P0.3 | `SceneWidget` + orbit/pan/zoom interaction + bounding box + axes | scene/interaction.py, primitives.py (Lines/Box/Axes/BoxWithAxes), viewport.py, SceneWidget.py | ✅ |

### Phase 1 — basic 3D items

| Wave | Item | silx source | Status |
|---|---|---|---|
| P1.1 | Scatter3D (points / spheres) | items/scatter.py, primitives Points/ColorPoints/Spheres | ✅ |
| P1.2 | Mesh / Box / Cylinder / Hexagon | items/mesh.py, primitives Mesh3D/ColormapMesh3D + Geometry | ✅ |
| P1.3 | 3D ImageData / ImageRgba / HeightMap | items/image.py, items/_pick.py, primitives ImageData/ImageRgba | ✅ |

P1.1 notes: `Scatter3D` ports silx's `Points`/`_Points` faithfully — billboarded,
pixel-sized markers (all eight `_Points` symbols) via `scene3d_points.wgsl`.
Documented simplifications: colour is mapped through the colormap on the CPU
(`Colormap::color_at`) at geometry-build time rather than in a GPU colormap
texture (points are few vs image rasters); the `Spheres` primitive (shaded 3D
spheres — not used by silx `Scatter3D`, which renders `Points`) is not yet ported.
Per-point picking lands in **Phase 4**: `SceneWidget::pick` returns the hit point's
index (`ScenePickKind::Point`); silx's `_pickFull` per-point *data* payload beyond
the index is the documented remaining tail.

P1.3 notes: `ImageData3D`/`ImageRgba3D` render a 2D image as one textured quad
(`scene3d_image.wgsl` + `Scene3dImageLayer`, an `Rgba8Unorm` texture per layer),
matching silx's single-quad-per-image approach (not per-pixel geometry); image
colour is premultiplied-linear so it round-trips the blit, with nearest/linear
`InterpolationMixIn`. `ImageData3D` colormaps on the CPU (as P1.1/P1.2);
`ImageRgba3D` takes `Color32` pixels directly. `HeightMapData`/`HeightMapRGBA`
render the height field as size-1 square points — exactly how silx renders them
(`primitives.Points`, marker `'s'`) — reusing the point pipeline; mismatched
colour/height sizes are nearest-neighbour resampled. Documented divergence: silx's
resample indexes the column axis by the field *height* (image.py:318/390, a bug on
non-square data); this port uses *width* (agrees for equal-sized data).
`HeightMap` points are picked by `SceneWidget::pick` (Phase 4); silx's image
`_pickFull` pixel-index resolution (mapping a quad hit to a texel index) is the
documented remaining tail there.

P1.2 notes: a `scene3d_mesh.wgsl` pipeline shades lit triangles with silx's
camera-fixed headlight (`DirectionalLight` defaults: ambient 0.3, diffuse 0.7, no
specular), computed per-frame on the GPU from the view-transformed normal. Items
in `render::scene3d_items`: `Mesh3D` (uniform/per-vertex colour) and
`ColormapMesh3D` (per-vertex scalar through a `Colormap`, CPU `color_at` as
Scatter3D), both supporting `triangles`/`triangle_strip`/`fan` modes + optional
indices via a single `expand_triangles` owner (strips/fans expand to a triangle
list, since the GPU path is `TriangleList` only) and a flat-normal fallback when
no normals are given. `Box3D`/`Cylinder3D`/`Hexagon3D` port
`_CylindricalVolume`: faceted Box (4 faces) / Hexagon (6), smooth radial-normal
Cylinder (nb_faces), one or many instances per call. Documented simplifications:
colormap on CPU (as P1.1); lighting params are silx's viewport defaults baked in
(a lighting on/off + parameter API is a later enhancement). Mesh picking lands in
**Phase 4** — the mesh triangles are picked by `SceneWidget::pick` (CPU
ray/triangle).

### Phase 2 — `ScalarFieldView` flagship

| Wave | Item | silx source | Status |
|---|---|---|---|
| P2.1 | Marching-cubes isosurface + ScalarField3D | items/volume.py, silx.math.marchingcubes | ✅ |
| P2.2 | Cut planes + colormap | scene/cutplane.py, primitives PlaneInGroup/ClipPlane | ✅ |
| P2.3 | ScalarFieldView widget + ComplexField3D | ScalarFieldView.py, items/volume.py | ✅ |

P2.3 notes: shipped in three waves. **P2.3a** closed a latent `SceneWidget` bug —
`upload` forwarded only the lines/triangles channels, silently dropping
points/meshes/images/textured-meshes — by adding `Scene3dGeometry::extend_from`
(`render::gpu_scene3d`), the single owner that merges all six channels; the widget
now appends every data-item channel beneath the chrome. **P2.3b** ports silx
`items.volume.ComplexField3D`: a complex field `(re, im)` projected to a real
scalar through a `ComplexMode` feeding an inner `ScalarField3D`. The shared
`ComplexMixIn.ComplexMode` enum (silx puts it on the base shared by the 2D
`ImageComplexData` and 3D `ComplexField3D`) was relocated from
`widget::complex_image_view` to `core::complex` so 2D + 3D share one enum without
inverting the `core → render → widget` layering; `set_complex_mode` clears the
iso-surfaces and keeps the cut plane (silx `setComplexMode`), the two
amplitude-phase composites have no scalar (`to_scalar → 0.0`). **P2.3c** ports the
`ScalarFieldView` flagship widget (`src/widget/scalar_field_view.rs`): it owns one
`ScalarField3D` (iso-surfaces + a cut plane) rendered through a `SceneWidget`,
mirroring silx — `set_data` frames the camera to the volume box only on the
**first** data (silx `centerScene`-once; subsequent updates keep the viewpoint via
the new `SceneWidget::set_bounds_keep_view`), with `add_isosurface` /
`add_auto_isosurface` / `remove_isosurface` / `clear_isosurfaces` mapping 1:1 to
silx, the cut plane configured via `field_mut` + `rebuild`. Geometry is uploaded
eagerly on data-layer change (not per frame), matching `SceneWidget`. The
`setComplexMode`/iso-surface re-resolve UI panel lands with the Phase 3 tools;
3D picking of the field (surfaces + cut plane → value) lands in **Phase 4**
(`ScalarFieldView::pick`, CPU ray-geometry — not the "GPU picking" the earlier
notes assumed).

P2.2 notes: the plane math (`src/core/scene3d/plane.rs`) ports silx
`scene.utils.Plane` + the box/segment intersection helpers (`boxPlaneIntersect`,
`segmentPlaneIntersect`, `angleBetweenVectors`). The GPU side generalises the
P1.3 image quad into `Scene3dTexturedMesh` (`render::gpu_scene3d`) — an
arbitrary world-space triangle list sampling one texture through the same
premultiplied-alpha `image_pipeline` (a shared `build_image_texture_bind_group`
owner; quads and meshes collect into one draw list keyed by `vertex_count`).
`CutPlane` (`render::scene3d_items`) ports silx `items.volume.CutPlane`: a
config item (plane + colormap + interpolation + resolution + visibility) owned by
`ScalarField3D`, hidden by default, reading the field samples from its parent
(silx wires the data `copy=False` — one owner). `ScalarField3D::append_to`
intersects the plane with the volume box (`box_plane_intersect`) → the contour
polygon, rasterises the slice onto a `resolution × resolution` grid (CPU field
sampler matching silx's texture convention — voxel centre `(ix,iy,iz)` at world
`(ix+0.5,…)`, clamp-to-edge, nearest/trilinear), colours it through the colormap
(CPU `color_at`), and emits the fan-triangulated polygon as one textured mesh.
Documented simplifications: the slice is a 2D grid texture rather than silx's
per-fragment 3D-texture sampling, so sharpness is bounded by `resolution` (the
same CPU-colormap deviation as P1.1–P2.1). **Deferred (documented):** silx's
`ClipPlane` (`scene/primitives.py` `ClipPlane` — a scene-graph geometry-clipping
plane that sets a per-fragment `gl_ClipDistance`, cross-cutting every shader) is
not ported; it is not used by `ScalarField3D`'s cut plane and would require a
clip-distance uniform threaded through all 3D pipelines — a separate enhancement,
not part of the cut-plane flagship.

P2.1 notes: `marching_cubes` (`src/core/scene3d/marching_cubes.rs`) is a
line-for-line port of silx's C++ `mc.hpp` slice-by-slice algorithm + the verbatim
256-case lookup tables from `mc_lut.cpp` (Paul Bourke / Cory Bloyd, MIT), driven
the same way as `marchingcubes.pyx`. Output vertices/normals stay in
`(z,y,x)`/`(nz,ny,nx)` (silx's order); `sampling` and `invert_normals` are carried
through faithfully. `ScalarField3D`/`Isosurface` (`render::scene3d_items`) own the
`(depth,height,width)` field and its iso-surfaces: each surface is extracted with
marching cubes and emitted as a lit solid-colour mesh (P1.2 path), mapping the
`(z,y,x)` vertices to world `(x+0.5, y+0.5, z+0.5)` — silx's `_isogroup` swap
matrix + `Translate(0.5,0.5,0.5)`. Field bounds are the full volume box (silx
`BoundedGroup`). Auto-level (`mean_plus_std`, silx's documented default) re-resolves
on data change. Documented simplifications: the cut plane is P2.2; lighting
uses the baked-in viewport defaults (as P1.2). Iso-surface picking lands in
**Phase 4** — the surface triangles are picked by `SceneWidget::pick` (CPU
ray/triangle, not the "GPU picking" framing); silx's `_pickFull`
marching-cubes-per-bin *index* resolution is the documented remaining tail there.

### Phase 3 — tools / window / parity tail

| Wave | Item | silx source | Status |
|---|---|---|---|
| P3.1 | Viewpoint presets (PositionInfo → Phase 4; GroupProperties → P3.2) | actions/viewpoint.py, tools/ViewpointTools.py | ✅ |
| P3.2 | 3D colorbar + egui parameter panel (GroupProperties) | tools/GroupPropertiesWidget.py | ✅ |
| P3.3 | SceneWindow composition + io snapshot + roadmap reconcile | SceneWindow.py, actions/io.py | ✅ |

P3.2 notes: ports silx `tools.GroupPropertiesWidget` as `ScalarFieldProperties`
(`widget::scalar_field_properties`) — an egui form that sets a `ScalarFieldView`'s
presentation and rebuilds it. silx's `GroupPropertiesWidget` applies one property
(colormap / marker / marker-size / line-width) to *all* matching items in a group;
a `ScalarFieldView` owns one colormapped item (the cut plane) plus solid-colour
iso-surfaces, so the panel exposes exactly those: cut-plane visibility, colormap
name (the new `Colormap::set_name`, faithful to silx `Colormap.setName` — rebuilds
the LUT in place keeping the value range), value range, autoscale-over-the-volume
(`autoscale_cut_plane_colormap`), and per-iso level/colour/remove + add
(`addIsosurface`/`removeIsosurface`). The **3D colorbar** reuses the existing 2D
`ColorBarWidget` driven by the cut-plane colormap; silx's `plot3d` package ships
**no** colorbar of its own (verified — nothing matches `colorbar` under
`gui/plot3d/`), so this is a siplot convenience, not silx parity, and is labelled
as such. Verified through an AccessKit harness (`tests/scalar_field_properties_render.rs`):
the Visible checkbox shows the cut plane, Autoscale fits `[5,10] → [0,1]`, and Add
appends an iso-surface at the data-range midpoint. **Deferred (documented):** silx's
generic `plot3d._model` (`core.py`/`items.py`/`model.py` — a `QAbstractItemModel`
tree editor of the whole scene graph) is not ported; it is a generic scene-graph
editor whose faithful port would be speculative for the current item set, and the
concrete per-field form covers a `ScalarFieldView`'s editable properties.

P3.3 notes: shipped in two waves. **P3.3a** ports silx `SceneWindow.SceneWindow`
(a `QMainWindow` composing a `SceneWidget` central widget, a viewpoint toolbar,
an interactive-mode toolbar, a `GroupPropertiesWidget` dock, a `ParamTreeView`
dock, and a `PositionInfoWidget`) as `SceneWindow` (`widget::scene_window`): the
parts that are ported — the `viewpoint_menu` drop-down (P3.1), a
`ScalarFieldView` scene (P2.3c), and a toggleable `ScalarFieldProperties` panel
(P3.2) — composed with the established `show_inside` panel idiom (`Panel::top`
toolbar + `Panel::left` properties + `CentralPanel` scene) so the scene gets a
real pixel rect. The `PositionInfoWidget` was deferred here (needs 3D picking) and
is added in **Phase 4 (PK4)** as a `Panel::bottom` readout fed by the cursor pick.
Not composed (documented): the generic `ParamTreeView` (`plot3d._model`, see P3.2
/ the N/A list). **P3.3b** ports the
save-snapshot capability behind silx `actions/io.py` (`CopyAction`/`SaveAction`/
`PrintAction`, all built on `Plot3DWidget.grabGL()` → a `QImage` saved as
PNG/JPEG): `SceneWidget::snapshot(render_state, size_px)` and the underlying
`snapshot_scene3d` (`render::gpu_scene3d`) render the current scene synchronously
off the egui frame loop into a transient `RENDER_ATTACHMENT | COPY_SRC` target
and read it back as tightly packed RGBA8 — the 3D analogue of the 2D
`WgpuBackend::render_to_rgba`, reusing the same `save.rs` readback helpers
(`padded_bytes_per_row` / `rows_to_rgba8`); pair with `encode_png` for the
grab-then-save flow. Structural (not duplicated): `encode_offscreen` now takes
the color/depth views as parameters, so the on-screen `prepare` path and the
snapshot path share one draw sequence (the snapshot is pixel-for-pixel the
rendered scene); the persistent blit target stays `TEXTURE_BINDING`-only and the
snapshot uses its own copyable target. Verified by headless readback
(`tests/scene_window_render.rs`: the iso-surface renders through the composed
window, the View toolbar + properties controls are present, and the Properties
toggle hides the panel; `tests/scene_snapshot_render.rs`: a snapshot captures the
iso colour and encodes to a valid PNG, and a non-square size returns a
`width*height*4` buffer). **N/A (Qt chrome):** the `QFileDialog` save/print
dialogs, clipboard copy, and the `VideoAction` PNG-serie/MNG export are Qt-shell
plumbing around the same `grabGL` primitive — the snapshot primitive is the
ported piece; wiring it to a file/clipboard/print sink is application-side.

P3.1 notes: ports silx's **viewpoint presets** in full. `SceneWidget::set_viewpoint`
mirrors `actions/viewpoint.py._SetViewpointAction` — `camera.extrinsic.reset(face)`
then `centerScene()` — for all seven faces (front/back/left/right/top/bottom/side,
the existing `CameraFace`); `SceneWidget::rotate_scene(angle_degrees)` ports
`RotateViewpoint`'s per-frame `viewport.orbitCamera("left", angle)` as a primitive
the caller animates. `viewpoint_menu` (`widget::scene_widget`) ports
`tools.ViewpointTools.ViewpointToolButton` — a "View" drop-down whose items invoke
the presets, verified end-to-end through an AccessKit harness click
(`tests/scene_viewpoint_render.rs`). **Relocated (now delivered):**
`tools.PositionInfoWidget` is built entirely on `SceneWidget.pickItems(x, y, …)`
— 3D scene picking — so it could not ship in P3.1 without that picker. Picking is
CPU ray-geometry (not GPU readback, as the earlier notes wrongly framed it) and is
ported in **Phase 4** (PK1–PK4); the readout (`ScenePositionInfo`) ships in **PK4**.
The `GroupPropertiesWidget` properties panel is scoped with
the egui parameter panel in **P3.2** (its silx `tools/GroupProperties.py` +
`_model/*` source), so P3.1 stops at the viewpoint tools. The
`PositionInfoWidget` it depends on is delivered in **Phase 4**.

### Phase 4 — picking + PositionInfoWidget

| Wave | Item | silx source | Status |
|---|---|---|---|
| PK1 | Pick ray + segment/triangle intersection (CPU core) | items/_pick.py `PickContext.getPickingSegment`, scene/utils.py `segmentTrianglesIntersection` | ✅ |
| PK2 | `SceneWidget::pick` traversal (surfaces + points) | items/_pick.py `_pickFull`, scene/viewport.py | ✅ |
| PK3 | `ScalarField3D` cut-plane / volume value pick | items/volume.py `CutPlane._pickFull`, scene/utils.py | ✅ |
| PK4 | `ScenePositionInfo` (PositionInfoWidget) + SceneWindow wiring | tools/PositionInfoWidget.py | ✅ |

**Framing correction (supersedes the "deferred with GPU picking" notes
above).** Earlier waves described 3D picking as *GPU picking* awaiting a
colour-id readback pass. That was wrong: silx 3D picking is **CPU
ray-geometry intersection**, no GPU readback. `PickContext.getPickingSegment`
(`items/_pick.py`) unprojects the click to a near→far segment over the full
NDC z-range (`viewport.pick` is a no-op stub), and each item's `_pickFull`
runs `segmentTrianglesIntersection` (signed-tetrahedron volumes,
Kensler–Shirley 2006), `segmentVolumeIntersect`, or a segment/plane test —
all on CPU geometry siplot already holds. Phase 4 ports that; no GPU-picking
pass exists to defer.

PK1 notes: `core::scene3d::pick` (`src/core/scene3d/pick.rs`) is the pure CPU
core, depending only on `Camera` + `Mat4` (core layer). `picking_segment(camera,
ndc)` unprojects screen NDC to a world near→far segment via
`camera.matrix().inverse()` (the silx-convention [-1,1] NDC matrix, so its
inverse unprojects from silx NDC; near at z=-1, far at z=+1, with perspective
divide) — the analogue of `getPickingSegment(frame='scene')`.
`segment_triangles_intersection(segment, &[[Vec3;3]])` is a line-for-line port of
silx `segmentTrianglesIntersection` (signed-tetrahedron-volume test, skip
degenerate, parametric `t ∈ [0,1]`), returning `TriangleHit { triangle, t,
barycentric }` sorted near→far. Unit-tested incl. a `picking_segment` round-trip.

PK2 notes: `SceneWidget::pick(ndc) -> Option<ScenePick>` (`widget::scene_widget`)
traverses the widget's own CPU geometry (no GPU): it builds the segment, runs
`segment_triangles_intersection` over `Scene3dGeometry::pick_triangles()` (the
triangles channel + the mesh channel, via `chunks_exact(3)`), and tests
`pick_points()` by projecting each point to NDC and taking those within
`PICK_POINT_TOLERANCE_PX` (7 px). Nearest by NDC depth wins; `ScenePick` carries
the world `position`, `ndc_depth`, and `kind` (`Surface` or `Point { index }`).
`Scene3dGeometry` owns its vertex layout via the `pick_triangles`/`pick_points`
accessors, so the widget never reaches into vertex structs. Covers every
triangle item (mesh / iso-surface / box / cylinder / hexagon) and every point
item (scatter / height-map) — the work the P1.x/P2.x notes called "`_pickFull`
deferred". Verified render-free (`tests/scene_pick.rs`).

PK3 notes: `ScalarField3D::value_at(world) -> Option<f32>` samples the field at a
world position (box test, then the voxel-centre sampler used by the cut plane,
honouring its interpolation), and `ScalarField3D::pick_cut_plane(segment)` returns
the segment/plane intersection when the cut plane is visible and the hit lies
inside the volume box (`segment_plane_intersect` + `value_at`). The cut plane is a
textured mesh (not in the triangle channel), so it is picked against the field
directly rather than through `SceneWidget::pick`. Unit-tested over a ramp field,
keyed to the voxel-centre convention (world z=2.5 ⇒ z-index 2).

PK4 notes: `ScalarFieldView::pick(ndc) -> Option<FieldPick>` unifies the two
channels silx's `PositionInfoWidget` reduces to for a `ScalarFieldView` — the
data surfaces / scatter (`SceneWidget::pick`) and the cut plane
(`ScalarField3D::pick_cut_plane`) — taking the nearest by NDC depth and sampling
the value at the chosen world position with `value_at` (one value path for
surface and cut-plane hits alike). `FieldPick { position, value }` is the data
silx reads from a `PickingResult`. `ScenePositionInfo`
(`widget::scene_position_info`) ports `tools.PositionInfoWidget`: an X/Y/Z/Data
readout (silx `_xLabel`/`_yLabel`/`_zLabel`/`_dataLabel`), each `-` when nothing
is picked. `SceneWindow` composes it in a `Panel::bottom` row and feeds it the
cursor pick each frame (one-frame lag — the scene rect it picks against is only
known after the central panel lays out, the idiomatic egui immediate-mode
trade-off). **N/A (Qt chrome):** silx's interactive picking-mode toggle action is
Qt-shell, like the rest of the `SceneWindow` toolbars. **Remaining tail
(documented, not blocking):** silx's per-item `_pickFull` returns richer payloads
than world-position + sampled value — data-index/bin resolution for scatter, image
pixel indices for `ImageData3D`, marching-cubes bin for iso-surfaces. siplot's
pick returns world position + field value + point index, which is exactly what the
`PositionInfoWidget` consumes; the richer per-item index payloads are a later
enhancement, not a separate "GPU picking" effort. Verified
`tests/scalar_field_pick.rs` (cut-plane hit + value, and the empty-pick case) and
the `Data: -` readout assertion in `tests/scene_window_render.rs`.

## Verification

Per the project's empirical pattern (no golden images): headless wgpu pixel
readback via `egui_kittest` for render correctness, plus pure-compute unit tests
for the math (camera/projection values vs silx, marching-cubes vs known cubes,
transform round-trips). Honest labels: render-verified, not pixel-compared to
silx's OpenGL output (different rasterizer).
