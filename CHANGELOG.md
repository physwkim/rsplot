# Changelog

All notable changes to this workspace are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
pre-1.0 [Semantic Versioning](https://semver.org/) (a `0.x` minor bump may carry
breaking changes).

This is a workspace of three crates released together: **siplot** (the plotting
library), **sidm** (a PyDM-style EPICS display layer built on siplot), and
**adl2sidm** (a MEDM `.adl` → SiDM-Rust-source converter).

## [Unreleased]

### Added — `siplot` 3D scene subsystem (`silx.gui.plot3d` port)

A full true-3D scene stack ported from `silx.gui.plot3d` onto siplot's
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

## [0.2.0] - 2026-06-13

The headline of this release is two new crates — **sidm** and **adl2sidm** —
alongside a large expansion of siplot. siplot 0.1.0 was the plotting library
alone; 0.2.0 turns the workspace into a full EPICS display stack and a MEDM
screen converter on top of it.

### Added — `sidm` 0.2.0 (new crate)

A PyDM-style EPICS display layer ported from `pydm` onto siplot + epics-rs.

- A headless data **`Engine`** owning a tokio runtime, with channel addresses
  over `ca://` (Channel Access), `pva://` (pvAccess), `calc://` (a pure-Rust
  derived-channel expression evaluator), and the IOC-free `loc://` / `fake://`
  schemes. The EPICS backends are feature-gated (`ca`, `pva`, `calc`, all
  default-on); `--no-default-features` gives the dependency-light core.
- The PyDM widget set: `SidmLabel`, `SidmLineEdit`, `SidmEnumComboBox`,
  `SidmEnumButton`, `SidmPushButton`, `SidmSlider`, `SidmSpinbox`,
  `SidmByteIndicator`, `SidmScaleIndicator`, `SidmDrawing`, `SidmImage`,
  `SidmImageView`, `SidmFrame`, and the siplot-backed plots `SidmTimePlot`,
  `SidmWaveformPlot`, `SidmScatterPlot`.
- MEDM/PyDM display fidelity: display-format and precision handling, alarm
  severity colouring with selectable MEDM/PyDM palettes, a disconnect-only
  border mode, justified MEDM-cell geometry with vertical/horizontal centering,
  and a single-owner no-local-echo write model (values re-sync from the monitor).
- MEDM Btn2 **middle-click PV-name copy** (clipboard + X11 PRIMARY on Linux),
  matching MEDM/PyDM operator workflows.
- Channel writes go out as plain `CA_PROTO_WRITE` (never `WRITE_NOTIFY`), so a
  busy record can never stall a writer; discarded/failed writes log through the
  `log` facade.

### Added — `adl2sidm` 0.2.0 (new crate)

A converter mirroring `adl2pydm`, but emitting **compile-checked Rust source**
instead of Qt `.ui` XML.

- Parses a MEDM `.adl` screen into a widget IR and emits a self-contained SiDM
  `Screen` module; the generated code is compile-gated against the real `sidm`
  API (a fidelity check `adl2pydm` cannot do against Qt).
- Every MEDM widget maps to a real SiDM widget, including arc/polygon/polyline
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
  `--use-scatterplot`) and an installable `adl2sidm` binary.

### Added — `siplot` 0.2.0

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

- siplot is now the root of a three-crate workspace; `sidm` reaches egui/wgpu
  through `siplot::egui` to keep a single egui/wgpu in the tree.

[Unreleased]: https://github.com/physwkim/siplot/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/physwkim/siplot/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/physwkim/siplot/releases/tag/v0.1.0
