# Changelog

All notable changes to this workspace are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
pre-1.0 [Semantic Versioning](https://semver.org/) (a `0.x` minor bump may carry
breaking changes).

This is a workspace of three crates released together: **rsplot** (the plotting
library), **rsdm** (a PyDM-style EPICS display layer built on rsplot), and
**adl2rsdm** (a MEDM `.adl` → RsDM-Rust-source converter).

## [Unreleased]

## [0.5.6] - 2026-07-19

### Changed

- **rsplot**: the toolbar Print action no longer depends on the `printers`
  crate, so a default build links no CUPS library (`-lcups`) and needs no
  `libcups2-dev` at build time. Printer enumeration and job submission now shell
  out to the `lpstat` / `lp` command-line tools. Printing works wherever those
  are on `PATH` (CUPS on Linux/macOS); on platforms without them (e.g. Windows)
  the printer list is empty and the print action reports no destination
  (`print_graph` returns `Ok(false)`). The public `print_graph` /
  `print_graph_to` signatures are unchanged.

### Removed

- **rsplot**: the `printers` crate dependency (and its transitive CUPS /
  `libcups` build requirement).

## [0.5.5] - 2026-07-18

### Changed

- **Bumped the EPICS backends `epics-ca-rs` / `epics-base-rs` / `epics-pva-rs`
  to 0.24.0** (from 0.22.x), affecting `rsdm`'s `ca`/`pva`/`calc` features
  only. Three breaking API changes were adapted:
  - `ConnectionEvent::Unresponsive` was removed — 0.24 folds the echo-timeout
    into `Disconnected` (an unresponsive circuit disconnects every consumer; the
    distinction now survives only inside the client's `ChannelState`). rsdm
    already handled both identically.
  - The pvAccess RPC reply became `RpcReply` (an enum) instead of a
    `(descriptor, value)` tuple; its `Empty` variant is pvxs's no-value
    `ExecOp::reply()`. `rsdm` reads the value through `RpcReply::into_value`,
    treating `Empty` as the existing "connected, no scalar sample" case.
  - `EnumInfo` became `#[non_exhaustive]` (it gained a `string_form`); a test
    constructs it via `EnumInfo::new` instead of a struct literal.

## [0.5.4] - 2026-07-16

A documentation-only patch release. No code, API, or behaviour changes.

### Added

- **`rsdm` and `adl2rsdm` now carry a README**, which crates.io renders on each
  crate's page. Both crates previously shipped without a `readme` field or a
  README file, so their crates.io pages showed nothing while `rsplot`'s
  rendered normally. crates.io bakes the README per published version, so this
  is why a new release was cut rather than an edit to 0.5.3.

## [0.5.3] - 2026-07-10

A regression release. A workspace-wide upstream-parity audit (silx / PyDM /
adl2pydm + MEDM C) found that three of the fixes 0.5.2 shipped were wrong, and
turned up ten further divergences from the reference behaviour. All are fixed
here. `VolumeRaycaster` users should upgrade: the volume it rendered in 0.5.2
carried no thickness cue and lost the hue of faint voxels.

### Fixed — `rsplot`

- **Volume opacity depends on the thickness a ray crosses again.** 0.5.2's
  "Beer-Lambert" correction divided by the *step count*, an exponent carrying no
  world distance, so every ray accumulated the same transmittance whatever chord
  it traversed: a uniform volume rendered as a flat silhouette, and at the
  default 256 steps the correction was exactly the identity. Each sample is now
  corrected from a reference *spacing* (`box_diagonal / 256`, a world distance),
  which keeps the step-count invariance 0.5.2 was after while restoring the
  density cue. A ray crossing the full box diagonal at the default step count
  renders exactly as before.
- **Faint voxels keep their colour.** The premultiplied 3-D texture 0.5.2
  introduced stored the product `rgb · a` in 8 bits, and the shader must divide
  the coverage back out to recover the straight colour — a product that at
  coverage `a` resolves the quotient only to steps of `1/a`. An authored
  `(255, 128, 0)` voxel at `a = 3/255` read back as `(255, 170, 0)`; at
  `a = 1/255` no hue survived at all. The texture is now `Rgba16Float`, which
  carries the straight colour exactly at every coverage while the filter still
  interpolates premultiplied colour (no return of the dark fringe). **This
  doubles the volume texture's VRAM.**
- **`VolumeRaycaster::remove` no longer blanks another view.** Two views sharing
  a `VolumeId` share one texture, by design; `remove` freed it outright, so the
  survivor silently rendered nothing. The texture is now claim-counted and lives
  exactly as long as the views holding its id. Dropping a view releases its
  claim too, so a `VolumeRaycaster` that is simply dropped no longer pins its
  VRAM for the app's lifetime.
- **Alt+Wheel zooms X only and Shift+Wheel zooms Y/Y2 only**, the silx bindings
  the axes menu already advertised. Plain wheel and keep-aspect are unchanged.
- **Middle-button drag pans in every interaction mode**, as in silx. The
  existing right-button pan is kept.
- **The Reset-Zoom toolbar button disables when neither axis autoscales**, and
  its tooltip names the single autoscaling axis when exactly one is on.
- **Programmatic `set_limits` and toolbar Zoom-In/Out repair their limits.**
  Inverted, degenerate or float32-overflowing ranges reached the transform
  unrepaired; every view-limits commit now runs the same clamp, including the
  Y2 and extra-axis paths.
- **The colormap dialog's histogram drops samples equal to the range maximum**,
  as `Histogramnd` does with its default half-open last bin.
- **`Scatter` BINNED_STATISTIC drops points on the max edge** instead of
  clamping them into the last bin, matching `scipy.stats.binned_statistic_2d`
  as silx calls it. A degenerate extent now admits no point rather than
  collapsing every point into bin 0.

### Fixed — `rsdm`

- **`calc://` re-evaluates when a child connects or disconnects.** PyDM
  re-emits on a connection-state change; rsdm only watched values.
- **`calc://` no longer retriggers on a child's alarm or metadata change**, only
  on an actual value change, as PyDM's `update` list specifies.
- **A `dialect = medm` expression reading operand `I` or `J` re-evaluates on an
  alarm status / severity change.** MEDM's `setDynamicAttrMonitorFlags` adds
  `monitorStatusChanged` and `monitorSeverityChanged` exactly when the
  expression uses those operands. rsdm never watched severity, so a transition
  left the calc showing a stale value, and it had no alarm status to read at all
  — operand `I` bound a constant `0.0`. Both are now monitored, and only when
  the expression reads them.

### Added — `rsdm`

- **`ChannelState::status`** carries the wire alarm status — the record's `STAT`
  for `ca://`, the pvAccess `alarm.status` enum for `pva://`. It backs the MEDM
  calc operand `I`.

### Fixed — `adl2rsdm`

- **Embedded-display and composite-file children take the host screen's
  colormap.** MEDM's `parseCompositeFile` parses and discards the child file's
  `"color map"` block and resolves the children's `clr`/`bclr` against the
  parent display's palette; adl2rsdm used the child's own. Screens on the
  default palette are unaffected; those with a custom palette rendered every
  embedded widget in the wrong colours.
- **An arc with no `path` spans 90°, not 360°** — MEDM's `createDlArc` default.

### Changed — `rsplot`

- The volume 3-D texture is `Rgba16Float` rather than `Rgba8Unorm`, doubling its
  VRAM. See the faint-voxel fix above for why an 8-bit texture cannot carry the
  data.
- `VolumeRaycaster` now holds a clone of its `RenderState` (a bundle of `Arc`s)
  so it can release its texture claim on drop.

## [0.5.2] - 2026-07-08

A correctness and robustness pass over the `VolumeRaycaster` shipped in 0.5.1,
from a post-release review, plus two `rsdm` data-engine race fixes surfaced by
the cross-platform CI. `adl2rsdm` is re-released in lockstep (no functional
change).

### Fixed — `rsplot`

- **Dark fringe around opaque regions is gone.** The 3-D volume texture stored
  straight-alpha RGBA, so the linear sampler bled a transparent voxel's `rgb=0`
  into neighbouring colour at a boundary, darkening it. The volume is now
  premultiplied on upload and composited in premultiplied space, so the filter
  interpolates colour that keeps its hue. (`premultiply_rgba` is unit-tested.)
- **Opacity no longer depends on the ray step count.** Per-sample coverage was
  composited as-is, so raising `set_steps` also made the whole volume more
  opaque. Each sample's coverage is now Beer-Lambert–corrected to a 256-step
  baseline, so opacity depends only on the data and `set_alpha_scale`; the
  default view is unchanged.
- **`set_steps` is clamped to `[1, 4096]`.** A very large value marched every ray
  that many times in one frame and could trip the OS GPU-timeout watchdog and
  reset the device.
- **Invalid volumes are rejected up front.** `set_volume` now validates the
  extent and that the RGBA length matches `depth·height·width·4`, so a bad call
  is a no-op (keeping any prior upload) instead of a wgpu validation panic deep
  in the render backend.
- **Ctrl/Cmd-pan tracks the pointer one-to-one.** The pan was anchored on the far
  plane, so the volume moved faster than the cursor; it is now anchored at the
  box-centre depth (the same depth the wheel zoom uses).
- **Same `VolumeId` painted twice in one frame keeps each camera.** The per-id
  uniform buffer was shared between `prepare` and `paint`, so two callbacks with
  one id both rendered from whichever prepared last (e.g. the same volume in two
  panels). Dynamic camera state moved to per-callback buffers.

### Changed — `rsplot`

- **`VolumeRaycaster::remove` / `remove_volume_raycaster(id)`** free a volume's
  GPU resources (3-D texture, bind group, uniform buffer); previously an id's
  VRAM was held for the app's lifetime with no way to release it.
- Dropped the unused `cam_pos` uniform (the shader un-projects rays through
  `inv_mvp`), shrinking the per-frame uniform buffer.

### Fixed — `rsdm`

- **`ca://` no longer leaks a duplicate initial sample.** The connect-time
  metadata fetch posted the initial value unconditionally, but the connection
  task can run its connect handler more than once (the CA client emits a native
  DBR-type "change" on the first connect, re-entering it). The second post
  re-emitted a value the value monitor had already delivered, and a strip-chart /
  event-plot `subscribe_values` created in between received it as a spurious
  sample. The connect-time value is now gated on an actual change, like the
  monitor path.
- **`calc://` always publishes its initial derived value.** The poll loop
  consumed a child's recompute trigger even on ticks where not every child was
  connected yet, so if the last child to connect was not itself a triggering
  variable (an `update` list excluding it), the first all-connected tick saw no
  change and never ran the initial evaluation — the calc stayed connected but
  valueless. Child stamp changes are now folded in only once every child is
  connected, so the initial evaluation is order-independent.

## [0.5.1] - 2026-07-07

A feature-bearing patch over the `0.5.0` crate rename: a new volume-rendering
widget, a toolbar restructure across the image-widget family, and the profile
overlay drawn on the image. Also the first cross-platform CI (Linux / macOS /
Windows), which surfaced and fixed a Direct3D-12 shader-portability bug. `rsdm`
and `adl2rsdm` are re-released in lockstep (no functional change beyond an
internal lint cleanup).

### Added — `rsplot`

- **`VolumeRaycaster`** — an interactive GPU direct-volume-rendering widget.
  Ray-marches a `(depth, height, width)` RGBA8 volume in a fragment shader
  (`shaders/volume_raycaster.wgsl`): a full-screen triangle un-projects each
  pixel through the inverse camera matrix, slab-tests the volume box, and
  front-to-back alpha-composites samples of a 3-D texture straight into egui's
  render pass (premultiplied-alpha blend, no offscreen target or depth buffer).
  Orbit / pan / wheel-zoom reuse the existing `Camera` + `OrbitDrag` / `PanDrag`
  interaction state machines. Transfer knobs: `set_alpha_scale` (global opacity),
  `set_steps` (samples per ray), `set_cull_floor` (skip near-transparent
  samples). Unlike `ScalarFieldView` (iso-surface *geometry*), this renders the
  volume itself — every voxel's colour and opacity contribute along the ray.
  Naga-validated headlessly plus an `egui_kittest` wgpu render test (opaque
  volume ⇒ visible colour, transparent volume ⇒ nothing).
- **Compact plot control toolbar.** The `Plot2D` control toolbar now renders
  compact by default — the essential buttons (reset zoom, box-zoom / pan modes,
  invert Y, keep-aspect) stay in the row and everything else folds into a single
  `⋯` overflow menu, so the toolbar stays one line and the plot keeps its
  height. Toggle with `Plot::set_toolbar_compact` (`toolbar_compact` to read).
- **Shared control toolbar on `ImageView`, `ImageStack`, and `CompareImages`.**
  All three now surface the standard silx plot controls above their own rows,
  matching the rest of the image-widget family (previously reachable only on the
  bare inner plot).
- **`ImageView` image controls as detached settings windows.** The image-specific
  controls (interp / agg / alpha / profile / mask) are now toolbar buttons that
  each open their own detached settings window, matching silx's toolbar-action /
  dock-widget layout instead of inline combos and sliders. The colorbar stays a
  plain toggle.
- **Profile overlay drawn on the image.** The profile ROI and its width band are
  now drawn directly on the image while a profile tool is active.

### Fixed — `rsplot`

- **Curve rendering no longer fails on Windows / Direct3D-12.** The curve
  fragment shader ended in a function-terminal `discard`, which naga's HLSL
  backend emits as a path with no return value; the D3D12 FXC compiler rejects
  it (X3507, "not all control paths return a value"), so `create_render_pipeline`
  for the curve pipeline — and every curve / scatter draw — failed on Windows.
  Vulkan and Metal tolerate it, so it was invisible until cross-platform CI. A
  trailing unreachable return keeps the shader portable.
- **The plot no longer zooms while a profile drag positions the ROI.** A drag
  that places or resizes a profile ROI is consumed by the profile tool instead
  of also driving a box zoom.

## [0.4.2] - 2026-07-02

Follow-up patch refining the 0.4.1 trackpad-momentum fix. `rsplot` only; `rsdm`
and `adl2rsdm` are re-released in lockstep (no functional change).

### Added — `rsplot`

- **`Plot::scroll_zoom` flag (default `true`)** to disable wheel/trackpad zoom
  per plot. When `false` the whole wheel handler is skipped — no scroll is read,
  no momentum guard runs, no zoom happens — while box-drag zoom, the toolbar
  Home/Zoom buttons, and the context menu are unaffected. A consumer that finds
  wheel zoom troublesome can opt out with `plot.plot_mut().scroll_zoom = false`.

### Fixed — `rsplot`

- **Wheel-zoom no longer stops working on plots that refit to data on every
  update.** The 0.4.1 momentum guard was armed inside the low-level
  `reset_zoom_to_data_range`, which the widget also funnels its
  autoscale-refit-on-content-change through (`apply_auto_limits` →
  `apply_limits_from_data_bounds`). A plot rebuilding its curves each update
  re-armed the guard every frame, so the handler swallowed every scroll and
  wheel-zoom stopped entirely (box-drag zoom and Zoom Back were unaffected). The
  arm now lives on the user-facing reset verbs (`Plot::reset_zoom` arms after the
  refit; `Plot::zoom_back` already armed its own) and the shared low-level path
  no longer arms — so autoscale refits leave a later wheel-zoom alone, while the
  context-menu Reset Zoom / Zoom Back over the data area still suppress momentum.
- **Context-menu "Reset Zoom" refits to the live data range instead of restoring
  a stale home snapshot.** For a long-lived plot whose content is rebuilt while
  zoomed in, `home_limits` was re-captured as the *zoomed* view, so Reset Zoom
  reverted to the zoom (a no-op) instead of showing the full data. It now calls
  `Plot::reset_zoom` — refit the autoscale-on axes to live data, matching the
  toolbar Home button and silx `resetZoom`. `home_limits` is still captured on
  first show for external consumers; only this menu item stops reading it.

## [0.4.1] - 2026-07-02

A single-fix patch release: the macOS trackpad-momentum reset bounce-back in
`rsplot` (below). `rsdm` and `adl2rsdm` are re-released in lockstep (no
functional change).

### Fixed — `rsplot`

- **Reset Zoom / Zoom Back no longer bounces back under macOS trackpad momentum.**
  A view reset (right-click *Reset Zoom*, *Zoom Back*, or reset-to-data) restores
  the view while the pointer sits over the data area, but a macOS trackpad /
  Magic Mouse keeps sending *momentum* scroll for ~1 s after the gesture ends — so
  `smooth_scroll_delta` stayed non-zero and the wheel-zoom handler re-zoomed the
  just-restored view on the frames right after the menu closed. Box-zoom leaves no
  residual scroll and so was unaffected, matching the reported asymmetry. A new
  `Plot::reset_scroll_guard`, armed by every view-reset path (`Plot::zoom_back`,
  `Plot::reset_zoom_to_data_range`, and the *Reset Zoom* context-menu item) and
  consumed by the single wheel-zoom handler, swallows the decaying momentum until
  the scroll settles back to zero, then disarms so a fresh gesture zooms normally.
  Non-momentum wheel mice are unaffected: the scroll is already zero the frame
  after a reset, so the guard disarms immediately.

## [0.4.0] - 2026-06-16

Post-`0.3.0` deep-audit pass over the `silx.gui.plot` fit subsystem and marker
set: completes every silx `FitManager` fit theory and the last missing marker
glyph. `rsplot` only; `rsdm` and `adl2rsdm` are re-released in lockstep (no
functional change).

### Added — `rsplot` fit theories (silx `FitManager` parity)

A FitWidget audit found rsplot exposed only **8 of silx's 19 `FitManager`
theories** (`silx/math/fit/fittheories.py`). The missing 11 are now ported,
byte-faithful to silx `funs.c`, each a new `PeakModel` + `FitModelChoice`
variant fitted through the existing iterative path. **All 19 silx fit theories
are now implemented.**

- **Area Lorentz** (`sum_alorentz`), **Split Gaussian** (`sum_splitgauss`),
  **Split Lorentz** (`sum_splitlorentz`).
- **Area Pseudo-Voigt** (`sum_apvoigt`), **Split Pseudo-Voigt**
  (`sum_splitpvoigt`), **Split Pseudo-Voigt 2** (`sum_splitpvoigt2`).
- **Degree 2–5 Polynomial** fit theories (`fitfuns.poly` / `estimate_poly`),
  reusing the numpy-convention `polyfit`/`poly_eval` — distinct from the
  polynomial *background* theory.
- **Hypermet** (`sum_ahypermet`): a Gaussian with short-tail, long-tail and step
  terms (`tail_flags = 15`, silx's default `HypermetTails`). Tail ratios seed to
  silx's `Initial*` CONFIG values; silx's per-tail `CFIXED`/`CQUOTED` defaults
  are reachable through the existing per-parameter constraint UI (consistent with
  rsplot's all-`Free` default-constraint invariant).

### Added — `rsplot` markers

- **Heart** (`♥`) marker glyph (silx `HEART`), a cardioid SDF in the marker
  shader plus the CPU legend icon — completing all 18 silx marker symbols.

## [0.3.0] - 2026-06-16

This release completes the `silx.gui.plot` 2D parity port (the queue is now
exhausted save for a handful of documented platform/perf residuals) and adds a
full true-3D scene subsystem ported from `silx.gui.plot3d`.

### Added — `rsplot` 3D scene subsystem (`silx.gui.plot3d` port)

A full true-3D scene stack ported from `silx.gui.plot3d` onto rsplot's
wgpu/egui infrastructure, rendered through an offscreen depth-tested pass that
blits into egui's color-only render pass. Tracked wave by wave in
`doc/plot3d-parity-roadmap.md`.

- **Scene foundation**: a row-major `Mat4`/`Vec3` + `Camera` math layer (look-at,
  perspective/orthographic projection, orbit/pan/zoom, `resetCamera`) ported
  line-for-line from silx and unit-tested against its values; an interactive
  **`SceneWidget`** (left-drag orbit, right-drag pan, wheel zoom) with a bounding
  box + RGB axes chrome.
- **3D items**: `Scatter3D` (billboarded point markers), `Mesh3D` /
  `ColormapMesh3D` with silx's camera-fixed headlight shading, the
  `Box3D` / `Cylinder3D` / `Hexagon3D` cylindrical-volume primitives, and 3D
  `ImageData` / `ImageRgba` / `HeightMap` textured-quad items.
- **`ScalarFieldView` flagship**: a marching-cubes iso-surface extractor (silx's
  256-case lookup ported verbatim) plus a colormapped cut plane through a
  `ScalarField3D` volume, and `ComplexField3D` (a complex field projected to a
  real scalar through a shared `ComplexMode`). `setData` frames the camera only
  on first data, matching silx `centerScene`-once.
- **Tools / window**: the seven silx **viewpoint presets** with a "View"
  drop-down (`viewpoint_menu`) and a `rotate_scene` orbit primitive; a
  `ScalarFieldProperties` egui panel (port of `GroupPropertiesWidget`:
  cut-plane visibility, colormap, value range, autoscale, per-iso level/colour/
  add/remove) with a colorbar reusing the 2D `ColorBarWidget`; a composed
  **`SceneWindow`** (toolbar + scene + toggleable properties panel); and an
  off-screen **scene snapshot** (`SceneWidget::snapshot` / `snapshot_scene3d`)
  reading the rendered scene back as RGBA8 for `encode_png` (the analogue of
  silx `grabGL` + save-as-PNG).
- **3D picking + `PositionInfoWidget`**: CPU ray-geometry picking ported from
  silx (`PickContext.getPickingSegment` + `segmentTrianglesIntersection`, *not* a
  GPU colour-id readback). `SceneWidget::pick` unprojects a click to a world
  segment and intersects the scene's own triangles/points;
  `ScalarField3D::pick_cut_plane` / `value_at` pick the cut plane and sample the
  volume; `ScalarFieldView::pick` unifies both into a `FieldPick` (world position
  + field value). The composed `SceneWindow` shows a `ScenePositionInfo` readout
  (port of `PositionInfoWidget`: X/Y/Z/Data) fed by the cursor pick each frame.
- **Documented simplifications**: colormaps are applied on the CPU at
  geometry-build time (not via a GPU colormap texture); silx's per-item
  `_pickFull` richer payloads (data-index/bin resolution, image pixel indices —
  beyond world position + sampled value) and its generic `plot3d._model`
  scene-graph tree editor are deferred, noted in the roadmap rather than stubbed.

### Added — `rsplot` 2D plotting completions (`silx.gui.plot` parity)

The remaining `silx.gui.plot` gaps were closed, finishing the parity port begun
in 0.1.0 / 0.2.0. Tracked row by row in `doc/parity-roadmap.md`.

- **`ImageStack` lazy/threaded loading**: a second mode beside the in-memory
  `set_frames` — `set_sources` + a pluggable `set_loader` (`FrameLoader` trait,
  silx `setUrls`/`setUrlLoaderClass`) load each slot on a background thread, with
  a configurable prefetch radius (`set_n_prefetch`, silx `_preFetch`/`N_PRELOAD`)
  that preloads neighbours as you browse. A concrete `Hdf5FrameLoader` reads 2D
  datasets and 3D-stack slices on demand, and the frame table shows each source
  URL with a per-row remove (silx `UrlList`/`removeUrl`).
- **`CompareImages` SIFT auto-alignment**: the `AUTO` mode registers image B onto
  image A via SIFT keypoints + affine least squares (`core::sift_align` on the
  pure-Rust `lowe-sift` crate, mirroring silx `__createSiftData`→`LinearAlign`),
  with `transformation()` exposing the decomposed affine (silx `getTransformation`),
  an `Align:` status-bar summary, and a toggleable matched-keypoint overlay.
- **`PrintPreview` page editor**: a print-preview window with page layout/scale
  controls (silx `PrintPreviewToolButton`/`PrintPreviewDialog`).
- **Mask file save/load — all five silx formats**: npy, EDF (fabio-style), TIFF,
  HDF5 (with a "Select a 2D dataset" picker, faithful to silx's `"a"` append
  mode), and fit2d `.msk` (byte-verified against fabio), behind native Load/Save
  toolbar actions.
- **Per-pixel image alpha map** (silx `ImageData.setAlphaData`), preserved across
  image re-uploads.
- **Round line joins + caps** for thick polylines (silx line-join rendering).
- Completed interactive 2D tooling: the ruler measurement tool, `PositionInfo`
  live cursor snapping, the `AlphaSlider` active-image binding, the
  `ComplexImageView` amplitude-range dialog, the `StackView` 3D-profile toolbar +
  2D stacked-profile window, the `CompareImages` draggable split separator, the
  `ScatterView` line-/scatter-profile tools, and the ROI manager table widget
  with Arc three-point/polar modes, Band unbounded mode, an interaction-mode
  submenu, save/load dialogs, the `sigRoiAboutToBeRemoved` signal, and concave
  polygon fill via ear-clipping triangulation.
- **Colormap registry**: `register_colormap` for named LUTs and colormap-state
  serialization round-trips.

### Changed — `rsplot`

- New optional-feature-free dependencies scoped to the mask/alignment work:
  `tiff` and the pure-Rust `rust-hdf5` (mask TIFF/HDF5 save/load) and `lowe-sift`
  (CompareImages AUTO alignment). All are plain build-everywhere crates — no
  native `libhdf5` and no Python/OpenCV.

### Note on parity scope

The `silx.gui.plot` parity queue is exhausted. The remaining non-ported items
are documented accepted residuals, not omissions: custom marker fonts (egui
`FontId` cannot express QFont weight/italic without bundled font assets), async
GPU stats/histogram (CPU equivalents are complete), the mid-gesture
`sigInteractiveRoiCreated` signal (immediate-mode emits `DrawingProgress`
instead), runtime matplotlib-dynamic colormap loading (needs a Python
dependency), a native print dialog/printer submission (OS-native; the preview is
ported), and an OpenGL backend selector (N/A — rsplot is wgpu-only).

## [0.2.0] - 2026-06-13

The headline of this release is two new crates — **rsdm** and **adl2rsdm** —
alongside a large expansion of rsplot. rsplot 0.1.0 was the plotting library
alone; 0.2.0 turns the workspace into a full EPICS display stack and a MEDM
screen converter on top of it.

### Added — `rsdm` 0.2.0 (new crate)

A PyDM-style EPICS display layer ported from `pydm` onto rsplot + epics-rs.

- A headless data **`Engine`** owning a tokio runtime, with channel addresses
  over `ca://` (Channel Access), `pva://` (pvAccess), `calc://` (a pure-Rust
  derived-channel expression evaluator), and the IOC-free `loc://` / `fake://`
  schemes. The EPICS backends are feature-gated (`ca`, `pva`, `calc`, all
  default-on); `--no-default-features` gives the dependency-light core.
- The PyDM widget set: `RsdmLabel`, `RsdmLineEdit`, `RsdmEnumComboBox`,
  `RsdmEnumButton`, `RsdmPushButton`, `RsdmSlider`, `RsdmSpinbox`,
  `RsdmByteIndicator`, `RsdmScaleIndicator`, `RsdmDrawing`, `RsdmImage`,
  `RsdmImageView`, `RsdmFrame`, and the rsplot-backed plots `RsdmTimePlot`,
  `RsdmWaveformPlot`, `RsdmScatterPlot`.
- MEDM/PyDM display fidelity: display-format and precision handling, alarm
  severity colouring with selectable MEDM/PyDM palettes, a disconnect-only
  border mode, justified MEDM-cell geometry with vertical/horizontal centering,
  and a single-owner no-local-echo write model (values re-sync from the monitor).
- MEDM Btn2 **middle-click PV-name copy** (clipboard + X11 PRIMARY on Linux),
  matching MEDM/PyDM operator workflows.
- Channel writes go out as plain `CA_PROTO_WRITE` (never `WRITE_NOTIFY`), so a
  busy record can never stall a writer; discarded/failed writes log through the
  `log` facade.

### Added — `adl2rsdm` 0.2.0 (new crate)

A converter mirroring `adl2pydm`, but emitting **compile-checked Rust source**
instead of Qt `.ui` XML.

- Parses a MEDM `.adl` screen into a widget IR and emits a self-contained RsDM
  `Screen` module; the generated code is compile-gated against the real `rsdm`
  API (a fidelity check `adl2pydm` cannot do against Qt).
- Every MEDM widget maps to a real RsDM widget, including arc/polygon/polyline
  shapes, static images, byte/bar/indicator/meter monitors, and the plots.
- Structural z-order: decoration behind, controls on top, pinned by `egui::Order`
  and emitted one placement per child layer so a composite reproduces MEDM
  file order on every layer while staying a transparent group.
- Faithful MEDM rendering: per-widget height-derived fonts, `clr`/`bclr` colours
  reaching widget faces, dynamic-attribute `clr` alarm/discrete colour rules,
  `calc://`-gated visibility rules, uniform `$(macro)` expansion in every string,
  and a responsive (window-filling) layout mode that is the default
  (`--absolute` opts back into fixed MEDM pixels).
- **Recursive related-display conversion**: a related-display button opens the
  converted child screen in an egui viewport, with runtime macro tables built at
  click time (MEDM `relatedDisplayCreateNewDisplay` semantics).
- A `clap` CLI (`--protocol` / `--macro` / `--out` / `--absolute` /
  `--use-scatterplot`) and an installable `adl2rsdm` binary.

### Added — `rsplot` 0.2.0

- **Interactive histogram colorbar** with draggable vmin/vmax handles, an
  auto-range context menu, and an in-chrome gutter rendering for `ImageView` /
  `Plot2D`.
- **Multi-axis Y** (`YAxis::Extra(n)`, N stacked Y axes) with an ergonomic
  `Plot1D` multi-axis API.
- **Time-aware X axis**: DST-correct named-zone offsets, a wall-clock tick mode,
  and an X-axis time offset so relative vertices show absolute ticks.
- **FitWidget**: multi-peak Gaussian fitting with auto peak-search, a background-
  model selector, an editable initial-parameter input, and the full leastsq
  constraint set (FREE/POSITIVE/FIXED/QUOTED/FACTOR/DELTA/SUM/IGNORED).
- **Composite views**: StackView (3D-profile data layer, per-axis calibration,
  per-frame block aggregation), ScatterView (line-profile extraction), and
  CompareImages (RGB composite modes, origin/center/stretch alignment, a
  coordinate/value status bar).
- **Export**: SaveAction gains JPEG, PDF, and EPS raster export (all with
  hand-written encoders, no new dependencies) and a printer-selection Print
  dialog.
- **Save/load**: mask EDF codec and ROI text serialization.
- Toolbar/interaction additions: RulerToolButton + distance core, a
  zoom-enabled-axes menu with box-zoom constraint, pan-with-arrow-keys toggle,
  reusable Profile/Symbol tool buttons, and a LimitsToolBar.
- Scatter IRREGULAR_GRID vertex-indexed mesh with cell picking, plot-wide curve
  style cycling, and live StatsWidget binding across all plot items.

### Changed

- rsplot is now the root of a three-crate workspace; `rsdm` reaches egui/wgpu
  through `rsplot::egui` to keep a single egui/wgpu in the tree.

[Unreleased]: https://github.com/physwkim/rsplot/compare/v0.5.6...HEAD
[0.5.6]: https://github.com/physwkim/rsplot/compare/v0.5.5...v0.5.6
[0.5.5]: https://github.com/physwkim/rsplot/compare/v0.5.4...v0.5.5
[0.5.4]: https://github.com/physwkim/rsplot/compare/v0.5.3...v0.5.4
[0.5.3]: https://github.com/physwkim/rsplot/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/physwkim/rsplot/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/physwkim/rsplot/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/physwkim/rsplot/compare/v0.4.2...v0.5.0
[0.4.2]: https://github.com/physwkim/rsplot/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/physwkim/rsplot/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/physwkim/rsplot/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/physwkim/rsplot/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/physwkim/rsplot/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/physwkim/rsplot/releases/tag/v0.1.0
