# adl2sidm → adl2pydm parity roadmap

Tracks the port of [adl2pydm](https://github.com/BCDA-APS/adl2pydm)
(`~/codes/adl2pydm`, a Python tool converting MEDM `.adl` screens to PyDM `.ui`
files) into the **`adl2sidm`** workspace crate, which instead converts MEDM
`.adl` screens to **SiDM (Rust)** display modules.

`adl2pydm` parses `.adl` into a widget tree and writes PyDM `.ui` (Qt Designer
XML) that PyDM loads at runtime. SiDM has no runtime display loader — SiDM
screens are programmatic Rust structs (an `eframe::App` holding widgets + an
`Engine`) — so `adl2sidm` emits **Rust source** that constructs the equivalent
`sidm` widgets. Because the output is Rust, the generated screen can be
*compile-verified* against the real `sidm` APIs (the `tests/compiles.rs` gate),
a check `adl2pydm` cannot do against Qt.

Plan of record: `~/.claude/plans/deep-growing-balloon.md`.

Status legend: ✅ done · 🚧 in progress · ⬜ planned · ⏸ deferred (tracked, not
dropped).

## Architecture decisions

- **New crate `adl2sidm`** (binary + library), the workspace's third member
  after `siplot` and `sidm`. The converter emits source as text, so it needs no
  GUI/async/EPICS dependencies — only a CLI parser. A dev-dependency on `sidm`
  backs the compile-check fidelity test.
- **Output = generated Rust source**, one module per `.adl` screen: a `Screen`
  struct holding the widgets + `Engine`, a `new(cc: &eframe::CreationContext)`
  builder, and a `ui(&mut self, ui)` draw method. (A runtime display-file format
  + loader is the larger alternative — deferred, matching the `sidm` plan's
  deferral of display loading.)
- **Absolute positioning.** MEDM screens are absolute `x/y/w/h`; each widget is
  placed at its MEDM `Rect` via egui absolute placement inside a fixed-size
  canvas sized to the `display` block. (Proportional/grid scaling — adl2pydm's
  `grid_layout.py` / `use_layout` — is a later optional wave.)
- **Z-order: decoration behind, controls on top.** A hard correctness rule, not
  cosmetics: in egui a later-drawn `Area` renders on top *and captures pointer
  input*, so a MEDM static rectangle over a control would hide it and steal its
  clicks. Within each container, widgets are partitioned by draw category and
  emitted back-to-front — decoration (`static`) → `monitor` → `controller` —
  preserving MEDM order within each category. The category → z-layer table is a
  single owner next to the symbol map.
- **Default channel protocol `ca://`** (MEDM is a Channel Access tool); bare
  MEDM PV names get the prefix. Overridable via `--protocol`; basic `$(macro)`
  substitution via `--macro` (port of adl2pydm `convertMacros`).
- **Deferred** (tracked, not dropped): runtime `.adl`/display-file loader;
  proportional/grid scaling; SiDM-side `DrawingShape` Arc/Pie/Polygon/Polyline;
  a rules engine for MEDM dynamic-attribute CALC; related-display navigation,
  shell-command, embedded display (matching `sidm`'s own deferred set); the MEDM
  `image` static GIF/TIFF-file display (SiDM has only `SidmImageView`, a live
  array-data viewer — no file-image widget to map onto).

## MEDM widget coverage

One row per MEDM widget (the keys of `adl2pydm/symbols.py` `adl_widgets`).
Category drives the z-layer: `static` = decoration (back), `monitor` = read-only
(middle), `controller` = interactive (front).

| MEDM widget | category | SiDM target | status |
|---|---|---|---|
| text | static | `SidmLabel` | ✅ |
| text update | monitor | `SidmLabel` | ✅ |
| text entry | controller | `SidmLineEdit` | ✅ |
| menu | controller | `SidmEnumComboBox` | ✅ |
| choice button | controller | `SidmEnumButton` | ✅ |
| message button | controller | `SidmPushButton` | ✅ |
| valuator | controller | `SidmSlider` | ✅ |
| wheel switch | controller | `SidmSpinbox` | ✅ |
| byte | monitor | `SidmByteIndicator` | ✅ |
| bar | monitor | `SidmScaleIndicator` | ✅ |
| indicator | monitor | `SidmScaleIndicator` | ✅ |
| meter | monitor | `SidmScaleIndicator` | ✅ |
| composite | container | `SidmFrame` (children re-layered inside) | ✅ |
| rectangle | static | `SidmDrawing(Rectangle)` | ✅ |
| oval | static | `SidmDrawing(Ellipse)` | ✅ |
| strip chart | monitor | `SidmTimePlot` | ✅ |
| cartesian plot | monitor | `SidmWaveformPlot` / `SidmScatterPlot` | ✅ |
| image | monitor | ⏸ stub + warning (static GIF/TIFF file, no array channel) | ⬜ |
| arc | static | ⏸ stub + warning (no `DrawingShape::Arc`) | ⬜ |
| polygon | static | ⏸ stub + warning (no `DrawingShape::Polygon`) | ⬜ |
| polyline | static | ⏸ stub + warning (no `DrawingShape::Polyline`) | ⬜ |
| related display | controller | ⏸ disabled `SidmPushButton` (nav deferred) | ⬜ |
| shell command | controller | ⏸ disabled `SidmPushButton` (shell deferred) | ⬜ |
| embedded display | container | ⏸ skip + warning (not in adl2pydm either) | ⬜ |

Dynamic-attribute CALC (visibility/colour rules; adl2pydm `calc2rules.py`):
emitted as a `// TODO: dynamic rule:` comment on the widget — a documented gap,
not silently dropped (SiDM has no rules engine yet).

## Wave / commit log

- ⬜ A1 — workspace member `adl2sidm` scaffold + this roadmap skeleton.
- ✅ A2 — `adl_parser.rs` (block parser + widget-tree IR). Faithful port of
  `adl_parser.py`: line-oriented block/assignment scanning, colour-table
  resolution (`colors` hex list or `dl_color` blocks), geometry, `control`/
  `monitor`/etc. attribute blocks (whose `clr`/`bclr` override the widget colour,
  as in `parseColorAssignments`), `limits` splicing, `points`, recursive
  `composite` children, and indexed `trace`/`pen`/`display`/`command` records.
  6 unit tests; sanity-checked against all 60 real adl2pydm fixtures (no panic).
- ✅ A3 — `symbols.rs` (MEDM → SiDM map + category + z-layer table). `lookup`
  maps every MEDM widget to its SiDM target + a draw `Category`
  (Decoration/Monitor/Control/Container); `Category::z_layer` is the single
  owner of the back-to-front ordering, `has_channel` tells the emitter whether
  to wire a PV. `related display`/`shell command` are typed Control (front) even
  though adl2pydm types them `static`, so a decoration cannot occlude them.
  6 tests (full-coverage of `ADL_WIDGET_SYMBOLS`, z-layer ordering, stub flags).
- ✅ B4 — `codegen.rs` scaffold + simplest widgets (text / text update / text
  entry). Emits the `Screen` struct + `new(cc)` + `ui()` + the absolute `place`
  helper; channel address = `control`/`monitor` `chan` with `$(macro)`
  substitution + protocol prefix; `precDefault` → `.with_precision`; static
  `text` → a fieldless `ui.label`. The z-order is applied as a stable sort by
  `ZLayer` AND per-Area `egui::Order`. Imports are conditional so output is
  warning-clean. 4 codegen tests; the generated screen was smoke-checked to
  `cargo check` clean against real sidm/siplot/eframe (confirming the forked
  `eframe::App::ui(ui, frame)` shape the C11 example will wrap).
- ✅ B5 — emitter batch: controls (message button, menu, choice button, valuator,
  wheel switch, byte). `message button` → `SidmPushButton` (label = MEDM `label`,
  `press_msg`/`release_msg` → press/release values); `menu` → `SidmEnumComboBox`;
  `choice button` → `SidmEnumButton` (`stacking="column"` → horizontal; `row` =
  default vertical); `valuator` → `SidmSlider` (user-defined `*Src="default"`
  limits → `with_limits`, `dPrecision` → `with_precision`, parsed as float to
  match adl2pydm's `1.000000` form); `wheel switch` → `SidmSpinbox` (limits +
  precision from MEDM `format`, falling back to the `limits` block's `precDefault`
  that real wheel-switch screens carry); `byte` → `SidmByteIndicator`
  (`sbit`/`ebit` → `num_bits` = `1+|ebit-sbit|`, `shift` = `min(sbit,ebit)`;
  `direction` `right`/`left` → horizontal). Big-endian display order (`sbit<ebit`)
  has no `SidmByteIndicator` builder yet — reported as a warning, not dropped.
  A single `push_channel_widget` owner emits every channel widget's ctor + field +
  placement, so `let _ = self.wN.show(ui);` and the back-to-front layering are
  uniform. 7 new codegen tests; the full 6-control screen was smoke-checked to
  `cargo check` clean (no warnings) against real sidm.
- ✅ B6 — emitter batch: indicators + shapes (split into B6a/B6b/B6c for the
  composite's nested re-layering).
  - ✅ B6a — scale indicators (`bar`/`indicator`/`meter` → `SidmScaleIndicator`).
    `bar` → `with_bar_indicator(true)` + the MEDM decoration `label` drives the
    value label (PyDM `showValue`: shown only for `limits`/`channel`, vs SiDM's
    show-by-default); `meter` shares the `indicator` (pointer-scale) emitter, as
    adl2pydm's `write_block_meter` does. User-defined limits, `precDefault`, and
    `direction` map to `with_limits`/`with_precision`/`with_orientation`. A single
    `direction_orientation` owner now maps MEDM `direction` → sidm `Orientation`
    for both the scale indicators and `byte` (byte was re-pointed at it, fixing a
    latent mismatch where an unknown direction warned "using right" but left the
    widget vertical). 4 new codegen tests; smoke-checked clean against real sidm.
  - ✅ B6b — shapes (`rectangle` → `SidmDrawing(Rectangle)`, `oval` →
    `SidmDrawing(Ellipse)`). Channel-less decorations use a unique `loc://`
    placeholder (`dynamic_channel`); a `dynamic attribute` `chan` overrides it.
    The `basic attribute` block sets the brush/pen: `fill="solid"` →
    `with_fill(colour)`, `fill="outline"` (MEDM `NoBrush`) →
    `with_fill(Color32::TRANSPARENT)` + a border forced to width >= 1 (as
    adl2pydm does); `width>0` adds `with_border`. `style="dash"` has no
    `SidmDrawing` pen-style builder, so it warns rather than dropping silently.
    A shared `apply_protocol` now backs both `channel_address` and
    `dynamic_channel`. 4 new codegen tests; smoke-checked clean against real sidm.
  - ✅ B6c — `composite` → `SidmFrame` with children re-layered (back-to-front)
    and coordinate-translated to the frame interior. The composite's children are
    emitted by draining the placements the recursion produced
    (`placements.drain(start..)`), re-sorting them by `ZLayer`, and writing each
    inside the frame's `show(ui, |ui| { … })` closure with coordinates translated
    to the frame origin — so the back-to-front rule holds *independently* inside
    every frame, and nesting (composite-in-composite) translates coordinates
    recursively. The frame is a `SidmFrame` on a `loc://` placeholder channel
    (or the composite's own `chan` when set). `ui()` destructures
    `let Self { _engine: _, w0, w1, … } = self;` so a frame closure can borrow
    its sibling fields disjointly (`SidmFrame` paints `Frame::NONE`, so it never
    occludes the children it wraps). 8 new codegen tests, incl. a nested
    composite-in-composite asserting two frames, recursive coordinate
    translation, and the deepest control nested inside both closures; the
    single- and nested-composite screens were generated and `cargo check`'d clean
    (no warnings) against real sidm. Gate: clippy -p adl2sidm clean, nextest
    39/39.
- ✅ B7 — emitter batch: plots (strip chart, cartesian plot). `strip chart` →
  `SidmTimePlot`, one `add_channel` per MEDM `pen`; `period` scaled by `units`
  (`minute`→60, `hour`→3600) sets `with_time_span` (converting MEDM's unit-tagged
  period to sidm's seconds, where adl2pydm passes it through raw). `cartesian
  plot` → `SidmWaveformPlot` (default) or `SidmScatterPlot` (`--use-scatterplot`):
  each `trace` is a curve. Waveform — a trace needs `ydata` (else skipped, as
  adl2pydm requires a `y_channel`); `xdata` present → `add_xy_channel(y, Some(x))`,
  absent → `add_channel(y)` (Y vs index). Scatter — a trace needs *both* `xdata`
  and `ydata` (sidm's scatter pairs two scalar channels in `(x, y)` order); a
  trace missing either is warned and skipped, and `count` maps to the scatter
  buffer size (waveform has no per-curve budget, so `count` is dropped there).
  Pen/trace colours resolve from `clr`/`data_clr` against the table. A new
  `push_plot_widget` owner emits the `let mut <field> = …::new(rs, <PlotId>)…;`
  constructor plus a follow-up `add_*` per curve and the back-to-front placement,
  so plots layer uniformly with the other widgets; each plot gets a distinct
  `PlotId`. 4 new codegen tests (strip-chart pens + unit-scaled span; waveform
  x/y vs y-only traces, count dropped; scatter buffer + (x,y) order + missing-x
  skip-warning; both plots Middle-layer with distinct ids). The waveform- and
  scatter-mode screens were generated and `cargo check`'d clean (no warnings)
  against real sidm. Gate: clippy -p adl2sidm clean, nextest 43/43.
  - **`image` moved to the B8 stub set.** The plan slotted `image →
    SidmImageView` here, but the MEDM `image` widget is a *static GIF/TIFF file*
    display (`type="gif"`, `"image name"="apple.gif"`) with no channel, whereas
    `SidmImageView` is a live array-data viewer that *requires* an
    `image_address` channel. There is no faithful mapping — forcing one would
    fabricate a channel that the `.adl` never names — so `image` becomes a
    stub + warning alongside the deferred 6, not a plot emitter. (`image` still
    warns through the default dispatch arm until B8 lands its dedicated stub.)
- ⬜ B8 — stubs + warnings for the deferred 6 + `image` + CALC `// TODO` comments.
- ⬜ C9 — CLI (`--protocol` / `--macro` / `--out` / `--use-scatterplot`).
- ⬜ C10 — `tests/compiles.rs` fidelity gate (generated `.rs` `cargo check`s against `sidm`).
- ⬜ C11 — runnable end-to-end example (sample `.adl` + generated `Screen` + tiny `eframe` main).
