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
| P0.3 | `SceneWidget` + orbit/pan/zoom interaction + bounding box + axes | scene/interaction.py, primitives.py (Lines/Box/Axes/BoxWithAxes), viewport.py, SceneWidget.py | ☐ |

### Phase 1 — basic 3D items

| Wave | Item | silx source | Status |
|---|---|---|---|
| P1.1 | Scatter3D (points / spheres) | items/scatter.py, primitives Points/ColorPoints/Spheres | ☐ |
| P1.2 | Mesh / Box / Cylinder / Hexagon | items/mesh.py, primitives Mesh3D/ColormapMesh3D + Geometry | ☐ |
| P1.3 | 3D ImageData / ImageRgba / HeightMap | items/image.py, items/_pick.py, primitives ImageData/ImageRgba | ☐ |

### Phase 2 — `ScalarFieldView` flagship

| Wave | Item | silx source | Status |
|---|---|---|---|
| P2.1 | Marching-cubes isosurface + ScalarField3D | items/volume.py, silx.math.marchingcubes | ☐ |
| P2.2 | Cut planes + colormap | scene/cutplane.py, primitives PlaneInGroup/ClipPlane | ☐ |
| P2.3 | ScalarFieldView widget + ComplexField3D | ScalarFieldView.py, items/volume.py | ☐ |

### Phase 3 — tools / window / parity tail

| Wave | Item | silx source | Status |
|---|---|---|---|
| P3.1 | Viewpoint presets + PositionInfo + GroupProperties | actions/viewpoint.py, tools/* | ☐ |
| P3.2 | 3D colorbar + egui parameter panel | tools/GroupProperties.py, _model/* (→ egui) | ☐ |
| P3.3 | SceneWindow composition + io snapshot + roadmap reconcile | SceneWindow.py, actions/io.py | ☐ |

## Verification

Per the project's empirical pattern (no golden images): headless wgpu pixel
readback via `egui_kittest` for render correctness, plus pure-compute unit tests
for the math (camera/projection values vs silx, marching-cubes vs known cubes,
transform round-trips). Honest labels: render-verified, not pixel-compared to
silx's OpenGL output (different rasterizer).
