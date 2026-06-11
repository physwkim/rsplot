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
  shell-command, embedded display (matching `sidm`'s own deferred set).

## MEDM widget coverage

One row per MEDM widget (the keys of `adl2pydm/symbols.py` `adl_widgets`).
Category drives the z-layer: `static` = decoration (back), `monitor` = read-only
(middle), `controller` = interactive (front).

| MEDM widget | category | SiDM target | status |
|---|---|---|---|
| text | static | `SidmLabel` | ⬜ |
| text update | monitor | `SidmLabel` | ⬜ |
| text entry | controller | `SidmLineEdit` | ⬜ |
| menu | controller | `SidmEnumComboBox` | ⬜ |
| choice button | controller | `SidmEnumButton` | ⬜ |
| message button | controller | `SidmPushButton` | ⬜ |
| valuator | controller | `SidmSlider` | ⬜ |
| wheel switch | controller | `SidmSpinbox` | ⬜ |
| byte | monitor | `SidmByteIndicator` | ⬜ |
| bar | monitor | `SidmScaleIndicator` | ⬜ |
| indicator | monitor | `SidmScaleIndicator` | ⬜ |
| meter | monitor | `SidmScaleIndicator` | ⬜ |
| composite | container | `SidmFrame` (children re-layered inside) | ⬜ |
| rectangle | static | `SidmDrawing(Rectangle)` | ⬜ |
| oval | static | `SidmDrawing(Ellipse)` | ⬜ |
| strip chart | monitor | `SidmTimePlot` | ⬜ |
| cartesian plot | monitor | `SidmWaveformPlot` / `SidmScatterPlot` | ⬜ |
| image | monitor | `SidmImageView` | ⬜ |
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
- ⬜ A3 — `symbols.rs` (MEDM → SiDM map + category + z-layer table).
- ⬜ B4 — `codegen.rs` scaffold + simplest widgets (text / text update / text entry).
- ⬜ B5 — emitter batch: controls (message button, menu, choice button, valuator, wheel switch, byte).
- ⬜ B6 — emitter batch: indicators + shapes (bar/indicator/meter, composite, rectangle/oval).
- ⬜ B7 — emitter batch: plots + image (strip chart, cartesian plot, image).
- ⬜ B8 — stubs + warnings for the deferred 6 + CALC `// TODO` comments.
- ⬜ C9 — CLI (`--protocol` / `--macro` / `--out` / `--use-scatterplot`).
- ⬜ C10 — `tests/compiles.rs` fidelity gate (generated `.rs` `cargo check`s against `sidm`).
- ⬜ C11 — runnable end-to-end example (sample `.adl` + generated `Screen` + tiny `eframe` main).
