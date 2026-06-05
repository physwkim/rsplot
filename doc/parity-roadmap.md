# siplot → silx parity roadmap

Generated from an 11-agent parity sweep of `silx.gui.plot` against the
siplot implementation. Scope (per project decision): **silx.gui.plot +
adjacent silx.gui** (colors, data-adjacent GUI widgets).

**Totals (as-of-sweep):** 397 features — 165 Done, 49 Partial, 183 Missing.
**Re-baselined 2026-06-04 (main @ `d04232a`):** ≈191 Done · 130 open (10 H / 48 M /
34 L) — see [Remaining work (re-baselined 2026-06-04)](#remaining-work-re-baselined-2026-06-04-main--d04232a).
**After Wave 13 (ROI styling, main @ `7495590`, not pushed):** ≈199 Done · 122 open (7 H / 44 M / 33 L) — 8 ROI
rows closed (per-instance color/name/selection, `sigCurrentRoiChanged`, line style/width, manager default-color, fill).

**After Wave 14 (item-click + hover + marker-drag triad, main, not pushed):** ≈206 Done · 115 open (6 H / 42 M / 29 L)
— 7 rows closed: curveClicked (H), curveClicked/imageClicked (L), hover-with-metadata (M), ItemsInteraction picker/
state-machine (M), marker drag-start/end triad (L), markerMoving/markerMoved split (L). `ItemHovered` carries the full
silx `prepareHoverSignal` payload (`selectable` omitted as structurally constant); the marker drag now emits
`MarkerDragStarted`→`MarkerMoved`×N→`MarkerDragFinished` (silx `beginDrag`/`drag`/`endDrag`).

**After Wave 15 (PlotWidget interaction-events + panzoom + draw feedback, main, not pushed):** ≈212 Done · ~109 open
— 6 rows closed: `LimitsChanged` carries the new `{x,y,y2}` ranges (row 61), RoiCreate draw events surface as
`DrawingProgress`/`DrawingFinished` (row 57), axis-constraint adaptive normalization mirroring silx `ViewConstraints`
allow_scaling=True — max-range capped to the position window + a wider-than-window view snaps to it (row 62),
box-zoom and draw previews unified onto one selection-overlay renderer with silx Zoom's `fill="none"` dashed outline
(row 60), the polygon first-point close target box (row 66), and the pencil brush-footprint preview circle (row 65).
Rows 49 / 63 (overlay-only replot, StateMachine base) and their re-baseline duplicates resolved as **N/A** — functional
parity achieved differently (immediate-mode + persistent GPU buffers make redraw always-on and uploads event-driven;
imperative `apply_interaction` dispatch replaces the silx state-machine hierarchy). This closes the enumerated
interaction-events-panzoom open rows.

**Mask drawing tools + colormap autoscale (main, not pushed):** ≈216 Done · ~105 open — 3 H rows closed: on-plot
Rectangle (row 126), Ellipse (row 128), and Polygon (row 127) mask draws; plus the ColormapDialog Stddev3/Percentile
autoscale wired to raw pixels (row 133, ◐→✅). The pure fill primitives (`update_rectangle`/`update_ellipse`/
`update_polygon`) already existed; the gap was the on-plot draw interaction. Wired high-level like the pencil
(reusing `DrawState`/`feed_draw_state`/the extracted `paint_draw_preview`, no new interaction mode or `PlotEvent`):
`MaskTool::draw_mode` is the single source of truth for which tools are shape draws; `MaskToolsWidget::handle_shape_draw`
feeds the draw machine and, on finish, `fill_from_draw` converts data→cells (silx `int()`/`astype(int64)` truncation,
`_plotDrawEvent`) and masks the level; `ImageView::handle_mask_shape_draw` paints the rubber-band preview.

**ROIStatsWidget display (main, not pushed):** ≈217 Done · ~104 open — 1 H row closed: the per-ROI statistics table
(row 111). `RoiStatsWidget` renders one row per ROI (`ROI | N | min | max | mean | sum | integral`) over the active
item; `PlotWidget::feed_roi_stats`/`show_roi_stats_widget` reduce each ROI via the pre-existing
`image_roi_stats`/`curve_roi_stats` and follow the active item + live ROI list. Pure row-building (`roi_stats_rows`)
is headlessly tested (image/curve per-ROI, empty); the active-item feed + render stay GPU-unverified. The
`high_level_roi_stats` example now uses the widget instead of a hand-rolled bbox reduction.

**CurvesROIWidget integration (main, not pushed):** ≈218 Done · ~103 open — the last open H row closed: per-curve ROI
stats (row 117). `CurvesRoiWidget` shows one row per x-span ROI over the active curve
(`ROI | From | To | Raw Counts | Net Counts | Raw Area | Net Area`); `PlotWidget::feed_curves_roi_stats`/
`show_curves_roi_widget` reduce each ROI via the new pure `curve_roi_counts`, faithful to silx
`computeRawAndNetCounts`/`computeRawAndNetArea` (array-order selection — numpy does not sort; linear-endpoint
background for net counts; numpy-trapezoid for area; NaN-y summed as-is, unlike `curve_roi_stats`). Pure core
(`curve_roi_counts`, 7 tests) + row-building (`curve_roi_rows`, 2 tests) are headlessly tested; the active-curve feed
+ egui render stay GPU-unverified. New example `high_level_curves_roi`. No open H rows remain in the primary table.

**Histogram bin alignment (main, not pushed):** ≈219 Done · ~102 open — 1 M row closed (row 108, ◐→✅). `HistogramAlign`
{Left,Center,Right} + the pure `histogram_edges(positions, align)` mirror silx `items.histogram._computeEdges`
(`Histogram.setData(align=)`): Left treats positions as left edges (append `x[-1]+last_gap`), Right as right edges
(prepend `x[0]-first_gap`), Center right-aligns then shifts each edge left by half its following gap so positions land
at bin centres; a lone position uses silx's unit-gap fallback. Public `PlotWidget::add_histogram_aligned`/`_with_legend`
take N positions + N counts + an alignment. The pure edge derivation is headlessly tested per alignment (incl.
single-position, non-uniform centre, empty, and the edges→step-values composition); the GPU add stays unverified.

**StackView perspective + 3D transposition (main, not pushed):** ≈221 Done · ~100 open — 2 M rows closed (182, 183;
duplicates 1233, 1237). The silx `StackView.py` port (`high_level.rs`, frames as `Vec<Vec<f32>>`) gains volume
browsing: `StackPerspective` {Axis0,Axis1,Axis2}, the pure `stack_frame`/`stack_frame_count` slice a 2D frame out of a
row-major `[d0,d1,d2]` volume per perspective (matching silx `__createTransposedView` transpose (1,0,2)/(2,0,1)), and
`dimension_axis_labels` picks X=width-axis / Y=height-axis labels (silx `__updatePlotLabels`). `StackView::set_volume`
keeps the volume and re-slices on `set_perspective`; `perspective_ui` is the browse-dimension combo. Pure core
headlessly tested (per-perspective slicing, frame counts, display-axis mapping, length/range rejection); the
volume-load + perspective-switch GPU path stays unverified. Default dimension labels only — the `setLabels` API (row
184) is the next commit.

**StackView dimension labels (main, not pushed):** ≈222 Done · ~99 open — 1 M row closed (184; duplicate 1267).
`StackView::set_dimension_labels([&str;3])`/`dimension_labels()` mirror silx `setLabels`/`getLabels`: each volume
dimension gets a label (empty → default `"Dimension N"`, like silx's `label or default`), and `apply_axis_labels`
chooses the X (width-axis) and Y (height-axis) label from these as the perspective rotates. `perspective_ui` names the
browse-dimension combo entries from the same labels. The pure default-label → axis-label mapping is headlessly tested;
the `set_dimension_labels` GPU label-application path stays unverified.

**ScatterView selection-mask accessors (main, not pushed):** ≈223 Done · ~98 open — 1 M row closed (198, ◐→✅;
duplicate 1273). `ScatterView::selection_mask() -> &[u8]` and `set_selection_mask(&[u8]) -> Result<usize, _>` mirror
silx `ScatterView.getSelectionMask`/`setSelectionMask`: per-point u8 levels (0 unmasked, 1..=255 a level). `set`
validates the length equals the current point count (silx raises ValueError on shape mismatch), copies the levels, and
commits to the mask's undo history. The mask is a selection query (not a render overlay), so no re-render is needed.
The accessors are thin wrappers over the already-tested `ScatterMaskWidget`; constructing a `ScatterView` needs a wgpu
`RenderState`, so the wrapper's length-check path itself stays GPU-unverified.

**ScatterView position-info panel (main, not pushed):** ≈224 Done · ~97 open — 1 M row closed (205, ◐→✅; duplicate
1261). `ScatterView::show_position_info(ui, &PlotResponse)` embeds the existing `PositionInfo` widget with the silx
X/Y/Data/Index columns (ScatterView.py:90-101). The pure `scatter_pick_pixels(cursor, points_px, radius)` picks the
nearest scatter point in **pixel** space (so the snap radius is constant on screen at any zoom; ties → highest/top-most
index, silx `_pickScatterData`), and `scatter_position_info(Option<ScatterPick>)` builds the bar: X/Y/value/index snap
to the pick, or X/Y show the bare cursor and Data/Index show "-" when nothing is within `SCATTER_PICK_RADIUS_PX`
(= default marker size). The cursor is tracked from the plot's pointer event (silx `sigMouseMoved`). Pure pick + column
builder headlessly tested (nearest/none/tie, snap/fallback/placeholder); the transform-projection + render path stays
GPU-unverified.

Status legend: ✅ Done · ◐ Partial · ☐ Missing · ✅ N/A (resolved: parity achieved differently). Effort S/M/L. Priority H/M/L.

> This file tracks the port. The per-area tables below are the **as-of-sweep
> baseline**; landed work is recorded in the Progress log so the baseline stays
> a stable reference. Follow-ups that a wave deliberately deferred are listed too.

## Remaining work (re-baselined 2026-06-04, main @ d04232a)

These figures supersede the older **"Remaining work (live, code-verified
2026-06-02)"** view, which was frozen before Waves 10–12 and the ~18 commits that
landed after the Wave-12 progress-log entry (detached windows; Arc/Band/Circle/
Ellipse/Rect handle-drag editing; the `apply_curve_alpha` double-premultiply fix;
the R2.1 mid-drag mode-switch fix; legend icon styling; active-curve axis labels;
right-click zoom menu; vertical-colorbar/rotated-Y-label centering fixes). An
8-slice read-only recon (workflow `wf_b2b09dc2-620`) re-derived **every** status
from the code on `main` rather than trusting the stale status column:
**191 features Done** (counts-based, approximate) vs **130 still open** (Partial +
Missing, deduped; four stale "Missing" interaction rows dropped because their own
evidence shows them implemented — log-aware pan/zoom, float32-safe limits, overflow
protection). The tables below enumerate the **92 explicitly-tracked open rows** with
a priority (10 H / 48 M / 34 L), each traced to a silx ref; the climb to 130 comes
from per-slice tallies that include un-enumerated lower-detail gaps. P = priority,
E = effort. The frozen per-area baseline tables further down are kept only as the
as-of-sweep reference.

### core-axes-ticks

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ☐ Missing | M | M | Per-axis timezone support | items/axis.py:278-294 (get/setTimeZone) | No timezone field on Plot/axis; `dtime_ticks.rs` is UTC-only, no `setTimeZone` API |
| ✅ N/A | L | M | Overlay-only replot optimization | PlotWidget.py:719-730 (`_setDirtyPlot`, overlay vs True) | Resolved W15 — not a gap. silx's `_dirty` defers redraws for a *retained* backend; siplot is immediate-mode with persistent GPU buffers: curve/image data uploads only on add/update (event-driven), the per-frame `prepare` only re-decimates a *changed* view + writes cheap uniforms, `paint` draws from persistent buffers. There is no per-frame full upload to skip; a `dirty()` gate would be speculative + can't skip egui's frame paint. `DirtyState`/`dirty()` remain as a faithful silx-concept port (set, not consumed in production) |

### interaction-events-panzoom

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✔ Done (W14) | H | M | Specific item-click signals (curveClicked/markerClicked/imageClicked) | PlotInteraction.py:1223-1261, PlotEvents.py:88-173 | `PlotEvent::{CurveClicked,ImageClicked,ItemClicked}` carry handle + position + button via the `pick_topmost`/`click_event_for_pick` owner path |
| ✔ Done (W14) | M | M | Hover event signals with item metadata | PlotInteraction.py:1135-1154, PlotEvents.py:73-85 | `PlotEvent::ItemHovered{handle,kind,label,x,y,xpixel,ypixel,draggable}` mirrors silx `prepareHoverSignal`. silx `selectable` omitted by design: every pickable item is set-active on click in siplot, so the flag is structurally constant `true` (uninformative) |
| ✔ Done (W15) | M | S | DrawingProgress / DrawingFinished event wiring | PlotInteraction.py:529-532, PlotEvents.py:34-55 | `apply_interaction` surfaces the RoiCreate `DrawEvent` on `PlotResponse.draw_event`; `high_level.rs` emits `PlotEvent::DrawingProgress{mode,points}` / `DrawingFinished{mode,params}` (the latter alongside `RoiCreated`) |
| ✔ Done (W14) | M | M | ItemsInteraction state machine (item picking/dragging) | PlotInteraction.py:1115-1350 | The pickers are wired through one owner `pick_topmost`: click → `Curve/Image/ItemClicked` (silx `_handleClick`), bare hover → `ItemHovered` (silx Idle), and a draggable-marker drag runs the `MarkerDragStarted`→`MarkerMoved`→`MarkerDragFinished` lifecycle (silx `beginDrag`/`drag`/`endDrag`) |
| ✔ Done (W14) | L | S | Marker drag-finished vs drag-moving signal split | PlotInteraction.py:1276-1299, 1350 | Split: `MarkerMoved` per frame (silx `markerMoving`) + distinct on-release `MarkerDragFinished` (silx `markerMoved`), bracketed by `MarkerDragStarted` |
| ✅ Done | L | S | Selection-area color/fill-mode styling | PlotInteraction.py:98-141 | W15: box-zoom rubber band and draw preview share one `draw_selection_polygon` renderer honoring `FillMode` (Hatch/Solid/None) + color. Box-zoom now renders silx Zoom's `fill="none"` dashed outline instead of a hardcoded solid rect (`plot_widget.rs`). Public per-mode color setter not exposed (no consumer) |
| ✔ Done (W15) | L | S | Limits-changed event with actual range values | PlotEvents.py:176-184 | `PlotEvent::LimitsChanged{x,y,y2}` now carries the new left-axis `(min,max)` ranges and the optional right axis, built from the post-change snapshot in `push_limits_changed_if` |
| ✅ Done | L | M | Axis constraints (min/maxXRange, min/maxYRange) | panzoom.py:222-366 | `AxisConstraints::apply` mirrors silx `ViewConstraints` (allow_scaling=True): max-range capped to the position window (`update` sanity), and a view wider than the window snaps to it (`normalize` adaptive expansion) — W15, `core/plot.rs` |
| ✅ N/A | L | M | StateMachine base (ClickOrDrag hierarchy) | Interaction.py:87-198 | Resolved — not a gap. silx's `StateMachine`/`ClickOrDrag` class hierarchy is an implementation structure, not a user-facing feature; siplot achieves the same interaction behavior with `DrawState` + the imperative `apply_interaction` dispatch (functional parity achieved differently) |
| ✔ Done (W14) | L | S | Marker drag-start/end callbacks (clicked/moving/moved triad) | PlotInteraction.py:1223-1350 | Full lifecycle: `MarkerDragStarted` (begin) → `MarkerMoved`×N (moving) → `MarkerDragFinished` (on-release moved); marker click arrives via `ItemClicked` from the unified picker |
| ✅ Done | L | S | Pencil preview circle during freehand draw | PlotInteraction.py:955-1110 (`updatePencilShape`) | W15: `ImageView::draw_brush_preview` paints the silx pencil footprint — an unfilled circle of radius `brush_size/2` (13 pts, `pencil_preview_circle`) at the cursor in MaskDraw mode, idle + painting (`high_level.rs`, `mask_tools.rs`) |
| ✅ Done | L | S | Cursor snap indicator in polygon mode | PlotInteraction.py:485-621 | W15: `draw_polygon_first_point` renders silx's first-point close target — an unfilled box of half-size `close_threshold_px` around the first vertex — throughout the polygon draw (RoiCreate + `show_with_draw`); `plot_widget.rs`, `interaction.rs::close_threshold_px()` |

### items

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✅ Done | M | M | Histogram bin alignment (left/center/right) | histogram.py:53-85 (`_computeEdges`, `setData(align=)`) | W15: `HistogramAlign` {Left,Center,Right} + pure `histogram_edges(positions, align)` mirror silx `_computeEdges` (Left=positions are left edges, append `x[-1]+last_gap`; Right=right edges, prepend `x[0]-first_gap`; Center right-aligns then shifts each edge left by half its following gap; lone position uses unit gap). Public `PlotWidget::add_histogram_aligned`/`_with_legend` take N positions + N counts + align. 7 headless tests (each align, single-position unit gap, non-uniform center, empty, step-value composition). The GPU add itself stays unverified |
| ◐ Partial | M | L | Scatter mode-specific picking | scatter.py:804-860 | Wave 7: `regular_grid_pick` (REGULAR_GRID cell→index by major order, `GridImage::cell` handles negative scale) and `BinnedStatistic::pick` (BINNED_STATISTIC bin→member indices, exclusive upper edges) added. Points/Solid use plain nearest-point picking (silx default `super().pick()`). Remaining: IRREGULAR_GRID picking — siplot's interpolated-image render has no 1:1 cell→point mapping (silx picks Delaunay triangles `//4`) |
| ◐ Partial | M | M | Image per-pixel validity mask before upload | items/image.py:209-251 | `ScalarMask` + Plot2D wiring (`8ef46c2`) exist; pre-upload mask application in the GPU render path still pending |
| ◐ Partial | M | L | Marker custom-callback constraint | items/marker.py:208-235 | H/V presets wired (Wave 11); arbitrary `setConstraint(fn)` callback form unsupported |
| ✅ Done | M | M | Histogram filled-region picking | histogram.py:244-290 (`__pickFilledHistogram`) | Wave 7: pure `pick_histogram(edges, values, baseline, x, y) -> Option<usize>` — strict bbox gate (x over edge span, y over `[min(0,vmin), max(0,vmax)]`), `searchsorted(edges, x, side="left")-1` (clamped) bin index via `partition_point`, inclusive y test against the bar `[baseline, value]`/`[value, baseline]`. None on malformed/all-NaN/out-of-box/no-hit |
| ☐ Missing | M | L | Image per-pixel alpha map | items/image.py:462-500 (get/setAlphaData) | `ImageData` has only global `alpha:f32`; no per-pixel alpha array (needs shader) |
| ◐ Partial | L | L | Marker text anchor/alignment | items/marker.py | `TextAnchor` enum defined but egui text drawn at a fixed offset; anchor not wired to render |
| ☐ Missing | L | M | Shape overlay-flag separate data layer | items/shape.py:54-73 (`isOverlay`) | `shape.is_overlay` ignored; all shapes draw in one overlay pass |
| ☐ Missing | L | L | ImageStack lazy URL/HDF5 loading | items/image.py:593-669 | Pre-loaded in-memory only; no URL/HDF5 lazy load or prefetch (out of declared scope) |
| ☐ Missing | L | L | CompareImages SIFT keypoint alignment | CompareImages.py | Basic modes only; no SIFT/affine alignment (out of scope) |

### roi

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✅ Done | H | S | Per-instance ROI color on the **live** plot | items/_roi_base.py:389-405 | Wave 13: unified `plot.rois` to `Vec<ManagedRoi>` (single source of truth); the live render `plot_widget.rs → chrome::draw_rois` resolves each ROI's color as `managed.color.unwrap_or(plot.roi_color)` via pure `chrome::roi_appearance`. On-screen pixels GPU/PlotWidget-UNVERIFIED; appearance resolution headlessly tested |
| ✅ Done | H | M | ROI label/name text on the live plot | items/_roi_base.py:492-511 (`_updateText`) | Wave 13: `ManagedRoi.name` plumbed through `chrome::roi_appearance.name` into the live `draw_rois` path (empty name → no label). Same single-collection fix as the color row |
| ✅ Done | H | S | ROI selection/highlight on the live plot | tools/roi.py:528-590 (`setHighlighted`) | Wave 13: `Plot::set_current_roi` (sole owner of `ManagedRoi.selected`) drives the live highlight; `draw_roi` thickens the current ROI's stroke. Invariant (exactly one selected; out-of-range clears; remove adjusts index) headlessly tested |
| ✅ Done | H | L | CurvesROIWidget integration | CurvesROIWidget.py:62-400 | W15: `CurvesRoiWidget` shows a per-ROI table (`ROI \| From \| To \| Raw Counts \| Net Counts \| Raw Area \| Net Area`) over the active curve; `PlotWidget::feed_curves_roi_stats`/`show_curves_roi_widget` reduce each x-span ROI via the pure `curve_roi_counts` (silx `computeRawAndNetCounts`/`computeRawAndNetArea`: array-order selection, linear-endpoint background, numpy-trapezoid). Pure core (`curve_roi_counts`) + row-building (`curve_roi_rows`) headlessly tested; the active-curve feed + render stay GPU-unverified. Example `high_level_curves_roi` |
| ✅ Done | H | L | ROIStatsWidget display | ROIStatsWidget.py | W15: `RoiStatsWidget` renders one row per ROI (`ROI \| N \| min \| max \| mean \| sum \| integral`) over the active item; `PlotWidget::feed_roi_stats`/`show_roi_stats_widget` reduce each ROI via `image_roi_stats`/`curve_roi_stats` and follow the active item + live ROI list. Row-building (`roi_stats_rows`) headlessly tested (image/curve per-ROI, empty); the active-item feed + render stay GPU-unverified. Example `high_level_roi_stats` uses it |
| ◐ Partial | M | M | EllipseROI orientation/rotation | items/roi.py:875-1177 | Axis-aligned only; missing rotational geometry / `_orientation` field |
| ◐ Partial | M | M | CircleROI dedicated handle UI | items/roi.py:727-950 | On-plot creation done; editing is whole-ROI translate + center/radius map, no dedicated handle UI |
| ◐ Partial | M | M | CrossROI marker symbols | items/roi.py:133-185 | Creation works; missing marker-symbol control + composite handle/label management |
| ◐ Partial | M | L | ArcROI interaction sub-modes | items/_arc_roi.py | Polar editing present; silx default ThreePointMode sub-mode + toggle UI missing |
| ✅ Done | M | S | Manager `sigCurrentRoiChanged` signal | tools/roi.py:346-347 | Wave 13: `PlotEvent::CurrentRoiChanged { previous, current }` emitted by `set_current_roi` only when the selection actually changes (mirrors silx `sigCurrentRoiChanged`) |
| ☐ Missing | M | M | ROI handle-symbol customization | items/_roi_base.py:600-680 | All handles render as fixed 6px squares; no `+` center / `s` vertex / `o` close-polygon glyphs |
| ✅ Done | M | M | ROI line style (dash/dot) on canvas | items/_roi_base.py (LineMixIn) | Wave 13: `ManagedRoi.line_style` (`RoiLineStyle::{Solid,Dashed,Dotted}`) resolved into `RoiAppearance.line_style` and drawn by the live path; manager combo edits it |
| ✅ Done | M | S | ROI line width on canvas | items/_roi_base.py:245 | Wave 13: `ManagedRoi.line_width` resolved into `RoiAppearance.line_width` on the live `draw_rois` path (was hardcoded 1.0pt); manager drag-value edits it |
| ✅ Done | M | S | Manager default-color application | tools/roi.py:782-797 | Wave 13: added `Plot::roi_color` (silx-default red) applied as the per-ROI fallback; manager color buttons now write `managed.color` and apply on the live plot |
| ☐ Missing | M | M | ROI edge-position constraints | items/_roi_base.py + per-class | `move_edge` does basic min/max clamping only; no inner>outer prevention, ratio lock, snapping |
| ◐ Partial | L | L | BandROI edge constraints/collision | items/_band_roi.py | Geometry + corner handles present; missing edge-snapping, collision detection, rotation sub-mode |
| ◐ Partial | L | M | ROI creation preview polish | tools/roi.py:493-510 | Live overlay present; missing polygon close-handle indicator + mode-label overlay during creation |
| ✅ Done | L | S | ROI fill enable/disable toggle | items/roi.py:531-539 | Wave 13: `ManagedRoi.fill` plumbed to `RoiAppearance.fill`; `set_roi_fill` API + manager checkbox toggle the interior fill on the live plot (silx `setFill`) |
| ☐ Missing | L | S | Distinct Horizontal/VerticalLineROI kinds | items/roi.py:366-510 | Uses HRange/VRange only; no single-Y/single-X spanning-line ROI with X/YMarker |
| ☐ Missing | L | M | ROI right-click context menu | tools/roi.py:625-642 (`_feedContextMenu`) | No remove / mode-select right-click menu |
| ☐ Missing | L | L | ROI save/load from file | CurvesROIWidget.py:194-210 (dictdump) | No `Serialize` on `Roi`; no file I/O in the manager |
| ☐ Missing | L | M | ROI keyboard shortcuts + naming UI | tools/roi.py mode actions | No shortcut bindings (e.g. R for Rect) or named-creation dialog |

### colormap-colorbar-mask

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✅ Done (W15) | H | M | ColormapDialog Stddev3/Percentile from raw pixels | ColormapDialog.py:240-280 | `ColormapDialog::apply` now autoscales from the active image's raw pixels (`Plot2D::get_image_pixels_raw`) via the pure `autoscale_range` (delegates to `AutoscaleMode::range` with the dialog's mode + percentiles) — Stddev3 (mean ± 3·std clamped to data range) and Percentile compute the real distribution range instead of falling back to MinMax; falls back to aggregated stats min/max only when no raw scalar pixels are retained |
| ✅ Done (W15) | H | M | Mask drawing tool: Rectangle | MaskToolsWidget.py:805-826 | On-plot rectangle draw: `MaskTool::draw_mode` arms a `DrawState` driven by `handle_shape_draw`; `rect_params_to_cells` (silx `int()` truncation) → `update_rectangle` on finish; `ImageView::handle_mask_shape_draw` paints the rubber-band preview (reuses `paint_draw_preview`/`feed_draw_state`) |
| ✅ Done (W15) | H | M | Mask drawing tool: Polygon | MaskToolsWidget.py:840-847 | On-plot polygon draw (click-to-add, snap-close) via the shared shape-draw wiring: `MaskTool::draw_mode` arm + `polygon_vertices_to_cells` (cast int64, swap `(x,y)`→`(row,col)` per silx `[:, (1,0)]`) → `update_polygon`/`polygon_fill_mask` on finish |
| ✅ Done (W15) | H | L | Mask drawing tool: Ellipse | MaskToolsWidget.py:828-838 | On-plot ellipse draw via the shared shape-draw wiring: `MaskTool::draw_mode` arm + `ellipse_params_to_cells` (center cast int64, y-semi→`radius_r`, x-semi→`radius_c`; radii stay float per silx) → `update_ellipse` on finish |
| ☐ Missing | M | M | Colormap catalog: matplotlib-dynamic loading | colors.py:938-955 | Fixed 15-entry catalog; silx loads all matplotlib colormaps dynamically at runtime |
| ✅ Done | M | M | ColormapDialog histogram display | ColormapDialog.py:1227-1295 (`computeHistogram`), :831-848 (display) | Wave 6: pure `compute_histogram` (silx `nbins=clamp(2,min(256,⌊√N⌋))`, finite-min/max or supplied range, log-space binning with edges → linear) + `set_histogram`/`histogram`/`clear_histogram` (silx `setHistogram`/`getHistogram`). Dialog auto-derives from the active image's raw pixels (recompute only on (re)open / normalization change); `draw_histogram_panel` renders normalized gray bars + a colormap gradient strip across `[vmin,vmax]` + vmin/vmax markers. Pure compute + API headlessly tested; on-screen panel GPU-unverified |
| ✅ Done | M | M | Mask threshold UI (below/between/above) | _BaseMaskToolsWidget.py:265-294, :848-937, :1182-1225 | Wave 7: threshold row in the ImageView mask-draw toolbar — `ThresholdMode` selector + min/max fields shown per mode (min for below/between, max for between/above, silx minLineEdit/maxLineEdit visibility) + `Mask below/between/above` Apply button calling the tested `update_threshold` (below→min, above→max, between→both) at the current level, commit + re-upload. `MaskToolsWidget` holds `threshold_mode`/`threshold_min`/`threshold_max` (silx group defaults: below checked, edits at 0; unit-tested). Toolbar row GPU-unverified |
| ◐ Partial | M | L | Mask file save (npy/edf/tif/h5/msk) | MaskToolsWidget.py:104-141 | `.npy` encode/decode exists; no file dialog and missing EDF/TIFF/HDF5/msk formats |
| ☐ Missing | M | L | Mask file load (npy/edf/tif/h5/msk) | MaskToolsWidget.py:589-629 | No load dialog or logic; silx has format filter + auto-detect + HDF5 dataset selection |
| ☐ Missing | M | S | Mask transparency/alpha slider UI | _BaseMaskToolsWidget.py:554-577 | `alpha` field exists (0.8 default) but no slider UI to control overlay opacity |
| ☐ Missing | M | S | Mask Mask/Unmask toggle + Ctrl modifier | _BaseMaskToolsWidget.py:790-810 | Pencil/eraser are separate tools; no Mask/Unmask radios or Ctrl-modifier toggle |
| ◐ Partial | M | S | Mask invert per-level UI | _BaseMaskToolsWidget.py:207-218 | `invert()` exists but no UI button or Ctrl+I shortcut |
| ✅ Done | M | S | Mask not-finite (NaN/Inf) button | _BaseMaskToolsWidget.py:296-304 | `MaskToolsWidget::mask_not_finite` (silx `updateNotFinite`: stencil `!isfinite`, tested) + Wave 7 "mask non-finite" button in the ImageView mask-draw toolbar (masks the active image's NaN/Inf pixels at the current level, commits, re-uploads; enabled when mask geometry matches the image). Button/overlay GPU-unverified |
| ☐ Missing | L | S | Mask per-level color override UI | _BaseMaskToolsWidget.py:394-398 | `overrides` field exists but no UI or get/`setMaskColors` API |

### composites

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✅ Done | M | S | ImageView `getHistogram()` public API | ImageView.py:699-725 | Wave 7: `histogram(axis) -> Option<ImageProfileHistogram { data, extent }>` (silx `getHistogram` `{data, extent}` dict). `ImageHistogramAxis::{X,Y}` → per-column / per-row sums (shared `image_column_sums`/`image_row_sums` helpers, also feeding `rebuild_histograms`); extent `(0, width)`/`(0, height)`, end-exclusive |
| ✅ Done | M | S | ImageView side-histogram show/hide API | ImageView.py:552-566 | Wave 7: `is_side_histogram_displayed`/`set_side_histogram_displayed` (silx `is/setSideHistogramDisplayed`). `show()` gates the top histogram, the radar viewport sync, and the bottom vertical-histogram+radar column on the flag — silx hides `_histoHPlot`/`_histoVPlot`/`_radarView` as one cluster; strip extents collapse to 0 via `side_histogram_extent` so the image reclaims the space |
| ✅ Done | M | S | ImageView `valueChanged` signal | ImageView.py:381,585-601 | Wave 7: `value_changed() -> Option<(col, row, value)>` (silx `valueChanged` emitted at `_imagePlotCB`). Pure `image_value_at` maps cursor data coords to a pixel index via silx `int((x-origin)/scale)` (identity geometry), guarded against the origin and the pixel grid; `None` when off-image (silx emits nothing) |
| ✅ Done | M | M | ScatterView position-info panel API | ScatterView.py:90-101 | `show_position_info(ui, &PlotResponse)` embeds `PositionInfo` with X/Y/Data/Index, snapping to the picked point (`scatter_pick_pixels`, pixel-space, silx `_pickScatterData`) |
| ✅ Done | M | S | ScatterView get/setSelectionMask API | ScatterView.py:412-418 | `selection_mask()`/`set_selection_mask(&[u8])` (silx getSelectionMask/setSelectionMask): per-point u8 levels; set validates length == point count, commits to undo history |
| ✅ Done | M | M | StackView perspective selection | StackView.py:364-397 | `StackPerspective` {Axis0,Axis1,Axis2} + `set_perspective`/`perspective`/`perspective_ui` combo pick the browse dimension |
| ✅ Done | M | M | StackView 3D transposition | StackView.py:409-441 | `set_volume([d0,d1,d2])` + `stack_frame` re-slice per perspective (silx transpose (1,0,2)/(2,0,1)); axis labels via `dimension_axis_labels` |
| ✅ Done | M | S | StackView dimension labels | StackView.py:799-827 | `set_dimension_labels([&str;3])`/`dimension_labels()` (silx setLabels/getLabels); empty→default `"Dimension N"`; axis labels rotate with perspective |
| ◐ Partial | M | M | CompareImages vline/hline separator modes | CompareImages.py:124-133, 422-445 | Wave 7: `HalfHalf` is the silx VERTICAL_LINE composite (A left / B right); added `CompareMode::SplitHorizontal` = HORIZONTAL_LINE (A top / B bottom) via the pure `split_composite` helper + toolbar button. Split position is a slider (`split` fraction). Remaining: an on-plot *draggable* separator line (silx `__separator` markers + `__separatorConstraint`) instead of the slider |
| ☐ Missing | M | M | ComplexImageView amplitude-range dialog | ComplexImageView.py:50-155 | No dialog to set displayed max amplitude + log10 delta at runtime |
| ☐ Missing | L | L | StackView 3D-profile toolbar | StackView.py:948-951 | No `Profile3DToolBar`; profiles not extracted across the stack dimension |
| ☐ Missing | L | M | StackView calibration (per-axis scale/origin) | StackView.py:551-566 | No `Calibration` objects / axis-transform API |

### actions-toolbars

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ◐ Partial | M | M | Print: native dialog + printer submission | actions/io.py:747-845 | `print_graph()` GPU-readback shim exists; no native printer dialog or print-to-device backend |
| ◐ Partial | M | M | ColormapAction floating-dialog toggle | actions/control.py:352-448 | Dialog wired to views but no toolbar toggle button that opens/closes it |
| ◐ Partial | M | M | ScatterToolBar viz-mode selector button | tools/toolbars.py:273-362 | Dispatch implemented; mode selector lives in the ScatterView panel, not the main toolbar |
| ◐ Partial | M | S | ProfileOption + line-width/method (sum vs mean) | PlotToolButtons.py:227-301 | Width/method selectors in panels; extraction always computes mean; no standalone toolbar button |
| ◐ Partial | M | S | ProfileToolButton (1D vs 2D selector) | PlotToolButtons.py:304-391 | Dimension selector in view panels, not a standalone toolbar button |
| ◐ Partial | M | M | SymbolToolButton (marker symbol + size) | PlotToolButtons.py:394+ | Selector exists in ScatterView UI; not wired as a toolbar button |
| ◐ Partial | M | M | ScatterVisualizationToolButton | PlotToolButtons.py:550+ | Viz-mode selector in the ScatterView panel, not the toolbar |
| ☐ Missing | L | L | SaveAction PDF/EPS/JPEG | PlotWidget.py:3232-3242 | PNG/PPM/SVG/TIFF only; `from_extension` rejects pdf/eps/jpeg |
| ☐ Missing | L | S | Y2AxisAutoScaleAction | PlotWidget.py | `y2_autoscale` flag modeled but not exposed as a toolbar action |
| ☐ Missing | L | S | ClosePolygonInteractionAction | actions/control.py:651+ | ROI draw has polygon snap-close but no dedicated action/button |
| ☐ — | L | S | OpenGLAction (backend selector) | actions/control.py | N/A: siplot is wgpu-only (intentional) |
| ☐ Missing | L | M | LimitsToolBar (editable X/Y min/max fields) | tools/toolbars.py | Limit APIs exist but no toolbar row of editable axis-limit spinners |

### stats-profile-fit-positioninfo

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ◐ Partial | M | M | Stats context (viewport/ROI masking, live binding) | stats/stats.py:143-600 | Pure engine; no live plot-state binding, item-change signals, or auto-recompute on viewport change |
| ◐ Partial | M | M | StatsWidget live item binding | StatsWidget.py:200-700 | Table UI present but not docked to a plot; no live binding to active curve/image |
| ◐ Partial | M | M | Profile line width / method (mean vs sum) | tools/profile/rois.py:220-260, core.py:204-270 | Wave 7: `ProfileMethod{Mean,Sum}` + `aligned_profile_values(.., position, roi_width, horizontal, method)` ports silx `_alignedFullProfile` (band of `roi_width` pixels, silx start/end placement, mean÷band or sum) for H/V line profiles. Remaining: free-line `line_profile` perpendicular band width, `rect_profile` sum method, and UI selectors wired to the extractor (row 232) |
| ◐ Partial | M | M | Fit range-selection UI (xmin/xmax) | FitWidget.py:336-361 | `fit_range` model works; no UI input fields or interactive on-plot range selection |
| ◐ Partial | M | M | PositionInfo snapping to nearest item | tools/PositionInfo.py:179-292 | Wave 7: `snap_to_nearest(cursor_px, candidates, threshold_px) -> Option<Snap>` ports the silx picking kernel (:236-292) — global-nearest point within `threshold²` (silx `<=`, ties to later, non-finite skipped), `SNAP_THRESHOLD_DIST = 5` logical px. Pure pixel-space math, unit-tested. Remaining (live plot/GPU state): `SNAPPING_CURVE`/`SNAPPING_SCATTER`/`SNAPPING_ACTIVE_ONLY`/`SNAPPING_SYMBOLS_ONLY` item selection, data→pixel projection of candidates, and red/normal label styling |
| ◐ Partial | M | M | FitWidget UI extras (editable params/constraints/peak-search/bg) | silx.gui.fit.FitWidget | Results table renders; missing editable param input, constraints UI, multi-peak UI, background-model selector |
| ◐ Partial | L | S | Profile cross-profile dual-curve display | tools/profile/rois.py:663-700 | H/V extracted separately; UI shows them in one window not simultaneously |
| ☐ Missing | L | L | Profile over stack (slice across 3D dim) | tools/profile/rois.py:1058-1165 | No stack-aware profile variants |
| ☐ Missing | L | L | Fit background subtraction (const/poly/strip/snip) | silx.math.fit.bgtheories.py | Peak models assume flat background; no selectable background subtraction |
| ☐ Missing | L | L | Fit parameter constraints (POSITIVE/FIXED/QUOTED/…) | fitmanager.py:421-430 | `leastsq` has no constraint enforcement; bare Levenberg-Marquardt |
| ☐ Missing | L | L | Fit multi-peak search | fittheories `peak_search()` | Single-peak fits only; no multi-peak discovery |
| ☐ Missing | L | L | Fit non-peak theories (step/exp/atan/poly) | fittheories | Only Gaussian/Lorentzian/PseudoVoigt + linear |
| ☐ Missing | L | L | Standalone RadarView overview widget | tools/RadarView.py:139-300 | RadarView is wired into views; no standalone full-data-thumbnail widget |
| ☐ Missing | L | L | Print preview dialog | PrintPreviewToolButton.py | No print-preview page with movable/resizable rect |
| ◐ Partial | L | L | ItemsSelectionDialog reuse | ItemsSelectionDialog.py | Dialog exists (Wave 6B-1) but isn't reused by fit/stats tools |

### Top candidate next waves (re-baselined 2026-06-04)

1. ~~**ROI on-canvas styling + selection feedback**~~ — **DONE (Wave 13, main, not
   pushed).** Closed the top H cluster via the structural fix: `plot.rois` is now
   `Vec<ManagedRoi>` (geometry + appearance) = single source of truth, the one live
   `chrome::draw_rois` path resolves each ROI's color/name/selection/width/style/fill,
   and `RoiManagerWidget` was reworked to a stateless editor over that one collection
   (its second `Vec<ManagedRoi>` removed). Closed rows: per-instance color, name label,
   selection highlight, `sigCurrentRoiChanged`, line style, line width, manager
   default-color, fill toggle. *Still open (separate rows, not this wave):* ROI
   handle-symbol customization, right-click context menu, edge-position constraints.
2. **Mask drawing tools + threshold/finite ops** (L, ~8 rows) — three H-priority
   Missing draw tools (Rectangle/Polygon/Ellipse) + threshold UI, invert/transparency/
   not-finite controls, Mask/Unmask toggle. Enum variants + `ImageMask` buffer +
   `MaskHistory` already exist (`mask_tools.rs`); this wave wires interaction + UI on
   done primitives. Biggest functional hole in the image workflow.
3. ~~**Structured item-click + hover signals**~~ — **DONE (Wave 14, main, not
   pushed) — click, hover, AND marker-drag triad.** Added item-identified
   `PlotEvent` variants `CurveClicked{handle,index,x,y,button}` /
   `ImageClicked{handle,col,row,button}` / `ItemClicked{handle,button}` (marker/
   scatter/shape) / `ItemHovered{handle,kind}`. `PlotWidget::show` routes this
   frame's `PlotPointerEvent` through one owner `pick_topmost(pos) ->
   (ItemHandle, PickResult)` (extends the prior `pick_topmost_item`), and the pure
   `click_event_for_pick(handle, &PickResult, button)` maps each `PickResult`
   variant to its event (unit-tested per boundary). `ItemHovered` was then
   expanded to the full silx `prepareHoverSignal` payload
   `{handle,kind,label,x,y,xpixel,ypixel,draggable}` (silx `selectable` omitted as
   structurally constant). Finally the marker-drag triad was added: the single
   owner `apply_interaction` (plot_widget.rs) surfaces `marker_drag_started`/
   `marker_drag_finished` on `PlotResponse` at the grab and release sites, and
   `show` emits `MarkerDragStarted`→`MarkerMoved`×N→`MarkerDragFinished` (silx
   `beginDrag`/`drag`/`endDrag`), covered by a headless press→move→release test.
   *Closed rows:* curveClicked/markerClicked/imageClicked (row 49) +
   hover-with-metadata (row 50) + ItemsInteraction picker/state-machine + marker
   drag-start/end triad + markerMoving/markerMoved split. **GPU
   boundary:** the end-to-end `pick_item` and the hover metadata assembly
   (`backend.marker`/`item_legend`) need a wgpu `RenderState` no test builds, so
   on-hardware picking is GPU-unverified; the pure `click_event_for_pick`, the
   headless marker-drag state-machine test, + the
   existing pure pickers are the headlessly-tested seam.
4. **ROI stats dock widgets** (L, 2 H rows) — `CurvesROIWidget` + `ROIStatsWidget`;
   the compute (`image_roi_stats`/`curve_roi_stats`) already exists, so it's widget
   construction + binding. Consumes the selection feedback from wave 3.
5. **StackView 3D depth** (L, ~5 rows) — perspective selection, 3D transposition,
   dimension labels, calibration, 3D-profile toolbar. Turns the 1D frame browser into
   a true volume browser. Self-contained in `image_stack.rs`.
6. **ColormapDialog completeness** (M, ~3 rows, 1 H) — Stddev3/Percentile from raw
   pixels (wire `Plot2D::get_image_pixels_raw` → `setHistogram`), histogram-distribution
   overlay, matplotlib-dynamic catalog. Localized to `colormap_dialog.rs` + a small
   Plot2D accessor.

The frozen as-of-sweep per-area tables below are kept only as the original reference;
the re-baselined view above supersedes their status columns.

## Progress log

### Wave 0 — cited detail
- ProfileWindow opens *beside* the main window (right → left → roomier edge,
  vertically centred), restores previous position on reopen. silx
  `ProfileManager.initProfileWindow`. Commit `3010677` (+4 boundary tests).

### Wave 1 — Colormap / Symbols / Mask model (parallel, worktree-isolated)
- **Colormap** (`c8b3794`..`1ab00de`): catalog → gray/reversed-gray/red/green/blue/
  temperature/jet/hsv; `Normalization::Arcsinh`; `AutoscaleMode` MinMax/Stddev3/
  Percentile(1,99); `Colormap::nan_color`; ColormapDialog wiring.
- **Symbols** (`22347ea`,`361a519`): silx symbol set — diamond, point, pixel,
  vertical/horizontal line, tick{left,right,up,down}, caret{left,right,up,down};
  char-code parsing matching silx.
- **Mask model** (`58e1416`..`f513506`): multi-level u8 buffer + level selector;
  rectangle/polygon/disk/ellipse fills; threshold below/between/above; invert;
  bounded undo/redo.
- Gate: clippy `--workspace` clean, **185 tests pass** (+41), doctests ok.
- **Deferred follow-ups** (cross-cluster wiring): arcsinh branch in `image.wgsl`;
  ColormapDialog Stddev3/Percentile need Plot2D raw-pixel access; Heart glyph SDF;
  mask on-plot drawing (Plot2D wiring) + Bresenham `draw_line` + mask file I/O.

### Wave 2 — Image render / ROI / Interaction / Complex view (parallel, worktree-isolated)
- **Image render** (`0494dc8`,`c6344d1`,`595c68e`): arcsinh image normalization
  (shader code-4 branch + `Normalization::Arcsinh`); `InterpolationMode`
  Nearest(default)/Linear (manual bilinear on scalar data before colormap, since
  R32Float is non-filterable); `AggregationMode` None/Max/Mean/Min + `aggregate_blocks()`
  (NaN-ignoring, remainder dropped, scale scaled by block — silx ImageDataAggregated).
- **ROI** (`bbd28ed`,`b909dbe`,`26e3fdc`): `Roi::contains()` for every variant; new
  Cross/Circle/Ellipse variants; chrome draws ROI color/name label/selected highlight;
  `RoiManagerWidget` per-ROI color/name, current-ROI tracking, add buttons.
- **Interaction** (`3030edd`,`4efac2d`,`122f7f1`): log-aware pan/zoom (silx `panzoom`);
  limits-history undo/redo stack (silx `LimitsHistory`); arrow-key pan (silx
  `PanWithArrowKeysAction`).
- **Complex view** (`ee3d82e`): `ComplexImageView` composite — ComplexMode
  Absolute/Phase/Real/Imaginary/SquareAmplitude/Log10Amplitude/AmplitudePhase, phase
  hsv LUT, amplitude×phase compositing.
- Gate: clippy `--workspace` clean, **239 tests pass** (+54), doctests ok.
- **Integration fix** (`3430ef5`): the complex-view cluster predated Wave 1's
  `Colormap::nan_color`, so its raw `Colormap {}` literal stopped compiling once both
  waves landed. Rebuilt `phase_colormap()` through `Colormap::new` + functional-update
  so future field additions no longer break the call site. Root cause: worktrees were
  cut from session-start HEAD, not current `main` — later waves must
  `git checkout -b waveN/<cluster> main` to inherit prior waves.
- **Deferred follow-ups** (cross-cluster wiring): interpolation/aggregation toolbar
  toggles; ROI creation-mode toolbar + on-plot draw; limits-history undo/redo buttons;
  ComplexImageView mode selector wired into a toolbar/menu (widget is self-contained).

### Wave 3 — ROI stats/styling / ColorBar+Alpha / Stats engine / ImageStack (parallel, worktree-isolated)
- **ROI** (`79822ac`,`9f17d0d`,`e112b63`,`b48175f`): handle geometry (`Roi::handles()` →
  RoiHandle{pos,kind}, `Roi::translate`, pure — no mouse wiring); ArcROI (annular sector,
  `rem_euclid` angle wrap) + BandROI (rotatable band) variants with `contains()`/handles/
  chrome draw; per-ROI line width/style(Solid/Dashed/Dotted, manual dash segments)/fill on
  ManagedRoi+chrome; ROI statistics module `roi_stats.rs` (`image_roi_stats`/`curve_roi_stats`
  — min/max/mean/sum/integral, NaN-skipping; silx ROIStats + CurvesROIWidget raw-count).
  Adding the two enum variants required 2 match arms in `high_level.rs` roi_description and
  `examples/roi.rs` (non-`#[non_exhaustive]` enum obligation, not new hub logic).
- **ColorBar/Alpha** (`c25a273`,`16c9c59`,`88877a9`): standalone `ColorBarWidget`
  (256-step gradient, vertical+horizontal, nice/decade ticks via silx `ticklayout.py`,
  `%g` end labels, rotated legend); `AlphaSlider` (0..=255 ↔ f32 alpha); Colormap utilities
  — custom-LUT (`with_lut` + Nx3/Nx4 resample to 256), `set_autoscale_percentiles`,
  `editable` flag guarding mutators (public field — silx `_editable` is public; required so
  FRU `..Colormap::new()` in complex_image_view still compiles), copy/set_from.
- **Stats** (`429f45b`,`4ad46dc`,`28a1574`): pure engine `core/stats.rs` — min/max/delta/
  mean/sum/COM/argmin-argmax-coords/integral, `StatScope::{All,OnLimits}` viewport clipping,
  curve-ROI x-range scope (silx `stats.py`); `StatsWidget` table (per-item rows, stat columns,
  auto/manual update, `%.7g` formatter); `PositionInfo` readout bar with pluggable converters
  (default polar). Built standalone — `high_level.rs`'s minimal ValueStats not yet delegated.
- **ImageStack** (`7ce78b3`): self-contained composite (mirrors ComplexImageView) browsing an
  in-memory `Vec<Option<Frame>>` — slider + first/prev/next/last, frame table with visibility
  toggles, waiting overlay for empty/hidden/size-mismatched slots; pure `FrameNav` core (clamp/
  step/visibility/labels) drives 22 tests. Lazy URL/HDF5 + prefetch out of scope (silx.io).
- Gate: clippy `--workspace` clean, **381 tests pass** (+142), doctests ok.
- **Deferred follow-ups** (need `high_level.rs`/`interaction.rs`, an actions wave): on-plot
  ROI editing/creation incl. Arc/Band per-handle drag + `Roi::move_edge` for the new kinds
  (no-op now); ROITable rich-stats columns wiring `image_roi_stats`; ColorBarWidget into
  ImageView/ScatterView/chrome; NamedItem/ActiveImage AlphaSlider (needs plot model);
  StatsWidget/PositionInfo bound to live items+cursor + PositionInfo snapping; delegating
  `high_level.rs` ValueStats to `core::stats`; ItemsSelectionDialog; ImageStack toolbar entry;
  promoting `RoiLineStyle` to the lib.rs convenience re-export.

### Wave 4 — Interaction draw-modes / Axis+datetime / Image-Marker-Shape items / Mask IO (parallel, worktree-isolated)
- **Interaction** (`c6e6fe3`,`60fe2af`,`9bacd18`,`2a059ce`,`5383c1b`): float32-safe
  pan/zoom clamping (silx `FLOAT32_SAFE_MIN/MAX`); new `PlotPointerEvent` (click/double-click/
  hover with button + data+pixel pos + limits tuples — kept distinct from `high_level`'s
  `PlotEvent`); `CursorShape` from draggable-edge detection; `DrawState` state machine
  (Rectangle/Ellipse/Line/H-V-Line/Polygon-with-snap-close/FreeHand → `DrawEvent::{InProgress,
  Finished}`, silx PlotInteraction Select*); selection-area fill modes (hatch/solid/none) +
  draw-mode rubber-band overlay painted in `plot_widget.rs` (not chrome). NaN bound clamps to
  lower (documented; keeps range finite/ordered, unlike numpy.clip).
- **Axis** (`9961a07`,`5052b4e`,`3957d98`,`da81279`,`7480498`,`ba4ef49`): `core/dtime_ticks.rs`
  (UTC-only DtUnit/bestUnit/calcTicks via days-from-civil, no chrono dep — silx
  `dtime_ticklayout.py`); per-axis autoscale + `DataRange` reset-zoom; `DataMargins` per-side
  ratios (log-aware); `axes_displayed` flag + `DirtyState{Clean,Overlay,Full}` lifecycle
  (state only); axis-label fallback to active-curve label; `grid_color` split from foreground.
  All additive — Plot is built only via `Plot::new`, so existing construction is untouched.
- **Items** (`dc18c84`,`3b53a6a`,`5d2836c`,`cf8c4fe`,`21c077d`): `ScalarMask` (per-pixel
  validity → NaN via the existing `nan_color` path; standalone since `ImageData` lives in
  `render/gpu_image.rs`); marker `is_draggable` + `MarkerConstraint`(H/V/custom, pure
  `apply_constraint`) + `drag`; `TextAnchor` alignment offset; `Shape::is_overlay`; infinite
  `Line` item with `clipped_segment(bounds)` (silx shape.py `Line.__updatePoints`).
- **Mask** (`b062e32`,`bc19767`,`233ccac`,`94f799a`): Bresenham pencil `draw_line` (width +
  degenerate, silx `shapes.draw_line`); hand-written NumPy `.npy` save/load with crop/pad +
  resize flag (no external crate); `mask_not_finite`; new `scatter_mask.rs` `ScatterMaskWidget`
  (per-point disk/polygon/rect masking, point-in-polygon from `Polygon.is_inside`, shared
  multi-level + undo/redo).
- Gate: clippy `--workspace` clean, **510 tests pass** (+129), doctests ok.
- **Deferred follow-ups** (actions/render wave): chrome wiring of `axes_displayed`/`grid_color`/
  datetime tick labels; render of `Line`/marker-drag/`Shape::is_overlay` data layer; `ScalarMask`
  applied before image upload; `DrawEvent::Finished` → ROI/mask creation; overlay-only replot
  short-circuit; per-pixel scalar alpha (needs shader); timezone for datetime axis; on-plot mask
  draw (plot→data coords) + mask colormap overlay + active-item sync; EDF/TIFF/HDF5/msk codecs.

### Wave 5 — Iterative fit / Scatter-viz algorithms / Export formats / RadarView (parallel, worktree-isolated)
- **Fit** (`f293ca1`,`5b84bff`): unconstrained Levenberg-Marquardt `leastsq` (forward numerical
  Jacobian, flambda damping, covariance via Gauss-Jordan inverse, reduced-χ² — silx
  `math/fit/leastsq.py`); peak models Gaussian/GaussianArea/Lorentzian/PseudoVoigt with analytical
  seeds (`fittheories.py`/`funs.c`); `fit_in_range(xmin,xmax)`; FitWidget `FitModelChoice` +
  results table (name·value·±error·reduced-χ²). Legacy estimate/linear fit kept (additive).
- **Scatter-viz** (`c8f0544`): pure `core/scatter_viz.rs` — Bowyer-Watson Delaunay; SOLID
  (per-vertex-colored `Triangles`); IRREGULAR_GRID (barycentric raster to image); regular-grid
  auto-detection + `GridMajorOrder`; `BinnedStatistic` mean/count/sum; per-point alpha. Render/
  picking wiring deferred. Note: IRREGULAR here = Delaunay-raster, not silx's quadrilateral mesh.
- **Export** (`5499da2`,`7abefe6`,`b225b68`,`0e9a258`): PPM (P6); SVG (base64-PNG `<image>` wrap);
  hand-written uncompressed baseline TIFF with DPI XResolution/YResolution tags; `SaveFormat` enum
  + extension auto-detect + `save_graph_with_format` dispatch (silx `saveGraph`/`PlotImageFile`).
  All encoders pure `encode_*(rgba,w,h,..)`; existing `save_graph`/PNG signature unchanged.
- **RadarView** (`5d8bede`): self-contained `widget/radar_view.rs` overview — full-extent box +
  draggable viewport rect, aspect-fit data↔widget mapping + inverse, clamp-to-extent, hit-test,
  emits new limits on drag (silx `tools/RadarView.py`). Live-plot pan wiring deferred.
- Gate: clippy `--workspace` clean, **584 tests pass** (+74), doctests ok.
- **Deferred follow-ups** (actions/render wave): wire scatter-viz outputs into the GPU triangle/
  image path + per-mode picking; FitWidget→live curve; RadarView→Plot2D pan + auto-extent; true
  vector SVG (record draw ops); JPEG/EPS/PDF; LM constraints + strip background + multi-peak search.

### Wave 6A — Chrome / input plumbing (model + primitives the HL wiring consumes)
First sub-wave of the chrome/actions effort. Single file-disjoint cluster owning `chrome.rs` +
`plot_widget.rs` + `interaction.rs` + `core/plot.rs` (the others would collide on `plot_widget.rs`,
which orchestrates chrome render + interaction together). Implemented by a worktree agent, then
each item adversarially verified (silx-fidelity + additive-only + gate); 2 fixes applied at source.
- **Datetime ticks** (`5ee3630`,`5477f69`): X-axis `TickMode` (Numeric/TimeSeries) on `Plot`; chrome
  `axis_ticks_with_mode` routes a TimeSeries linear X axis through `dtime_ticks::calc_ticks`/
  `format_ticks` (silx `XAxis.setTickMode`+`NiceDateLocator`). **Fidelity fix:** silx implements
  `setTickMode` on `XAxis` only (`YAxis` raises `NotImplementedError`, no `setYAxisTimeSeries`), so
  the time-series mode is X-only by API shape — dropped the symmetric `y_tick_mode` the first pass added.
- **Axes-hidden gutters** (`ac84ca0`): `ChromeRequest.axes_hidden` collapses every axis gutter to
  zero and skips frame/ticks/labels when `!axes_displayed()` (silx `setAxesDisplayed(False)`); colorbar
  strip still reserved (separate widget). Default false = no behavior change.
- **Infinite Line render** (`41eebb1`): `Plot.lines: Vec<Line>` + `add_line`; chrome `draw_lines`
  clips each via the tested `Line::clipped_segment` and paints the visible segment, called after
  `draw_shapes` (silx `items/shape.py` Line `__updatePoints`).
- **PlotPointerEvent / DrawEvent / mode surfacing** (`7d23f37`,`84289c7`): `apply_interaction` emits
  `PlotPointerEvent` (Clicked/DoubleClicked/Moved, pixel→data via transform; silx `prepareMouseSignal`)
  via additive `PlotResponse.pointer_event`; `draw_event` mirrors the latest draw event onto the plain
  show path; read-only `interaction_mode`. **Test fix:** added double-click (explicit headless
  timestamps) + right/middle-button coverage.
- Item 5 (ROI-edge cursor) was already complete on `main`; no no-op commit. Gate: clippy clean,
  **598 tests pass** (+14), doctests ok.
- **UNFIXED / deferred this sub-wave** (external dep or decision-gated): marker drag + marker-drag
  cursor (marker list lives in `Plot2D`, not core `Plot` — needs ownership decision); `Shape::is_overlay`
  data-layer (no under-chrome render path); `DirtyState::Overlay` short-circuit (render-loop wave);
  high-level mouseClicked/markerClicked/curveClicked **consumption** (lands in `high_level.rs`, 6B).

### Wave 6B-1 — HL view wiring (ImageView side) + standalone widget files (2 parallel clusters)
Two file-disjoint worktree clusters; per-item adversarial verify; 1 fix applied at source.
- **standalone-widgets** (own files, disjoint from `high_level.rs`): ColormapDialog NaN-color picker
  (`503bc34`) + percentile-fields regression test (`b70acc9`); ComplexImageView `show_mode_toolbar`
  mode selector (`3837a68`, silx `_ComplexDataToolButton`); ImageStack public `show_toolbar` +
  `NavAction` first/prev/next/last/goto (`9600976`); mask `.npy` codec moved to one owner in
  `render::save` (`encode_mask_npy`/`decode_mask_npy`, numpy v1.0 uint8) + path-string save/load,
  `mask_tools` delegates (`3054459`); NEW `ItemsSelectionDialog` — per-kind filter + grouped
  selection, genuinely missing vs the flat example (`e21fdff`). Interactive controls factored into
  pure `*_ui` helpers for headless-egui tests.
- **hl-views** (`high_level.rs` sole writer): ColorBar column synced to colormap (`68b8eb7`);
  AlphaSlider→active image (`9d8f890`); interpolation/aggregation selectors (`700edf0`); PositionInfo
  bound to the live cursor via 6A's `PlotPointerEvent` (`50b0f0e`,`9f6eb92` test fix adds
  Clicked/DoubleClicked coverage); RadarView overview→pan/zoom (`c2f35ee`); profile tool (`29e645a`);
  optional pre-upload `ScalarMask`→NaN on `Plot2D` images (`8ef46c2`); `ValueStats` delegated to
  `core::stats` single source (`d2468ab`).
- Gate: clippy `--workspace` clean, **633 tests pass** (+35), doctests ok. All 14 items accepted by
  review except the PositionInfo test gap (fixed). No items deferred within this sub-wave.
- **Deferred to 6B-2** (need `high_level.rs` + `plot_widget.rs`/`interaction.rs` together, or raw pixels):
  ScatterView ColorBar/scatter-viz-dispatch/mask-panel; StatsWidget + FitWidget binding; Stddev3/
  Percentile autoscale from raw pixels (`Plot2D::get_image_pixels_raw`); mask mode-vs-pan
  (`PlotInteractionMode::MaskDraw`) + pencil-draw routing.

### Wave 6B-2 — HL view wiring (ScatterView side) + stats/fit binding + raw-pixel autoscale
Single `high_level.rs`-sole-writer cluster (6 items); per-item adversarial verify (all 6 accepted, 0 issues).
- ScatterView value ColorBar synced to colormap limits (`b03d76c`, silx `ScatterView.py:83-88`).
- ScatterView visualization-mode dispatch: new `ScatterVisualization` enum (silx
  `ScatterVisualizationMixIn`); POINTS = existing marker cloud (default, unchanged); IRREGULAR_GRID /
  REGULAR_GRID / BINNED_STATISTIC convert retained `(x,y,value)` via the Wave-5 `core::scatter_viz`
  primitives → `GridImage` → image path; `rebuild_visualization` is the single owner of the
  scatter-vs-grid item handle (`f6ee396`, silx `scatter.py:402-680`). SOLID deferred (shader).
- ScatterView mask-tools panel: embeds a `ScatterMaskWidget` sized to point count; level/clear/invert/
  undo/redo/threshold/disk/rect/polygon selections applied to the scatter (`2440965`, silx
  `ScatterView.py:116-122`).
- `StatsWidget` bound to live items via new `RetainedItemData` on `ItemRecord` (populated by the typed
  curve/image spec entry points); `feed_active_stats` recomputes from the active item through the one
  `core::stats` engine (`c2d1fa3`, silx `StatsWidget`).
- `FitWidget` bound to a live curve: `set_fit_target`/`set_active_fit_target` pull the curve's retained
  `(x,y)` and call `FitWidget::set_data`; images/non-curves rejected (`43cc1df`, silx `FitWidget`).
- Raw-pixel autoscale: new `Plot2D::get_image_pixels_raw`; `autoscale_active_image` computes Stddev3
  (mean ± 3·std) / Percentile bounds NaN-ignoring via `AutoscaleMode` and re-uploads the image colormap;
  `colormap_dialog.rs` untouched (`1be8f53`, silx `ColormapDialog.py:450-480`).
- Gate: clippy `--workspace` clean, **647 tests pass** (+14), doctests ok. `lib.rs` gained one additive
  re-export (`ScatterVisualization`). No foreign-file edits.
- Integration note: an agent's first Item-1 edits leaked into the MAIN checkout via a bare repo-root
  path (same failure mode as 6B-1); the agent reverted with `git checkout -- <file>` and redid the work
  in the worktree, but its recovery left a stray `git reset` that fast-forwarded `main` to the branch
  tip. Verified safe before recording: `main^{tree}` byte-identical to the adversarially-verified
  `wave6/hl-scatter-stats` tip, 6 commits linear on `8a0ee44`, diff touches only `high_level.rs`+`lib.rs`,
  full-workspace gate re-run green on the main checkout.
- **Deferred to 6C** (need `plot_widget.rs`+`interaction.rs`+a toolbar toggle): mask mode-vs-pan
  (`PlotInteractionMode::MaskDraw`) + on-plot pencil-draw routing. **Deferred (shader):** SOLID scatter
  visualization, per-point scatter alpha.

### Wave 6C-1 — HL actions module + toolbar wiring (rfd Save + arboard Copy)
New `src/widget/actions/{mod,control,io}.rs` mirroring silx `plot/actions/` *behavior* (not the Qt
QAction hierarchy — immediate-mode egui needs none). Added crates `rfd` (native Save dialog) + `arboard`
(clipboard) — user-approved. Single `high_level.rs`+`actions/` sole-writer cluster (7 commits + 4
review-fix commits); per-item adversarial verify.
- ShowAxis toggle (`set_axes_displayed`, silx `ShowAxisAction`); ColorBar show/hide on ImageView+
  ScatterView (silx `ColorBarAction`); CurveStyle cycle of the active curve's `LineStyle` (silx
  `CurveStyleAction` — divergence: silx cycles plot-wide line/points booleans, accepted); ZoomIn/ZoomOut
  about the view center at silx's 1.1 step (silx `ZoomIn/OutAction`); ZoomBack popping `Plot::
  limits_history` with reset-zoom fallback (silx `ZoomBackAction`).
- Save (silx `SaveAction`): `rfd` native dialog → figure PNG (existing `save_graph`, GPU-readback shim)
  or active-curve CSV; `SaveTarget` extension→format + `curve_to_csv` are pure/tested. Copy (silx
  `CopyAction`): figure RGBA→`arboard::ImageData` via pure `decode_png_to_rgba`/`rgba_to_clipboard_image`,
  clipboard call is a shim.
- CurveStyle is backed by a new `RetainedItemData::Curve` holding the full `CurveData` (so the cycle
  preserves color/symbol/width/errors/fill); `lib.rs` gained `actions` + `SaveTarget`/`curve_to_csv`
  re-exports.
- Adversarial review found 4 fix-needed; all fixed at source on `main` as one-commit-per-finding:
  (A) colorbar-toggle observable-effect test (`e748a7d`); (B) toolbar zoom must NOT push limits history —
  silx pushes only from the drag-zoom interaction, removed the `apply_zoom` push (`5664ac8`); (C) zoom on
  a log axis must scale in `log10` space — added silx `scale1DRange`'s log branch + per-axis log threading
  + geometric log-midpoint, float32 clamp left to its separate item (`f2aeeae`); (D) curve CSV must emit
  true C/numpy `%.18e` (`e+00`) not Rust's `e0`, with a non-tautological test cross-checked vs Python
  (`08d9fe8`).
- Integration note: the agent's leak-check passed once after the first commit but later edits still leaked
  an uncommitted partial mirror into the MAIN checkout (missing the final `lib.rs` lines). The committed
  branch was authoritative; `reset --hard wave6/hl-actions` discarded the leak and fast-forwarded `main`
  to the verified tip (tree-identical), then the 4 fixes landed on top.
- Gate: clippy `--workspace` clean, **665 tests pass** (+18), doctests ok.
- **Deferred to 6C-2** (need `plot_widget.rs`+`interaction.rs`+`actions/mode.rs`): mask mode-vs-pan
  (`PlotInteractionMode::MaskDraw`) + on-plot pencil-draw routing. **Deferred to Wave 7:** Print action,
  median-filter + pixel-histogram actions, SVG/PPM/TIFF figure save (needs a public RenderState/format
  save path), per-axis X-only/Y-only autoscale toggles.

### Wave 6C-2 — Mask draw mode + on-plot pencil routing (final Wave-6 sub-wave)
Single worktree-isolated cluster owning `plot_widget.rs` + `actions/mode.rs` (NEW) + `high_level.rs`
(3 commits); per-item adversarial verify. BASE `79583e3`.
- `PlotInteractionMode::MaskDraw` variant — silx's dedicated pencil draw interaction, distinct from
  pan/zoom: primary-drag is reserved for mask painting. `apply_interaction`'s `== Pan`/`== Zoom`
  comparisons already suppress primary-pan and box-zoom for the new variant; the one primary-drag path
  NOT gated by an `== mode` check (ROI-edge grab, was `mode != Pan`) became `mode_grabs_roi_edge(mode)`
  excluding Pan AND MaskDraw, applied at the grab site and the hover resize-cursor site. Secondary-drag
  pan + wheel zoom intentionally left intact (matches silx draw interaction). NO enum-leak: every
  `PlotInteractionMode` use is an `== Variant` comparison (no exhaustive match anywhere), so the variant
  forced zero match-arm edits — clippy `--all-targets` (compiles examples) confirms.
- `actions/mode.rs` (NEW): `zoom_mode`/`pan_mode`/`mask_draw_mode`/`select_mode` — thin one-transition
  setters over `PlotWidget::set_interaction_mode`, mirroring silx `actions/mode.py` `ZoomModeAction`
  (`mode.py:45`) / `PanModeAction` (`mode.py:108`). silx has no `MaskModeAction` (`MaskToolsWidget` owns
  its pencil draw mode), so `mask_draw_mode` is a port-specific setter grouped with the others. `lib.rs`
  re-exports the four; `actions/mod.rs` gains `pub mod mode`.
- ImageView mask painting: embedded a `MaskToolsWidget` (resized to the active image on `set_image`; a
  shape change resets undo history = silx `reset(shape)`). `ImageView::show` routes the captured
  `PlotResponse` to the EXISTING Wave-4 `handle_interaction` strictly gated on `MaskDraw`
  (`image_view_should_paint_mask` — pan/zoom/select never paint); on change `upload_image` re-uploads with
  the painted level buffer → `ScalarMask` (`scalar_mask_from_level_buffer`, oversize lazily clipped to the
  image shape) applied as NaN-holes — the identical 6B-1 pre-upload representation (silx `getValueData`,
  `items/image.py`). Toolbar `set_mask_draw` toggle enters/leaves `MaskDraw` via `actions::mode` (Pencil
  tool on entry, None+Zoom on exit) with pencil/eraser/brush-size/clear controls while active.
- Adversarial review found 1 fix-needed, fixed at source on `main` as one commit (`31815f7`): the reused
  Wave-4 `handle_interaction` painted an inline circular brush at the current cell each frame with no
  inter-frame memory, so a fast pencil/eraser drag left gaps — unlike silx (`MaskToolsWidget.py:848-876`)
  tracking `_lastPencilPos` + `updateLine`. **Structural fix** (remove the dual painting impl): route
  on-plot pencil/eraser through the existing faithful `update_line`/`update_disk` primitives with a
  `last_pencil_pos` anchor — `paint_pencil_point` interpolates a thick Bresenham line from the previous
  sample (silx `updateLine`, width = brush size) then stamps a disk of radius `brush_size/2` (silx
  `updateDisk`); `end_pencil_stroke` clears the anchor on release / new click (silx resets `_lastPencilPos`
  on `drawingFinished`); `reset_geometry` clears it so a stroke never interpolates across a geometry
  change. Two silx-matching side-effects of using the shared primitives: the brush is now radius
  `brush_size/2` (dots match the line thickness, was 2×), and the eraser clears only the current mask
  level (was any level), matching `updatePoints(mask=False)`.
- Gate: clippy `-p siplot --all-targets` clean (== `--workspace`, sole member), **672 tests pass** (+7:
  5 feature tests + 2 fix regression tests), doctests ok.
- **Deferred (shader, unverifiable without GPU):** mask colormap GPU overlay — the port renders masked
  pixels as NaN-holes (scalar pipeline `nan_color`) instead. Per-point scatter on-plot pencil draw (needs
  `scatter_mask.rs`, a non-owned file; `ScatterView::show_mask_tools` already covers whole-mask + threshold
  ops). **Wave 6 complete.** **Deferred to Wave 7:** Print action, median-filter + pixel-histogram actions,
  SVG/PPM/TIFF figure save (needs a public RenderState/format save path), per-axis X/Y-only autoscale.

### Wave 7A — Figure output: SVG/PPM/TIFF save + Print action
Single worktree-isolated cluster owning `src/core/backend.rs` + `src/render/backend_wgpu.rs` + `high_level.rs`
+ `Cargo.toml` (3 commits); per-item adversarial verify. BASE `2de55f6`. Recon found the figure-save
primitives already complete — this wave is wiring + one new dep.
- SVG/PPM/TIFF figure save (NO new dep): the four `encode_{png,ppm,svg,tiff}` encoders and
  `render::save::save_graph_with_format` (one GPU readback → centralized format match) already existed &
  were tested; only the UI entry point dropped non-PNG. Added `Backend::save_graph_with_format` (trait +
  the sole `WgpuBackend` impl — `rg` confirmed one impl, no headless backend) + a `PlotWidget` passthrough;
  `save_to_path` now routes every `SaveTarget::Figure(fmt)` through it at `DEFAULT_SAVE_DPI=96` (PNG stays
  byte-identical, same `encode_png`); `save_dialog` gained PPM/SVG/TIFF rfd filters. Faithful to silx
  `SaveAction._saveSnapshot → plot.saveGraph(filename, fileFormat)` (actions/io.py:225-242). SVG remains the
  intentionally raster-wrapped encoder (true vector export still deferred). Pure test: the extension→
  `SaveFormat` dispatch table (png/ppm/svg/tif/tiff→Figure, csv→CurveCsv, pdf/noext→None) asserted without
  a GPU; the readback + write are shims.
- Print action (user-approved `printers` crate, v2.3.0 — verified to build on macOS: wraps CUPS/winspool).
  `print_graph` mirrors silx `PrintAction.printPlot` (actions/io.py:809-846): silx renders the plot to a PNG
  (`_plotAsPNG`) and draws that bitmap onto the printer via `QPainter`/`QPrinter` (RASTER, not vector). Here
  the figure is rasterized to a process-unique temp PNG via the existing `save_graph`, then submitted to the
  default printer (`get_default_printer` → `print_file`); returns `Ok(false)` with no default printer.
  `ToolbarIcon::Print` + `ToolbarResponse.print` + toolbar button (Save-button pattern, result ignored).
  Pure test: the temp-path naming (`print_temp_png_path`); the GPU readback + printer submit are untested
  platform shims. `printers::PrintersError` has no `Display`, so its `.message` field is used in the error
  string; printer-submit errors reuse `SaveError::Readback` (internal, toolbar ignores it) to avoid a
  `SaveError` enum-variant fanout. Print preview / `QPrintDialog` printer-settings UI intentionally omitted.
- Both adversarial reviewers returned `accept`; the integration diff was ALSO reviewed by hand before the
  fast-forward (the verify reviewers had been handed absolute worktree paths as the `git diff` pathspec,
  which can silently empty the diff — fixed for 7B by reporting repo-relative paths). No findings.
- Gate: clippy `-p siplot --all-targets` clean (printers compiled), **674 tests pass** (+2), doctests ok.
- **Deferred:** true VECTOR SVG (re-emit geometry); PDF/PS/EPS/JPEG (matplotlib-only, `from_extension`
  rejects); print preview / printer-settings dialog (no egui `QPrintDialog`). **Wave 7B next:** per-axis
  X/Y autoscale toggles + the widget-reset flag-respect fix; median filter (crate) + pixel-intensity
  histogram.

### Wave 7B — Per-axis X/Y autoscale toggles + widget-reset structural fix
Single worktree cluster owning `high_level.rs` + `actions/control.rs` (2 commits); per-item adversarial
verify (both accept, `structuralNotPatch` true). BASE `ec1b854`. `core/plot.rs` untouched — the model was
already complete.
- **Structural fix (the real bug):** the per-axis autoscale flags `x/y/y2_autoscale` (all default `true`)
  were honored by the model owner `Plot::reset_zoom_to_data_range` (tested) but the WIDGET reset path
  (`apply_limits_from_data_bounds`) **bypassed** them — it called `set_limits_internal` writing both axes
  unconditionally, so the flags were dead at the widget level (two reset paths, one flag-blind). Fixed by
  the PREFERRED single-owner delegation: a new pure `data_range_from_bounds(DataBounds)→DataRange` (per-axis
  `as_non_degenerate`, `None` for a dataless axis) feeds `reset_zoom_to_data_range`, with the `LimitsChanged`
  event preserved (`limits_snapshot` + `push_limits_changed_if`). Verified safe: `WgpuBackend::set_limits`
  only assigns `plot.limits`/`plot.y2` — the same two fields the model owner writes — and the event is the
  only widget-side bookkeeping, so delegation regresses nothing. Default case (all flags on + zero margins)
  is byte-identical to the old path; the only behavior change is the bug fix (off-axes now pinned) plus
  non-zero `data_margins` now applied on reset (matches silx `_forceResetZoom`, zero by default). The flag/
  log-force/margins logic now lives in exactly ONE place. Faithful to silx `PlotWidget.resetZoom`
  (PlotWidget.py:3352-3403). Pure tests: x-off/y-on keeps X refits Y; converse; all-off no-op; degenerate
  padding.
- Per-axis X/Y autoscale toolbar toggles: `ToolbarIcon::AutoscaleX/AutoscaleY` (+ `draw_autoscale_icon`
  glyph; the only exhaustive `ToolbarIcon` match got the two arms, `size()` uses a wildcard), checkable
  buttons (selected = `plot().x_autoscale()`), `ToolbarResponse.autoscale_x_changed/autoscale_y_changed`,
  + pure `toggle_x_autoscale`/`toggle_y_autoscale` in `control.rs`. Mirrors silx `XAxisAutoScaleAction`/
  `YAxisAutoScaleAction` (control.py:172-223) `_actionTriggered(checked)`: `setAutoScale(checked)` then
  reset-zoom ONLY on enable (disable pins the current view); the reset routes through the now-flag-aware
  widget path. Pure tests: enable refits only the enabled axis; disable does not reset.
- Gate: clippy `-p siplot --all-targets` clean, **680 tests pass** (+6), doctests ok (no doctest changed).
- **Deferred:** Y2 (right-axis) autoscale toolbar button (model flag already honored; a third button would
  bloat the toolbar row). **Wave 7C next:** median filter (`medians` crate) + pixel-intensity histogram.

### Wave 7C — Median filter + pixel-intensity histogram (FINAL substantive Wave-7 sub-wave)
Single worktree-isolated cluster owning new `src/widget/actions/analysis.rs` + `high_level.rs` +
`actions/mod.rs` + `Cargo.toml` (3 commits); per-item adversarial verify (both `accept`, no issues). BASE
`3d27137`. The pure compute is the tested deliverable; the kernel/conditional popup, the histogram window,
and the GPU image re-upload are UI/GPU shims. Agent read silx `silx.math.medianfilter` (`.pyx` defaults +
the C++ `median<T>()` in `include/median_filter.hpp`) and `actions/{medfilt,histogram}.py` for ground truth.
- **Median filter** (user-approved `medians` v3.0.12, build-verified on macOS): pure `median_filter_2d(data,
  w, h, kernel_h, kernel_w, conditional)` + `median_filter_1d` (= 2D with kernel `(kw, 1)`, silx
  `MedianFilter1DAction`) in `analysis.rs`. Faithful to silx `medfilt2d` for the **default `mode='nearest'`**
  that `MedianFilterAction` relies on (calls `medfilt2d` with no `mode`): out-of-bounds window indices clamp
  to `[0, dim-1]`; window median = `sorted[n/2]` (`std::nth_element` semantics — the *higher* of two
  centrals, not an average). Odd NaN-free windows use the crate's `Medianf64::medf_unchecked`; even/NaN-
  reduced windows use `select_nth_unstable_by(n/2)` because the crate's even path AVERAGES the two centrals
  (would diverge from silx) — pinned by `nan_ignored_even_count_takes_higher_central` ({10,20}→20). NaN
  ignored in-window; all-NaN window→NaN. `conditional`: replace center only if it equals the window min or
  max (a NaN center is never an extremum, so it propagates). `apply_median_filter`/`_1d` (on `PlotWidget`)
  read `RetainedItemData::Image` and re-upload in place via the existing `update_image_spec` (silx
  `addImage(replace=True)`) — no non-owned file. `MedianFilterParams` (kernel_width odd, conditional;
  default 3×3) + `Plot2D::show_median_filter`/`show_median_filter_toolbar` popup (silx `MedianFilterDialog`;
  spinbox min 1 step 2 enforced by `force_odd`). 11 unit tests (salt-spike removal, sorted-middle interior,
  NEAREST corner clamp, constant image, conditional keep-vs-fix, 1D≡2D-(k,1), 1×1 identity, NaN cases).
- **Pixel-intensity histogram**: pure `pixel_intensity_histogram(pixels, n_bins) → Option<PixelHistogram>`
  (edges/counts/min/max/mean/std/sum/n_bins) in `analysis.rs`. Faithful to silx `PixelIntensitiesHistoAction`
  / `HistogramWidget._updateFromItem` (histogram.py:226-419): bins = `min(1024, floor(sqrt(finite_count)))`
  then `max(2, …)` (a `Some(n)` override also floored at 2); range = finite `(min,max)` (NaN/±inf excluded
  from range, counts AND stats); `last_bin_closed=True` (max lands in the last bin, interior `v` →
  `floor((v-min)·n_bins/range)`); degenerate `min==max` range enlarged exactly as silx (`(-0.01,0.01)` at 0,
  else sorted `(v·0.99, v·1.01)`); stats = `nanmean`/population `nanstd`(ddof=0)/`nansum`; `None` on all-not-
  finite (silx `reset`). `active_image_histogram` (on `PlotWidget`) reuses `get_image_pixels_raw`;
  `Plot2D::show_pixel_histogram`/`_toolbar` is a CPU/egui-painter shim (bars in silx `#66aad7` + stats +
  editable bin count, NO second GPU `Plot1D`). 9 unit tests (sqrt-floor bins, exact last-bin-closed counts,
  stats, non-finite exclusion, degenerate ranges, all-not-finite None, bins-floored-at-2, interior floor).
- **Self-review fix (caught in my own integration diff, not by the reviewers — 1 commit `d0b86a9`):** the
  branch added `ToolbarResponse.median_filter_applied`/`pixel_histogram_open` but **nothing ever set them** —
  the buttons live on `Plot2D`'s standalone `show_*_toolbar` methods (own bool returns), while the field-
  producing `show_toolbar_controls` is on `PlotWidget` (shared with curve-only `Plot1D`, which silx never
  shows image actions on). Removed the dead fields (their doc comments falsely claimed "this frame"
  semantics); the `Plot2D` toolbar methods remain the API. f64-vs-float32 note: silx casts to float32 before
  `Histogramnd`; this port stays f64 (spec says `pixels: &[f64]`) — binning algebra is exact, a boundary
  value can shift ≤ float32-epsilon vs silx.
- Gate (main, post-fix): fmt clean, clippy `-p siplot --all-targets` clean (medians compiled),
  **700 tests pass** (+20: 11 median + 9 histogram), no doctest changed (new fences are ```ignore).
- **Deferred:** non-default silx median edge modes (reflect/mirror/shrink/constant — `MedianFilterAction`
  only uses `nearest`); silx `HistogramWidget` RangeSlider for custom range, weighted (count·value)
  histograms, and the embedded full `Plot1D`. **Wave 7 is now complete** (7A/7B/7C). Remaining backlog =
  the shader-gated items (SOLID scatter-triangle pipeline, per-point scatter alpha, mask colormap GPU
  overlay — NaN-holes) + marker drag/cursor + `is_overlay` data-layer (render-refactor) + true vector SVG.

### Wave 8 — Shader-gated GPU cluster (4 sequential sub-waves, all share the `high_level.rs` long-pole)
Verification boundary for this wave: WGSL is now **statically** validated headlessly via `naga` (8A) — the
prior "WGSL RUNTIME-UNVERIFIED (no naga CLI)" caveat is partly obsolete — but actual rendered pixels, GPU
alpha-blend, pipeline creation, and the per-vertex marker DRAW remain **GPU-only UNVERIFIED** on this machine
(Metal, no headless rasterizer). CPU geometry/data/LUT math is fully unit-tested. Each sub-wave = one
worktree-isolated implement + parallel adversarial verify; ff-merged to `main` preserving verified SHAs.
- **Wave 8A — headless naga WGSL validation gate** (`c253950`, 707 tests). New test-only `src/render/shaders.rs`
  (registered `#[cfg(test)] mod shaders;` in `render/mod.rs`): `validate_wgsl(name, src)` =
  `naga::front::wgsl::parse_str` + `Validator::new(ValidationFlags::all(), Capabilities::all())`, one `#[test]`
  per shader (clear/curve/errorbars/fill/image/image_rgba/markers) via `include_str!`. **Zero new deps** —
  `naga` is re-exported through `egui_wgpu::wgpu`. A malformed shader now fails `cargo nextest` headlessly.
- **Wave 8B — per-point marker color + per-point scatter alpha** (`62bc335` + `baf1715`, 712 tests). Fixes the
  latent bug that the marker pipeline **dropped** per-vertex colors: `markers.wgsl` gains
  `use_vertex_color: f32` (consumes a `_pad` slot — stays 112-byte std140, guarded by
  `marker_params_std140_size_unchanged`) + `@binding(2) vcolors` storage buffer; `vs_main` passes
  `vcolors[min(inst, len-1)]` through a color varying; `fs_main` = `select(params.color, in.color,
  use_vertex_color > 0.5)` (regression-safe: flag 0 ⟹ old behavior). `gpu_curve.rs` adds the binding +
  shares the existing per-vertex color buffer. `high_level.rs`: `compose_per_point_alpha` (round-trips
  `to_srgba_unmultiplied` to avoid double-premultiply) + `ScatterView.alpha: Option<Vec<f64>>` +
  `with_alpha`/`set_alpha`/`clear_alpha`; silx three-stage `colormap.alpha · per_point.alpha · global.alpha`
  (`scatter.py __applyColormapToData rgbacolors[:,-1] *= __alpha`). **UNFIXED (out of scope, recorded):** the
  pre-existing `apply_curve_alpha` double-premultiply bug on the shared curve/line path (kodex 162cc1a8).
- **Wave 8C — SOLID scatter triangle surface** (`6575cef`/`e5cc70d`/`5fc178b`, 717 tests; `high_level.rs` only).
  `ScatterVisualization::Solid` rendered through the **existing CPU `epaint::Mesh` path** (no GPU shader
  needed — recon corrected the assumption). Extracted `point_colors(values, colormap, alpha)` shared by the
  Points + Solid arms (no drift); `ScatterView.triangles_handle` with the invariant *Some iff a mesh is
  displayed*; Solid arm calls `scatter_viz::solid_triangles`, `None` on <3 finite / collinear clears the
  handle and draws nothing (silx "Cannot display as solid surface" early-out). silx `scatter.py:610-625`
  `backend.addTriangles` GL Gouraud. Enum-leak match arms fixed (`Points | Solid => None`; explicit Solid arm).
- **Wave 8D — mask 256×4 LUT overlay** (`12a7607` + `e38226a`, then 3 review fixes; 724 tests). Replaces the
  flat single-color boolean overlay (which collapsed all 255 levels) with silx's faithful per-level LUT.
  `core/colormap.rs`: `mask_overlay_lut(base, overrides, selected_level, alpha) -> [[u8;4];256]` faithful to
  `_BaseMaskToolsWidget._setMaskColors` (base RGB → per-level override → `alpha/2` for all → full `alpha` at
  the selected level → `lut[0]=[0,0,0,0]` set LAST; float→u8 = `(x*256).clamp(0,255) as u8` truncation
  matching numpy `clip(c*256,0,255).astype(uint8)`). `mask_tools.rs::apply()` rewired to discrete
  `lut[level] → RGBA` (**Option B**: reuses `image_rgba.wgsl`, NOT a scalar+colormap image — the
  linear-filtering LUT sampler would blend adjacent levels and corrupt the selected-level alpha). New fields
  `alpha` (default 0.8 = silx slider 8/10) + `overrides: Vec<Option<[u8;3]>>`(256); setters
  `set_transparency`, `set_mask_colors(rgb, Option<u8>)`, `reset_mask_colors(Option<u8>)`; pure helpers
  `mask_overlay_rgba` + `overlay_z_value`; z = active-image z + 1 (silx `MaskToolsWidget.py:482`, fallback
  `_z=1`). `high_level.rs`: additive `add_rgba_mask`. Default overlay color corrected to silx
  `rgba("gray")` = `#a0a0a4` (160,160,164) (gui/colors.py:71), not `#808080`. **Post-integration review fixes
  (one commit per finding):** `71c52de` wrong silx gray (`#808080`→`#a0a0a4`); `33f1ed6` tautological z test →
  test coupled to the real `overlay_z_value` helper; `8d671ef` `reset_mask_colors` per-level fidelity (silx
  `resetMaskColors(level)`). UNVERIFIED (GPU): on-screen compositing, alpha blend, draw-on-top z-layering.
- **Wave 8 complete** (8A/8B/8C/8D), `main` @ `8d671ef`, 724 tests, full-workspace gate green. Remaining
  backlog = marker drag/cursor (needs ownership decision) + `is_overlay` data-layer + `DirtyState::Overlay`
  + true vector SVG export + `delaunator` robustness upgrade (separately approvable).

### Wave 9 — Legend right-click context menu (silx `LegendSelector`)
Single cluster, all in `high_level.rs` (the single-writer long-pole), one worktree-isolated implement +
two parallel adversarial reviews (both **accept**), ff-merged preserving verified SHAs + one review-fix
commit. Recon confirmed **all 7 actions were already primitively backed** (no backend/render/shader change):
`CurveData` carries `symbol: Option<Symbol>` / `line_style: LineStyle` / `y_axis: YAxis`, and `GpuCurve::draw`
(gated on `line_style.draws_line()`) and `draw_markers` (gated on `symbol.is_none()`) are separate CPU-side
draw calls re-synced by the existing `update_curve_data` path. Verification boundary: PlotWidget cannot be
constructed without a GPU `RenderState` (no crate test builds one), so the egui menu render, the right-click
interaction, the rename `egui::Window`, and the on-screen toggle/axis-move EFFECT are **GPU/UI-UNVERIFIED**
on this machine; the testable core — the pure style transforms — is fully unit-tested.
- **`f10185c` — per-curve style setters + lossless restore cache** (730 tests; 6 new pure-fn tests). Public
  `set_curve_y_axis(handle, YAxis)` (recomputes data bounds + auto limits like `remove`, since Left↔Right
  reassigns which Y/Y2 bounds the curve feeds), `set_curve_points_visible`, `set_curve_lines_visible`.
  Checkable Points/Lines are **lossless**: pure free fns `set_symbol_visibility` / `set_line_visibility`
  stash the visible `Symbol`/`LineStyle` in a UI-only restore cache (`ItemRecord.hidden_symbol` /
  `hidden_line_style`) so hide→show restores the exact variant (e.g. `Dashed`, not `Solid`), falling back to
  documented defaults (`Symbol::Point` / `LineStyle::Solid`) only on an empty cache. Tests assert exact
  variants + no-op-does-not-clobber + empty-cache-default boundaries. **No dual meaning:** the cache is never
  read by any render/bounds/legend path — `CurveData.symbol`/`.line_style` stays the single source of truth.
- **`fd81309` — legend context menu + rename popup + response reporting**. `pub enum LegendAction`
  (re-exported in `lib.rs` beside `LegendResponse`); `legend_row_response` now returns the row
  `egui::Response` so `show_legend` attaches `Response::context_menu` while holding `&mut self`. Curve rows
  (`PlotItemKind::Curve`) get the full silx `LegendListContextMenu` set — Set Active (disabled if already
  active), Map to Y Left / Map to Y Right (current axis disabled), checkable Points (`symbol.is_some()`) and
  Lines (`line_style.draws_line()`), separator, Rename, Remove — re-read from the record each frame so a
  reopened menu is current. Non-curve rows (Image/Scatter/Histogram/Mask) get a graceful subset (Set Active /
  Rename / Remove). Rename = silx `RenameCurveDialog` as an `egui::Window` (single-line `TextEdit` +
  Apply/Cancel, Enter applies, Escape/close cancels) driven by `PlotWidget.rename_state`. Each action
  self-applies and records `(handle, LegendAction)` in `LegendResponse.context_action`.
- **`71f5653` — review fix (one commit, structural):** the visibility setters wrote the restore cache back to
  the record *before* `update_curve_data`; a failed update would have desynced the cache from the unchanged
  drawn state. Now the cache is read by copy/clone and committed only on the update-success path (strong
  state transition: the side cache commits through a finalizer that runs only after the real transition lands).
- **Wave 9 complete**, `main` @ `71f5653`, 730 tests, full-workspace gate green (`-p siplot` == sole
  workspace member). The on-screen menu/rename/toggle render is GPU/UI-UNVERIFIED (see boundary above).

### Wave 10 — Active-curve highlight styling (silx `HighlightedMixIn` / `getCurrentStyle`)
Single cluster, all in `high_level.rs`; one worktree-isolated implement + two parallel adversarial reviews
(both **accept**), ff-merged preserving verified SHAs. **No review-fix commit was owed** — the two findings
were a verified harmless no-op and a GPU-blocked test gap (below). Recon confirmed no backend/render/shader
change: `CurveData`'s `color`/`width`/`line_style`/`symbol`/`marker_size`/`gap_color` are all normal per-curve
fields already pushed by the existing `backend.update_curve`, so the highlight is expressible purely as a
render-time style overlay. Verification boundary: PlotWidget needs a wgpu `RenderState` no crate test builds,
so the GPU push, the `set_active_item` revert/apply wiring, the public setters, and the on-screen thicker line
are **GPU/PlotWidget-UNVERIFIED**; the testable core (the pure merge) is unit-tested.
- **`d852b04` — highlight machinery + pure merge + state + setters + tests** (734 tests; 4 new pure-fn tests).
  `pub struct CurveStyle` (override style: `Option` color/line_width/line_style/symbol/symbol_size/gap_color,
  each `None` = inherit; re-exported in `lib.rs`). Pure free fn `current_curve_style(base, highlight,
  highlighted) -> CurveData` = silx `getCurrentStyle`'s per-field merge exactly (when highlighted, each style
  field = highlight's value if `Some` else base's; when not, base unchanged; data fields x/y/colors/fill/
  baseline/errors/y_axis pass through). State: `active_curve_style` default `{ line_width: Some(2.0), ..None }`
  (silx `DEFAULT_PLOT_ACTIVE_CURVE_LINEWIDTH=2` / `DEFAULT_PLOT_ACTIVE_CURVE_COLOR=None`) + `active_curve_handling`
  default `true`; getters `active_curve_style()`/`is_active_curve_handling()`; single-owner helper
  `sync_curve_highlight`; public setters `set_active_curve_style`/`set_active_curve_handling`. Tests assert
  exact per-field values (not-highlighted→base verbatim; default→only width 2.0 with all else inheriting;
  multi-field override with width inheriting). *(Commit-split note: the owner + public setters landed in this
  commit, not commit 2, because commit 1 would otherwise carry a private `current_curve_style` with no
  non-test caller → `clippy -D warnings` dead-code failure; `#[allow]` is banned. Net diff unchanged.)*
- **`d676c00` — wire the overlay into the automatic transitions**. **Invariant:** GPU render style of a curve
  == `current_curve_style(retained_base, active_curve_style, is-active-curve)`; the retained `record.curve_data`
  is ALWAYS the base (the owner pushes the resolved style to `backend.update_curve` only, never
  `set_record_curve_data`) — single source of truth, no dual meaning (both reviewers traced
  activate→update→deactivate: the base width is never corrupted). `set_active_item` reverts the previous curve
  to base + applies the highlight to the new one through the owner; `update_curve_spec` re-overlays when the
  updated handle is the active curve (so an update keeps the highlight), leaving non-active curves' base push
  unchanged. Gated strictly on `PlotItemKind::Curve` (silx highlights only `kind=='curve'`; Scatter/Histogram,
  though `is_curve_like`, are never highlighted).
- **Reviewer findings, no fix owed:** (1) MINOR — the setters resolve the target via `active_curve()`
  (`is_curve_like`) which is broader than the strict `==Curve` highlight gate; both reviewers confirmed
  harmless (the owner re-derives the strict gate and pushes an idempotent base for non-Curve handles, never a
  dual-meaning record write). (2) GAP — the "retained base never corrupted" invariant is verified by code
  tracing, not a headless test; reviewer 2 suggested it "could be tested without a GPU" but that is mistaken —
  `record_curve_data`/`set_active_item`/`update_curve_spec` are all PlotWidget methods and `PlotWidget::new`
  requires a `&RenderState` (live wgpu device) with no headless constructor, so the regression test is
  GPU-blocked like the whole widget layer. Recorded honestly rather than faked with a GPU-requiring test.
- **Wave 10 complete**, `main` @ `d676c00`, 734 tests, full-workspace gate green. The on-screen highlight
  render is GPU/PlotWidget-UNVERIFIED (see boundary above).

### Wave 11 — Marker drag + cursor (silx `DraggableMixIn.drag` → `setPosition`)
Cross-cutting cluster (6 source files + 3 examples); one worktree-isolated implement + two parallel adversarial
reviews (**both accept**, all six dimensions true), ff-merged preserving verified SHAs, then **one review-fix
commit** for a structural finding (below). **Key architecture:** the marker source of truth is
`WgpuBackend.items` (`BackendItem::Marker`); `plot.markers` is a per-frame, z-sorted **mirror** that
`sync_plot_items` rebuilds every frame, so a drag persists through a new inherent
`WgpuBackend::update_marker` (mirrors `update_image`), never by mutating the mirror alone. A parallel
`plot.marker_handles: Vec<ItemHandle>` (populated alongside `plot.markers` in `sync_plot_items`, same
length+order) bridges the handle-less mirror back to backend identity so `apply_interaction` (which holds only
`&mut Plot`) can report the dragged marker. Verification boundary: `PlotWidget`/`PlotView` need a wgpu
`RenderState` no crate test builds, so the grab/drag/precedence state machine, the cursor side-effect, the
`update_marker` round-trip, `set_marker_position`/`marker_position`, and the `PlotResponse`/event wiring are
**GPU/PlotWidget-UNVERIFIED**; the pure core is unit-tested.
- **`eb924e8` — persistence + handle plumbing + pure helpers + public API + tests**. `Plot::marker_handles`;
  `WgpuBackend::update_marker`/`marker` (inherent, no trait change); pure `Marker::pick` per-kind hit-test in
  `core/marker.rs` (backend `pick_marker` now delegates to it — dedup, not duplicate); pure
  `interaction::marker_at` (topmost **draggable** marker under cursor, rev/z-order, skips non-draggable) and
  `interaction::marker_cursor` (drag-DOF size cursor); `PlotEvent::MarkerMoved`; public
  `set_marker_position`/`marker_position` (silx `setPosition`/`getPosition`, constraint applied via
  `Marker::drag`). Unit tests: `Marker::pick` (in/out radius, on/off span for v/h-line), `marker_at`
  (topmost / skip-non-draggable / none-on-miss), `marker_cursor` (all 5 kind+constraint mappings).
- **`8f6318a` — wire the drag into `apply_interaction`**. A draggable marker under the cursor at drag-start is
  the **highest-precedence primary-drag consumer in every mode except MaskDraw**, pre-empting primary-drag pan,
  the ROI-edge grab, and box-zoom (all gated on `!marker_dragging`); the marker size cursor takes precedence
  over the ROI-edge cursor. Wheel zoom and secondary-drag pan are intentionally **not** gated (silx: only the
  left-button item-drag branch competes with primary pan/zoom). `PlotWidget::show` persists the dragged marker
  back to the backend item via `update_marker` and emits `PlotEvent::MarkerMoved`. *(Commit-split note:
  `PlotEvent::MarkerMoved` + the public API landed in commit 1, not 2, because commit 1's `set_marker_position`
  pushes the variant — an unknown variant would not compile; `#[allow]` is banned. The 3 examples gained a
  `MarkerMoved` match arm as a forced `--all-targets` compile consequence of the exhaustive enum.)*
- **`994c591` — review-fix (structural): key marker drag by stable handle, not mirror index.** **Finding
  (reviewer 2, non-blocking):** `MarkerDrag` stored a `usize` index into `plot.markers`, the per-frame z-sorted
  mirror; reusing that positional value across frames as the grabbed marker's identity is a dual meaning, so a
  mid-drag rebuild/reorder of the mirror could shift the index onto a different marker (bounds-checked, no
  crash, but wrong-marker move). **Structural fix (preferred over the index patch):** store the marker's stable
  `ItemHandle` in `MarkerDrag` and re-resolve the mirror index from it each frame (drag-apply + drag cursor); a
  marker removed mid-drag now no-ops instead of moving an adjacent one. **Anchor audit:** `RoiDrag.roi` is the
  same shape but **distinct** — it indexes `plot.rois`, the source of truth `sync_plot_items` never rebuilds or
  reorders (not a derived mirror), and is pre-existing (out of Wave 11 scope) → left as-is (see UNFIXED).
- **UNFIXED (pre-existing, out of scope):** `RoiDrag` still keys an in-progress ROI drag by `usize` index into
  `plot.rois`; robust today because that Vec is the truth and is not reordered per frame, but a handle/id-keyed
  `RoiDrag` would be the same structural improvement. Not touched (Wave 11 scope is markers; needs user OK).
- **Wave 11 complete**, `main` @ `994c591`, 741 tests, full-workspace gate green. The on-screen drag, cursor,
  `update_marker` round-trip, and event/response wiring are GPU/PlotWidget-UNVERIFIED (see boundary above).

### Wave 12 — On-plot ROI creation (all 11 shapes) + whole-ROI translate (silx `RegionOfInterestManager.start` → `setFirstShapePoints`)
One worktree-isolated implement + two parallel adversarial reviews (**both accept**, all six dimensions —
faithfulToSilx / additiveOnly / testMeaningful / precedenceCorrect / mappingComplete / unverifiedHonest — true),
ff-merged preserving the verified SHAs, then **one review-fix commit**. **Key architecture:** the `DrawState`
gesture machine (`DrawMode`/`DrawParams`/`on_press`/`on_move`/`on_release`/`preview`) and `show_with_draw`
already existed (Wave 4), so this wave is a thin *caller-side* `DrawParams`→`Roi` mapping (a pure, testable core
function) plus a `PlotInteractionMode::RoiCreate(RoiDrawKind)` mode that **reserves the primary drag** for the
draw gesture (the same shape as Wave 6C-2's MaskDraw). Unlike markers, `plot.rois` **is the source of truth**
(`sync_plot_items` never rebuilds or reorders it), so `RoiDrag` correctly keys by `usize` index into it — no
mirror/handle indirection is needed here. **Verification-boundary correction (vs the Wave-11 note):**
`apply_interaction` takes `&mut Plot` (a pure core type), so it *is* headlessly testable via
`egui::Context::default()` + `ctx.run_ui` — wgpu paint callbacks are recorded into the painter, never executed —
and the agent added 5 headless wiring tests (`run_mode_frame`) exploiting this. Only `PlotWidget::show` (needs a
wgpu `RenderState` no crate test builds) and the actual on-screen render stay GPU/PlotWidget-UNVERIFIED.
- **`7636cbd` — ROI-creation pure mapping (testable core).** `RoiDrawKind` (11 kinds, one per `Roi` variant);
  `roi_draw_mode(kind)->DrawMode`; `roi_from_draw(kind, &DrawParams)->Option<Roi>` (the silx `setFirstShapePoints`
  equivalent for all 11, incl. `arc_from_two_points`, a faithful port of silx `_circleEquation` /
  `_createControlPointsFromFirstShape` circle-fit); `RoiGrab { Edge(RoiEdge), Translate }` +
  `roi_grab_at(rois, transform, cursor, grab_px)->Option<(usize, RoiGrab)>` (handle-over-body, topmost-first);
  new `DrawMode::Point` + `DrawParams::Point { x, y }` (single click → `Finished` immediately, for Point/Cross).
  16 pure unit tests.
- **`c00ec50` — wire on-plot creation + whole-ROI translate into `apply_interaction`.** The `RoiCreate` block
  drives `DrawState` from temp memory, appends the finished `Roi`, **re-arms continuously** (silx default), and
  surfaces a live `roi_preview`; `RoiDrag` gains `RoiGrab::Translate` → `Roi::translate(delta)` alongside the
  existing edge-move. `mode_grabs_roi_edge` excludes `RoiCreate` (so a draw gesture is never stolen by an edge
  grab); new `mode_allows_marker_drag` keeps marker drag out of create mode. `Interaction.roi_created` /
  `roi_preview` added; preview rendered via `draw_overlay`. `run_mode_frame` test helper + 5 headless
  apply-interaction wiring tests.
- **`1512ed7` — public API + event + re-exports.** `PlotEvent::RoiCreated { index }` emitted in
  `PlotWidget::show` (mirrors `RoiChanged`); `set_roi_create_mode(kind)`; `lib.rs` re-exports `RoiDrawKind`,
  `RoiGrab`, `roi_draw_mode`, `roi_from_draw`, `roi_grab_at`, `arc_from_two_points`. *(Commit-split note:
  `RoiCreated` forced a `--all-targets` match arm in the 3 examples — exhaustive-enum consequence, same shape as
  Wave 11's `MarkerMoved`.)*
- **`7ff440d` — review-fix (R1.1): strengthen the Point ROI-create test.** The reviewer's literal suggestion
  (assert `roi_created == Some(0)` on the *press* frame) could be wrong — a no-move click likely fires `on_press`
  via `clicked()` on the *release* frame, so a frame-pinned assertion is fragile. Fixed instead with a
  frame-agnostic "reported **exactly once**" assertion (collect `roi_created` across both press and release
  frames, assert `== vec![0]`), which catches both a missing report and a double-create regardless of egui frame
  collapse. Verified passing.
- **Documented deviations (silx-cited, not guessed):** `Roi::Ellipse` is axis-aligned (no orientation; does not
  model `EllipseROI._orientation`); X/Y bands use `Roi::VRange`/`HRange` (silx shows single-line `VerticalLineROI`/
  `HorizontalLineROI` as X/Y markers — distinct single-line kinds are still ☐); Arc/Band/Circle default geometry
  (widths, radii, angle conventions) read from silx source and cited in the mapping doc-comments.
- **UNFIXED (pre-existing, out of Wave 12 scope):** *(R2.1)* the `RoiDrag` apply block reads the in-progress drag
  from temp memory **unconditionally of mode**, so a mid-drag mode switch can apply a stale grab one frame; this
  is identical to `main`'s pre-Wave-12 behaviour (the same pattern the MarkerDrag/MaskDraw blocks use) and was not
  introduced here. Also carried from Wave 11: `RoiDrag` keys by `usize` index — safe because `plot.rois` is the
  truth, not a per-frame mirror, but a future id-keyed `RoiDrag` would be the uniform structural choice.
- **Wave 12 complete**, `main` @ `7ff440d`, 762 tests, full-workspace gate green (clippy `-D warnings` clean,
  doctest ok). The on-screen creation, preview overlay, continuous re-arm, and whole-ROI translate are
  GPU/PlotWidget-UNVERIFIED; the `DrawParams`→`Roi` mapping and the `apply_interaction` wiring are headlessly
  unit-tested.

### Post-Wave-12 commits (landed on `main`, unlogged until the 2026-06-04 re-baseline)
These shipped after the Wave-12 progress entry as individual one-commit-per-feature fixes (not run as a
labelled wave), so they were not in the Progress log; the 2026-06-04 re-baseline folds them into the Done count.
- **ROI handle-drag editing** for the kinds Wave-12 creation left ◐: `e9b046b` Rect corner/diagonal resize,
  `6b9e5d0` Band, `fc14807` Arc (incl. start/end-angle via `Vertex(2)`/`Vertex(3)`), `cac2edf` Circle/Ellipse
  regression tests; `2dbbb8e` cancels an in-progress ROI drag on a mid-drag mode switch (**closes the Wave-12
  R2.1 UNFIXED** — the stale-grab-one-frame window).
- **`cd0338d`** fixes the `apply_curve_alpha` double-premultiply on the shared curve/line path (**closes the
  long-standing cross-wave UNFIXED**, kodex `162cc1a8`).
- **Detached tool windows:** `4c39632` detached-window helper sharing placement maths with `profile_window`;
  `dea42fd` shows all floating tool windows as detachable native OS windows; `a8bb863` doc-example update.
- **Active-curve axis labels** (silx `_setActiveItem`): `2de1a8e` swap in the active curve's labels, `f2ce5b5`
  example; **right-click zoom menu** `b389721` (replaces double-click reset).
- **Rotated-label centering fixes:** `b7b62bd` off-center Y / clipped y2 labels; `fbe7f6e` wrong-sign vertical
  colorbar legend centering.
- **Curve legend icons** (silx `CurveLegendsWidget`/`LegendIcon`): `6fb9f33` line style + color + marker,
  `c0f38dc` example, `525dfac` legend in `high_level_active_curve_labels`, `d04232a` drop the icon bounding box.

### Re-baseline (2026-06-04, recon workflow `wf_b2b09dc2-620`)
The "Remaining work (live, code-verified 2026-06-02)" view was stale (frozen pre-Wave-10, before the commits
above). An 8-slice read-only recon (Explore agents, one per silx.gui.plot slice → synthesis) re-derived every
status from the code on `main`, ignoring the old status column. Result: **≈191 Done · 130 open** (92 enumerated
rows: 10 H / 48 M / 34 L). The new view replaced the 2026-06-02 section at the top of this file; the candidate
next-wave list is recorded there. Two prior cross-wave UNFIXED items were verified **closed** by the post-Wave-12
commits (`apply_curve_alpha` double-premultiply; ROI mid-drag stale-grab). One precision correction over the
recon's raw output: the H-priority "ROI per-instance color/label/selection on canvas" is recorded as a
**two-decoupled-collections** structural gap — `chrome::draw_roi` + `RoiAppearance` already render color
(chrome.rs:732), name (`:916`), and selection-width (`:736`), and `RoiManagerWidget` feeds real appearance
(roi_manager.rs:223-235), but the live plot render (`plot_widget.rs:353 → draw_rois`) passes
`RoiAppearance::default()` over a bare `plot.rois: Vec<Roi>`; the fix is to plumb/join `ManagedRoi` metadata into
the live render path, not to add new render code.

### Wave 13 — ROI on-canvas styling + selection feedback (unify the two decoupled ROI collections)
Done **inline** (not the parallel worktree method), 4 one-feature-per-commit landings on `main` (not pushed),
each passing the full per-crate gate (`cargo fmt --all`; `cargo clippy -p siplot --all-targets -- -D warnings`
clean; `cargo nextest run -p siplot`; `cargo test --doc -p siplot`). Closes candidate-wave #1 — the top
H-priority cluster — via the **structural fix the re-baseline named**: the root cause was two decoupled ROI
collections (`Plot.rois: Vec<Roi>`, rendered live with a hardcoded `RoiAppearance::default()`, vs
`RoiManagerWidget`'s own `Vec<ManagedRoi>` whose `draw()` was never overlaid), so per-ROI styling never reached
the interactive plot.
- **`3c614c8` — Unify ROI collections: `plot.rois` holds `ManagedRoi` as the single source of truth.** Moved
  `ManagedRoi`/`RoiLineStyle` from `widget::roi_manager` into `core::roi` (core may not depend on widget;
  re-exported from `widget::roi_manager` + `lib.rs` for path compat). `Plot::rois` is now `Vec<ManagedRoi>`;
  added `Plot::roi_color` (silx-default red) + private `current_roi`; the one live `chrome::draw_rois(painter, t,
  rois: &[ManagedRoi], default_color, style)` path resolves each ROI's appearance. **Single-owner invariant:**
  `Plot::set_current_roi` is the SOLE writer of every `ManagedRoi.selected`; `Plot::remove_roi` (remove + adjust
  index) and `Plot::clear_rois` (clear + `current_roi=None`) are the SOLE collection-shrinking mutators → "exactly
  the current ROI is highlighted; the current index never dangles" holds by construction. Bypass audit anchor
  `rg '\.rois\.(push|remove|clear)'`: push sites distinct (a new ROI is not auto-current); the two high-level
  remove/clear bypass sites routed through the owner. Adapted every `.roi` reader/writer (plot_widget render +
  edit-drag + creation + cursor, interaction `roi_grab_at`, high_level `add_roi`/`rois`/`rois_mut`/
  `show_roi_manager`, 4 examples). Tests: selection/removal invariant in `core::plot`.
- **`c60bae6` — high-level ROI styling API + current-ROI selection event.** `set_roi_color`/`set_roi_name`/
  `set_roi_line_width`/`set_roi_line_style`/`set_roi_fill` (handle-indexed), `add_managed_roi`,
  `current_roi`/`set_current_roi`; `PlotEvent::CurrentRoiChanged { previous, current }` emitted only when the
  selection actually changes (silx `sigCurrentRoiChanged`). *(Commit-split note: the new variant forced a
  `--all-targets` match arm in 3 exhaustive-match examples — same enum consequence as Wave 11/12.)*
- **`b29db3c` — rework `RoiManagerWidget` onto `plot.rois`; remove the second ROI collection.** The manager is now
  stateless beyond its window (`{ win, open }`): every control (current-ROI radio, color swatch, name, line
  width/style, fill, remove, the +shape and clear-all buttons) mutates the plot's single `ManagedRoi` collection
  via the owner methods, so edits render on the plot at once. Its own `Vec<ManagedRoi>`/`current`/`default_color`
  + dead `draw()` removed (−4 owned-state tests). `examples/high_level_roi_manager.rs` now seeds two styled, named
  ROIs (blue "feature A" rect; orange filled "spot" circle set current) in Select mode.
- **`7495590` — extract testable ROI appearance resolution.** Pure `chrome::roi_appearance(&ManagedRoi,
  default_color) -> RoiAppearance` (color fallback, name→Option, selected, width, style, fill) with a headless
  `mod tests` case — the GPU-render boundary's testable seam.
- **Verification boundary (reported honestly):** `PlotWidget::new` needs a wgpu `RenderState` no crate test
  builds, so the on-screen ROI pixels stay **GPU/PlotWidget-UNVERIFIED**. Headlessly tested = the core
  selection/removal invariant (`core::plot`) + pure `chrome::roi_appearance` resolution. Final gate: 781 tests
  passed / 0 failed, doctest ok (11 ignored), clippy `-D warnings` clean.
- **Wave 13 complete**, `main` @ `7495590` (NOT pushed). Per-crate scope only (`-p siplot` == `--workspace`,
  sole member), so the pre-push full-workspace pass is satisfied by the per-crate run.


## PlotWidget core, axes, frame, ticks  — 25✅ 2◐ 7☐

siplot has strong coverage of core axis features: linear/log/inverted axes, dual Y2 axis, axis labels, grid modes (major/major+minor/none), nice-number tick layout with minor ticks, axis constraints, keep-aspect-ratio, and auto-zoom-to-data. KEY GAPS: no TIME_SERIES/datetime axis support (silx TickMode.TIME_SERIES); no per-axis autoscale control (silx Axis.setAutoScale); no data-margin expansion ratios for resetZoom (silx setDataMargins); no axis visibility toggle (setAxesDisplayed); no timezone support for timestamp axes; no dirty/replot lifecycle (silx _setDirtyPlot/replot/autoreplot); no log-decade minor-tick formatting (silx uses decade sub-multiples); no frame/foreground color split (silx distinguishes axes from grid). Layout and tick mechanics are solid; the missing pieces are ecosystem features (time axes, autoscale per-axis, dirty tracking, data margin ratios) rather than core rendering bugs.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ☐ | H | M | Per-axis autoscale control (Axis.setAutoScale / isAutoScale) | `PlotWidget.py:2935-2957, items/axis.py:310-324` | silx allows per-axis autoscale toggle via Axis.setAutoScale(flag). siplot has only global auto-zoom-to-data (plot.home_limits) and widget-level auto_reset_zoom flag. Missing granular control: X aut |
| ◐ | H | S | Data range computation (getDataRange for bounds auto-expand) | `PlotWidget.py:908-918, resetZoom:3331-3350` | siplot computes and applies data bounds. silx's getDataRange returns (x, y, yright) tuples, each (min, max) or None. siplot stores bounds per data type but logic is similar. The gap is that egui |
| ☐ | M | L | TIME_SERIES / datetime axis tick mode (TickMode enum) | `items/axis.py:43-48, 296-308, _utils/dtime_ticklayout.py (entire file)` | silx supports datetime.datetime tick labels and timezone-aware formatting on axes via setTickMode(TickMode.TIME_SERIES). siplot only supports numeric ticks. Missing: DtUnit enum, bestUnit(), calcTi |
| ☐ | L | M | Data margin ratios for resetZoom (setDataMargins / getDataMargins) | `PlotWidget.py:3251-3270, resetZoom:3352-3397 margin expansion logic` | silx setDataMargins(xMin, xMax, yMin, yMax) stores per-side margin ratios, applied by resetZoom to expand limits around visible data. siplot has axes_margins (space reserved in gutters) but not dat |
| ✅ N/A | L | M | Dirty flag and replot lifecycle (_setDirtyPlot, replot, autoreplot) | `PlotWidget.py:719-730, 3309-3311, 3279-3289` | Resolved W15 — not a gap. silx's `_dirty`/`replot`/`autoreplot` are a retained-backend deferral mechanism; siplot is immediate-mode (egui owns the frame loop) with persistent GPU buffers, so redraw is always-on and uploads are event-driven. `DirtyState` exists as a faithful port (set, not consumed). Not a correctness issue. |
| ☐ | L | M | Axis label fallback to active curve label | `items/axis.py:187-218 (setLabel, _setCurrentLabel; PlotWidget handles active curve label swapping)` | silx axis.setLabel() sets the default, but PlotWidget swaps in the active curve's label if one is set. siplot stores a single label string per axis with no fallback logic. |
| ☐ | L | S | Axis visibility toggle (setAxesDisplayed / isAxesDisplayed) | `PlotWidget.py:2838-2855` | silx allows hiding axes/frame/ticks entirely via setAxesDisplayed(false), which zeroes axes margins. siplot chrome always draws frame and ticks. Missing: visibility flag in Plot, conditional axis/f |
| ✅ N/A | L | S | Overlay-only replot optimization | `PlotWidget.py:719-730 (dirty='overlay')` | Resolved W15 — not a gap. siplot is immediate-mode with persistent GPU buffers (data uploaded on add/update, not per frame); there is no per-frame full upload for dirty='overlay' to skip. Parity achieved differently. |
| ◐ | L | S | Separate foreground (axes/frame) and grid colors | `PlotWidget.py:setForegroundColor, setGridColor (separate calls); backends/_PlotFrameCore.py splits axis vs grid stroke` | siplot lumps frame/axes into 'foreground' color. silx allows independent frame stroke and grid color. Minor: egui chrome applies foreground to axis strokes and labels together; grid color is separa |

## Interaction, events, panzoom, limits history  — 19✅ 5◐ 23☐

siplot implements core panning and zooming, including wheel zoom anchored at cursor and box zoom, plus interaction mode switching (select/pan/zoom). Double-click reset and crosshair cursor are implemented. However, the signal/event architecture differs fundamentally: silx uses an extensive Qt signal system with fine-grained event callbacks (mouseClicked, mouseDoubleClicked, hover, markerMoving, drawingProgress, etc.) while siplot uses simple enums (PlotEvent) with discrete state changes only. Limits history (undo/redo stack) is completely missing. Drawing modes (polygon, rectangle, ellipse, line, freehand, etc.) are absent. Hover event signals are not emitted. Arrow-key panning is not implemented. Custom cursor shapes for ROI handle dragging are not set. Marker interaction callbacks are missing.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ☐ | H | M | Pan respects logarithmic axes with proper math | `PlotInteraction.py:233-290` | interaction.rs:pan() is linear only; does not handle log scale like silx's math.log10()/pow(10) conversions. Only works correctly for linear scales. |
| ☐ | H | M | Wheel zoom respects logarithmic axes | `panzoom.py:80-119` | zoom_about() in interaction.rs uses simple linear scaling; does not convert to/from log space like silx's scale1DRange() for log axes. |
| ☐ | M | L | Limits history (undo/redo stack for zoom history) | `LimitsHistory.py:34-82` | No LimitsHistory equivalent; no push/pop undo stack. Plot has home_limits (single snapshot) but no multi-level history like silx's _history list. |
| ☐ | M | L | Draw mode (polygon, rectangle, ellipse, line, freehand, etc.) | `PlotInteraction.py:1648-1683` | No draw/select-draw modes. silx supports 8+ drawing shapes (polygon, rectangle, ellipse, line, vline, hline, polylines, pencil) that emit drawingProgress/drawingFinished signals. siplot has only se |
| ☐ | M | L | Polygon drawing interaction (select mode) | `PlotInteraction.py:485-621` | No polygon draw mode. silx SelectPolygon allows point-by-point vertex entry with snap-to-first-point closure detection. |
| ☐ | M | M | Pan via arrow keys (left/right/up/down directional input) | `PlotInteraction.py (search PlotWidget for arrow handling)` | No keyboard shortcut support for arrow keys to pan in any direction; siplot only captures mouse input. |
| ☐ | M | M | Zoom enforces float32 safety limits during zoom | `PlotInteraction.py:241-250, panzoom.py:44-47` | No FLOAT32_SAFE_MIN/MAX checks; zoom_about() does not validate or clamp to safe range. Could silently overflow on extreme zooms. |
| ☐ | M | M | Signal: mouseClicked with button, position (data and pixel coords) | `PlotInteraction.py:168-199, PlotEvents.py:58-70` | No click event signal. siplot response object supports clicked() but does not emit structured PlotEvent callbacks with button/dataPos/pixelPos like silx's prepareMouseSignal. |
| ◐ | M | M | Selection area visualization (semi-transparent overlay during zoom/draw) | `PlotInteraction.py:98-141 (setSelectionArea), 421-430 (draw area), 526-528, etc.` | siplot draws box-zoom selection rectangle only. silx also draws selection areas for all draw modes (polygon, rectangle, ellipse, line, freehand) with hatch or solid fill and contrasting border colo |
| ☐ | M | M | Rectangle drawing interaction (select mode) | `PlotInteraction.py:767-807` | No rectangle draw mode. silx SelectRectangle captures 2-point drag and emits rectangle bounds. |
| ☐ | M | M | Floating-point overflow protection during pan/zoom | `PlotInteraction.py:233-289, panzoom.py:114-118` | No overflow checks. silx clips to FLOAT32_SAFE_MIN/MAX after pan/zoom; siplot merely checks is_valid() (non-degenerate), but does not protect against silent overflow. |
| ☐ | L | L | Signal: drawingProgress (real-time feedback during shape drawing) | `PlotInteraction.py:529-532, 789-792, 903-906, etc., PlotEvents.py:34-55` | No drawing modes or signals. silx emits drawingProgress for each vertex added during polygon/rectangle/ellipse/line/polyline drawing. siplot has no draw mode. |
| ☐ | L | L | Signal: drawingFinished (on shape completion) | `PlotInteraction.py:545-548, 800-803, 911-914, etc., PlotEvents.py:34-55` | No drawing mode completion signal. silx emits drawingFinished with final points and parameters. |
| ✅ N/A | L | L | Interaction state machine (ClickOrDrag, StateMachine) | `Interaction.py:87-198, PlotInteraction.py:153-209` | Resolved — not a gap. Implementation structure, not a user feature; siplot achieves the same interaction behavior with imperative `apply_interaction` dispatch + `DrawState`. Functional parity achieved differently. |
| ☐ | L | M | Signal: hover (mouseMoved) with item label, type, draggable/selectable flags | `PlotInteraction.py:1135-1154, PlotEvents.py:73-85` | No hover event signals. silx emits hover with item metadata (label, type, whether draggable/selectable, data and pixel position). siplot only tracks crosshair rendering. |
| ☐ | L | M | Signal: markerClicked with marker details and position | `PlotInteraction.py:1223-1241, PlotEvents.py:88-139` | No marker click event. silx emits structured markerClicked signal with marker name, position data, button, and draggable/selectable flags. siplot has no equivalent. |
| ✅ (W14) | L | M | Signal: markerMoving/markerMoved (marker drag feedback) | `PlotInteraction.py:1276-1299, 1350` | Now split: `MarkerMoved{handle}` each frame during the drag (silx `markerMoving`) plus a distinct on-release `MarkerDragFinished{handle}` (silx `markerMoved`), bracketed by `MarkerDragStarted{handle}`. Each event carries the handle; position is read via `PlotWidget::marker_position`. |
| ✅ (W14) | L | M | Signal: curveClicked with curve indices and position | `PlotInteraction.py:1243-1261, PlotEvents.py:159-173` | `PlotEvent::CurveClicked{handle,index,x,y,button}` carries the nearest picked vertex index + data position + button. (silx's full xdata/ydata arrays are reachable via the handle, not inlined.) |
| ✅ (W14) | L | M | Signal: imageClicked with pixel (col, row) index and position | `PlotInteraction.py:1263-1272, PlotEvents.py:142-156` | `PlotEvent::ImageClicked{handle,col,row,button}` emitted from the `pick_topmost` owner path over the existing `image_index`/`pick_image_pixel` picker. |
| ✅ | L | M | Cursor shape change (resize cursors for draggable markers/handles) | `PlotInteraction.py:1165-1184` | ROI-edge resize cursors were already set; Wave 11 adds the draggable-marker size cursor (marker_cursor: VLine→SizeHor, HLine→SizeVer, free Point→SizeAll, constrained Point→the free axis), shown on hover and during drag, taking precedence over the ROI-edge cursor. Matches silx CURSOR_SIZE_HOR/VER/ALL. |
| ✅ | L | M | Selection area color and fill mode (hatch, solid, none) | `PlotInteraction.py:98-141` | Closed W15: box-zoom + draw preview share one selection-overlay renderer honoring FillMode Hatch/Solid/None; box-zoom uses silx Zoom `fill="none"` dashed outline. |
| ☐ | L | M | Ellipse drawing interaction (select mode) | `PlotInteraction.py:681-765` | No ellipse draw mode. silx SelectEllipse converts 2-point drag into ellipse parameters (center, semi-axes) with eccentricity preservation. |
| ✅ | L | M | Freehand/polyline drawing (select mode) | `PlotInteraction.py:955-1110` | Done: `DrawMode::FreeHand` accumulates vertices (DrawState); the mask pencil footprint preview circle is now drawn at the cursor (W15, `ImageView::draw_brush_preview` + `pencil_preview_circle`). |
| ✅ | L | M | Axis constraints (minXRange, maxXRange, minYRange, maxYRange) | `panzoom.py:222-366` | Closed W15: `AxisConstraints::apply` mirrors silx `ViewConstraints` allow_scaling=True normalization — max-range capped to the position window, and a view wider than the window snaps to it (adaptive expansion) |
| ☐ | L | S | Signal: mouseDoubleClicked with position | `PlotInteraction.py:168-177, PlotEvents.py:58-70` | No double-click event signal emission. egui detects double_clicked() but no callback is emitted to application. |
| ◐ | L | S | Signal: limitsChanged with x/y/y2 range tuples | `PlotEvents.py:176-184` | siplot emits simple PlotEvent::LimitsChanged flag, but silx signal includes actual xdata/ydata/y2data range tuples for subscribers to know the new limits without querying the plot. |
| ☐ | L | S | Line drawing interaction (select mode) | `PlotInteraction.py:809-840` | No line draw mode. silx SelectLine captures start/end points and emits line coordinates. |
| ☐ | L | S | Horizontal line drawing (select mode) | `PlotInteraction.py:885-918` | No hline draw mode. silx SelectHLine draws horizontal line across data area. |
| ☐ | L | S | Vertical line drawing (select mode) | `PlotInteraction.py:920-953` | No vline draw mode. silx SelectVLine draws vertical line across data area. |

## Items: Curve, Histogram, Scatter  — 23✅ 3◐ 9☐

siplot covers basic curve rendering (solid/dashed lines, single symbols, error bars, fill + baseline) and per-vertex colors, with correct line-style scaling and custom dash patterns implemented. Histograms are rendered as filled step curves but lack alignment/orientation tracking (center/left/right modes). Scatter has only marker-only point cloud visualization (no Delaunay solid/grid/binned modes), no per-point alpha, and supports only 5 symbol types vs silx's 19. Symbol support is severely limited (Circle, Square, Cross, Plus, Triangle only; missing Diamond, Point, Pixel, and all caret/tick variants). Picking is implemented for nearest-point queries but histogram picking for filled regions is unimplemented. Per-point symbol sizes are not supported (curves/scatter only single size). Active-curve highlight styling is now applied at render (Wave 10): the active curve gets a distinct style (default line width 2) via silx's `getCurrentStyle` per-field merge as a render-time overlay.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ◐ | H | M | Curve symbols (circle, square, cross, plus, triangle) | `core.py:722-820 (SymbolMixIn), supported list includes o,d,s,+,x,.,etc.` | Only 5 symbols implemented (Circle, Square, Cross, Plus, Triangle). Missing: Diamond (d), Point (.), Pixel (,), VerticalLine (\|), HorizontalLine (_), all 4 Tick variants, all 4 Caret variants, Heart. |
| ☐ | M | L | Scatter visualization mode: Solid (Delaunay triangulation) | `scatter.py:283-296, core.py:1271-1275 (Visualization.SOLID)` | silx computes Delaunay triangulation in background thread and renders as filled triangles. siplot has no Delaunay support or triangle visualization. |
| ☐ | M | L | Scatter visualization mode: IrregularGrid (image from unstructured points) | `scatter.py:283-296, core.py:1286-1293 (Visualization.IRREGULAR_GRID)` | silx uses Delaunay triangulation + linear interpolation to create image from scattered points. siplot has no support. |
| ✅ | M | M | Curve highlight/selection state with different style | `curve.py:196,280-311 (getCurrentStyle), core.py:1875-1905 (HighlightedMixIn)` | Wave 10: the active curve renders with a distinct highlight style. `current_curve_style` is silx `getCurrentStyle`'s per-field merge (highlight field if Some, else the curve's own); default active style = line width 2 (silx `DEFAULT_PLOT_ACTIVE_CURVE_LINEWIDTH`), color unchanged. Applied as a render-time overlay (retained base stays the single source of truth), gated to `PlotItemKind::Curve`. **GPU-UNVERIFIED:** the on-screen thicker line needs a GPU `RenderState` no crate test constructs; only the pure merge is unit-tested. |
| ✅ | M | M | Histogram bin alignment (left, center, right) | `histogram.py:53-85 (_computeEdges), setData(align='center'/'left'/'right'), getAlignment` | W15: `HistogramAlign` + pure `histogram_edges(positions, align)` mirror silx `_computeEdges`; public `add_histogram_aligned`/`_with_legend` accept N positions + N counts + alignment. Headlessly tested per alignment. |
| ☐ | M | M | Scatter visualization mode: RegularGrid (image-like grid rendering) | `scatter.py:283-296, core.py:1277-1282 (Visualization.REGULAR_GRID)` | silx auto-detects regular grid from point coordinates and renders as image. siplot has no grid detection or grid rendering mode. |
| ☐ | M | M | Scatter per-point alpha transparency | `scatter.py:1009,1024,1051-1060 (setData with alpha parameter, __alpha field)` | silx Scatter supports per-point alpha values. siplot ScatterView only has global alpha from CurveSpec (scalar). |
| ◐ | M | M | Scatter picking with mode-specific logic | `scatter.py:804-860 (pick with special handling per Visualization mode)` | Points mode picking works (nearest_point). No picking support for Solid/RegularGrid/IrregularGrid/BinnedStatistic modes because those visualizations are not implemented. silx has triangulation-based p |
| ✅ | M | M | Plot item selection/highlight state tracking | `core.py:1875-1905 (HighlightedMixIn with setHighlighted/isHighlighted)` | Wave 10: the active-item selection is now propagated to the renderer for curves. `set_active_item` reverts the previous active curve to its base style and applies the highlight overlay to the new one (silx `setHighlighted(False)`/`(True)` in `_setActiveItem`), through a single owner `sync_curve_highlight`; `set_active_curve_style`/`set_active_curve_handling` mirror silx. Image/scatter selection still has no highlight style (silx highlights only `kind=='curve'`). |
| ☐ | L | L | Histogram picking (filled region sensitivity) | `histogram.py:244-290 (pick, __pickFilledHistogram with bounds check)` | No histogram-specific picking. silx histogram.pick() has special logic for filled histograms: bounds check in data space, searchsorted to find bin index, then y-range check (baseline <= yData <= value |
| ☐ | L | L | Scatter visualization mode: BinnedStatistic (2D histogram with statistics) | `scatter.py:283-296, core.py:1295-1301 (Visualization.BINNED_STATISTIC)` | silx bins scattered points into 2D grid and computes per-bin statistics (mean/count/sum). Rendered as colormapped image. siplot has no binning support. |
| ☐ | L | S | Scatter visualization parameter: grid_major_order (row/column) | `core.py:1303-1308, 1346 (VisualizationParameter.GRID_MAJOR_ORDER)` | Parameter exists in silx for regular/irregular grid modes. siplot has no grid visualization, so parameter not applicable. |
| ☐ | L | S | Scatter visualization parameter: binned_statistic_function (mean/count/sum) | `core.py:1325-1329, 1347 (VisualizationParameter.BINNED_STATISTIC_FUNCTION)` | Parameter for binned statistic mode. siplot has no binning support. |
| ☐ | L | S | Scatter visualization parameter: binned_statistic_shape (grid dimensions) | `core.py:1325,1333 (VisualizationParameter.BINNED_STATISTIC_SHAPE)` | Parameter for binned statistic mode. siplot has no binning support. |

## Items: Image, Marker, Shape, Complex  — 30✅ 4◐ 7☐

siplot implements core image (scalar colormapped + RGBA direct), marker (point/vline/hline), and shape (polygon/rectangle/polyline/hline/vline) drawing. Colormaps support 8 built-in names with linear/log/sqrt/gamma normalization and a 256-entry LUT. Images support origin/scale placement, per-pixel alpha, and GPU tiling for large datasets. Markers support point symbols, text labels with background color, and line styling. Shapes support fill (convex only), outline styling with dashed gaps. Missing: image masking, image interpolation modes (locked to nearest), complex image modes (7 variants from silx), image aggregation/downsampling modes (max/min/mean), image stack/3D support, marker draggability/constraints, marker text anchor positioning, shape overlay flag (all shapes draw as overlay), and Line item (infinite y=slope*x+intercept).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ☐ | M | M | Image per-pixel alpha map | `items/image.py:462-500 (ImageData.getAlphaData/setAlphaData, alternative image with alpha)` | silx supports setData(data, alternative=None, alpha=alpha_array). ImageData struct has only global alpha: f32, no per-pixel alpha array or alternative RGBA image overlay. Implementation requires addin |
| ☐ | M | M | Image masking (per-pixel validity mask) | `items/image.py:209-251 (getMaskData/setMaskData), 273-284 (getValueData applies mask as NaN)` | silx's ImageBase and ImageData support setMaskData(mask) to mark invalid pixels as NaN in getValueData. siplot ImagePixels::Scalar has no mask field. Requires extending ImageData struct and GPU pip |
| ✅ | M | M | Marker draggability (isDraggable/drag callback) | `items/marker.py:52-67 (MarkerBase with DraggableMixIn), 177-206 (drag method, setPosition)` | Wave 11: a draggable marker (is_draggable) is grabbed on primary drag (topmost via marker_at), follows the cursor through Marker::drag (constraint-aware), persists to the backend item, and reports via PlotResponse.marker_moved / PlotEvent::MarkerMoved. Public set_marker_position / marker_position (silx setPosition/getPosition). No separate sigDragStarted/sigDragFinished split (single move signal). |
| ☐ | L | L | Image aggregation/downsampling modes (max/mean/min) | `items/image_aggregated.py:46-138 (ImageDataAggregated.Aggregation enum with NONE/MAX/MEAN/MIN, _getLevelOfDetails LOD reduction)` | siplot has no ImageDataAggregated equivalent. No support for setAggregationMode or dynamic LOD reduction. Large images tile at GPU limits but no aggregation strategy for display downsampling (all t |
| ☐ | L | L | Complex image modes (7 variants: absolute/phase/real/imaginary/amplitude_phase/log10_amplitude_phase/square_amplitude) | `items/complex.py:105-378 (ImageComplexData class, ComplexMode enum with 7 modes, mode-specific colormaps and conversions)` | No ImageComplexData or complex image support. silx's complex modes convert complex input to float/RGBA for display with mode-dependent colormaps (e.g., phase as HSV, amplitude_phase as phase color + l |
| ◐ | L | L | Shape fill concavity limitation (convex-only rasterization) | `items/shape.py (silx defers to backend, matplotlib/pygfx handle concave, but silx does not guarantee correctness)` | egui's convex_polygon rasterizer will render concave polygon fill incorrectly (as convex hull). silx does not formally restrict to convex, so a concave polygon may display differently. No warning or f |
| ◐ | L | M | Image interpolation mode (nearest vs linear sampling) | `backends/BackendMatplotlib.py:805 (interpolation='nearest' hardcoded; silx backends via matplotlib accept 'nearest'/'bilinear'/etc.)` | Data sampler is hardcoded to Nearest (line 338-340), no option to use Linear. Colormap LUT sampler uses Linear (intentional for smooth color transitions). Need to expose interpolation mode control for |
| ☐ | L | M | Image stack (3D array, show one frame at a time) | `items/image.py:593-669 (ImageStack class, setStackData/getStackData/setStackPosition)` | No ImageStack equivalent. silx allows setStackData(stack_3d, position) to display one 2D slice at a time, with lazy updates. siplot has no stack/frame concept; only single 2D ImageData. Would requi |
| ◐ | L | M | Marker constraint function (horizontal/vertical/custom drag filter) | `items/marker.py:208-235 (getConstraint/_setConstraint with _horizontalConstraint/_verticalConstraint), 273-292 (Marker subclass overrides for constraint strings)` | Wave 11 wires the horizontal/vertical presets (MarkerConstraint::Horizontal/Vertical) into the drag and the size cursor (Horizontal→SizeVer, Vertical→SizeHor). The arbitrary custom-callback form (setConstraint(fn)) is not supported — presets only. |
| ☐ | L | M | Shape overlay flag (data layer vs separate overlay layer) | `items/shape.py:54-73 (_OverlayItem with isOverlay/setOverlay)` | silx's Shape has setOverlay(bool) to choose rendering layer (data vs overlay). siplot Shape has no overlay field; all shapes draw in one overlay pass (doc/design.md §8 notes this). To match silx be |
| ☐ | L | M | Line item (infinite y = slope·x + intercept) | `items/shape.py:289-393 (Line class, distinct from Shape, infinite line with slope/intercept, computed vertices via visible bounds tracking)` | silx has a separate Line item for infinite lines (y = slope*x + intercept or vertical x = intercept). siplot has no Line equivalent. Would require a separate Line struct in core/ and rendering code |
| ◐ | L | S | Marker text anchor/alignment (positioning relative to marker point) | `backends/BackendMatplotlib or similar: text drawn at marker point with implicit offset/anchor; silx does not expose anchor API directly in Marker but implicit in rendering` | silx's marker text positioning is fixed by the backend (typically offset from the point). siplot draws text at a fixed offset (likely to the right/below). No exposed anchor enum (e.g., TopLeft/Cent |

## Backend Render (WgpuBackend vs BackendPygfx)  — 30✅ 3◐ 7☐

The WgpuBackend in siplot implements the core Backend trait with substantial coverage of BackendPygfx functionality. Additive item methods (addCurve, addImage, addTriangles, addShape, addMarker) and state management (limits, axes, colors, margins) are present. Core rendering (transforms, data-to-pixel, picking) is implemented. Key gaps: symbol coverage is narrowed (5 vs 8+ in silx), line-join/cap handling is deferred, dpi parameter in save_graph is unimplemented, and some fine-grained line-style/gap-color options lack round-trip validation. No SVG/TIFF export (PNG only), and async stats/histogram compute (GPU reduction) is handled upstream in silx, not mirrored in siplot.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ◐ | H | L | addMarker: point/hline/vline marker with symbol, text, line constraints | `BackendBase.py:211-267, BackendPygfx.py:1397-1465` | Constraint callback (drag filter) is Qt-only; text rendering via egui, not screen-space overlay like silx. No support for QFont/bgcolor (color-only, no text background rect). |
| ◐ | H | M | Symbol support: circle, square, cross, plus, diamond, etc. | `BackendBase.py:116-126, BackendPygfx.py:85-98 (_SYMBOL_MAP)` | Silx supports 'o'(circle), '.'(point→circle-small), ','(pixel→square), '+'(plus), 'x'(cross), 'd'(diamond), 's'(square); siplot only has Circle, Square, Cross, Plus, Triangle. Missing: point/pixel  |
| ◐ | M | L | saveGraph: PNG export at specified DPI and size | `BackendBase.py:372-382, BackendPygfx.py:2588-2610` | PNG only; silx supports PNG, PPM, TIFF, SVG. DPI parameter is accepted but ignored (noted in silx too). No vector export. |
| ☐ | L | L | Line joins: round/bevel/miter for connected segments | `BackendPygfx.py uses pygfx LineMaterial (no explicit join config visible)` | Deferred design-doc item (§7·§13 B1). Butt caps + gaps visible at sharp turns; high-res curves hide this. |
| ☐ | L | L | Line caps: butt, round, square at line endpoints | `BackendPygfx.py uses pygfx LineMaterial` | Deferred (§7·§13 B1). Only butt caps implemented; round/square caps listed as future work. |
| ☐ | L | L | Grid on/off: toggle major/both grid display | `BackendBase.py:543-549, BackendPygfx.py:2754-2756` | setGraphGrid method not in Backend trait or WgpuBackend. Grid is drawn by chrome layer (not GPU backend); no on/off toggle exposed. |
| ☐ | L | M | Crosshair cursor (setGraphCursor): show/hide crosshair, set color/width/style | `BackendBase.py:289-310, BackendPygfx.py:1948-1990 (_updateCrosshair)` | Not part of Backend trait or WgpuBackend (interactive UI feature, deferred to widget layer). |
| ☐ | L | M | Time-series axis: display datetime objects on X axis | `BackendBase.py:468-489, BackendPygfx.py:2704-2714` | Backend trait has no mention; timestamp rendering is chrome-layer responsibility in both silx and siplot. |
| ☐ | L | M | GPU stats/histogram: async GPU compute for min/max/histogram (streaming data) | `BackendPygfx.py:1579-1622 (_WgpuComputeHelper, _AsyncCompute, _computeGpuDataStats, _computeGpuHistogram)` | Async GPU reduction (minmax, histogram via atomic+readback) implemented in silx for colormap autoscale; not mirrored in siplot backend (deferred to widget/colormap layer). |
| ☐ | L | S | postRedisplay / request_draw: request a repaint on next frame | `BackendBase.py:363-365 (postRedisplay calls replot)` | egui redraws every frame by default (widget layer responsibility); no explicit repaint scheduling in backend. |

## Colormap, ColorBar, and AlphaSlider  — 4✅ 7◐ 8☐

siplot has a minimal but functional implementation of colormaps with 8 catalog entries (Viridis, Inferno, Magma, Plasma, Cividis, Turbo, Greys, Spectral), supporting 4 of 5 silx normalizations (Linear, Log, Sqrt, Gamma; missing Arcsinh), and a basic colorbar drawer with log/linear ticks. The ColormapDialog supports name/normalization/gamma/autoscale/range controls. Missing: the full silx colormap catalog (gray, reversed gray, temperature, red, green, blue, jet, hsv + matplotlib colormaps), NaN color configuration, autoscale modes (minmax/stddev3/percentile), percentile controls, AlphaSlider widget, colorbar min/max labels, colorbar legend, ColorBarWidget standalone, and various Colormap utility methods.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ◐ | H | L | Colormap catalog names | `/Users/stevek/codes/silx/src/silx/gui/colors.py:444-455, /Users/stevek/codes/silx/src/silx/math/colormap.py:53-65` | siplot provides 8 colormaps (Viridis, Inferno, Magma, Plasma, Cividis, Turbo, Greys, Spectral); silx provides these plus gray, reversed gray, temperature, red, green, blue, jet, hsv, and all matplo |
| ◐ | H | M | Autoscale mode selection | `/Users/stevek/codes/silx/src/silx/gui/colors.py:318-331, 563-586` | siplot has a binary autoscale flag (line 120, checkbox); silx has explicit modes: MINMAX, STDDEV3, PERCENTILE. ColormapDialog applies naive min/max from first image (lines 159-177), ignoring stddev |
| ☐ | H | M | Autoscale percentile bounds | `/Users/stevek/codes/silx/src/silx/gui/colors.py:588-599, /Users/stevek/codes/silx/src/silx/math/colormap.py:355-368` | silx allows setAutoscalePercentiles(tuple[float, float]) with defaults (1.0, 99.0) and supports percentile mode autoscaling. siplot has no percentile autoscale support or configuration. Required to |
| ◐ | M | L | ColormapDialog interface | `/Users/stevek/codes/silx/src/silx/gui/dialog/ColormapDialog.py:24-300` | siplot ColormapDialog provides name, normalization, vmin/vmax, autoscale, gamma. Missing from siplot: histogram display (setHistogram, lines 49-55 docs, used to show data distribution), autoscal |
| ☐ | M | M | NaN color configuration | `/Users/stevek/codes/silx/src/silx/gui/colors.py:506-518, 337` | silx Colormap has getNaNColor/setNaNColor (default: fully transparent white #FFFFFF00). siplot has no NaN color field, property, or API. Required: add nan_color: [u8; 4] field to Colormap struct, e |
| ◐ | M | M | Colorbar drawing with ticks | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:296-920` | siplot draw_colorbar draws 64 vertical strips and decade/nice ticks with labels positioned right of the bar (lines 862-920). silx ColorScaleBar is much more sophisticated: 256-entry gradient widget |
| ☐ | M | M | Colorbar min/max labels | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:364-452` | silx ColorScaleBar displays formatted min/max labels at top/bottom with tooltips showing full precision. Dynamically resizes label width based on number formatting. siplot has no labels; ticks and  |
| ◐ | M | S | Normalization types | `/Users/stevek/codes/silx/src/silx/gui/colors.py:292-316` | siplot supports Linear, Log, Sqrt, Gamma (4 of 5). Missing: Arcsinh (inverse hyperbolic sine) normalization, which is explicitly supported in silx (lines 304-305, colors.py ARCSINH = 'arcsinh'). |
| ☐ | L | L | ColorbarWidget standalone | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:44-263` | silx ColorBarWidget is a standalone QWidget that can be placed anywhere, syncs with a Plot's active image/scatter colormap via signals. siplot colorbar is internal to chrome layout (widget/chrome.r |
| ☐ | L | M | Colorbar legend label | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:44-200, 266-294` | silx ColorBarWidget includes a _VerticalLegend widget that displays a vertical text label beside the colorbar (rotated 270 degrees). siplot draw_colorbar receives only the colormap and rect, no leg |
| ◐ | L | M | Colorbar orientation (vertical/horizontal) | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py (vertical only in examples but widget is orientation-agnostic)` | siplot draw_colorbar is hardcoded vertical (vmin at bottom, vmax at top, ticks to the right). silx ColorScaleBar and ColorBarWidget support both vertical (Qt::Vertical) and horizontal (Qt::Horizont |
| ☐ | L | M | AlphaSlider widget | `/Users/stevek/codes/silx/src/silx/gui/plot/AlphaSlider.py:86-250` | silx provides BaseAlphaSlider (abstract), ActiveImageAlphaSlider (tracks active image alpha), and NamedItemAlphaSlider (controls specific item by legend). QSlider range 0-255 emits float alpha [0.0, 1 |
| ☐ | L | M | Custom colormap registration | `/Users/stevek/codes/silx/src/silx/math/colormap.py:158-176, /Users/stevek/codes/silx/src/silx/gui/colors.py:343-386` | silx supports setColormapLUT to load custom Nx3 or Nx4 LUT arrays at runtime, and register_colormap global function. siplot Colormap::new() only accepts ColormapName enum, not custom arrays. Requir |
| ◐ | L | S | Colormap copy/comparison/serialization | `/Users/stevek/codes/silx/src/silx/gui/colors.py:399-423, 960-1050` | silx Colormap has copy(), setFromColormap(), __eq__, restoreState(), saveState() for round-trip serialization and state management. siplot Colormap derives Clone/PartialEq but lacks setFromColormap |
| ☐ | L | S | Colormap editability flag | `/Users/stevek/codes/silx/src/silx/gui/colors.py:351, 659-674` | silx Colormap._editable flag controls whether setName, setNormalization, etc. are allowed; raises NotEditableError if frozen. siplot Colormap has no editability concept. Required: add editable bool |

## ROI system (creation, editing, manager, statistics)  — 10✅ 11◐ 16☐

siplot implements a ROI core with all 11 silx shape kinds (Rect, HRange, VRange, Point, Line, Polygon, Cross, Circle, Ellipse [axis-aligned], Arc, Band), supporting on-plot interactive creation for every kind (Wave 12: arm a draw shape, draw it, the ROI is appended), edge dragging via handle-detection, and whole-ROI translate — all in pure data space. The manager provides basic add/remove/list UI and event reporting (RoiChanged + RoiCreated). However, it still lacks much of silx's visual/behavioral richness: no per-ROI color, naming/labels, selection highlighting, handle-symbol customization, style properties (line width, style, gap color), specialized edit handles (Ellipse orientation, Circle/Arc radius handles, Band rotation, Arc start/end-angle sub-modes), statistics calculation (mean, sum, integral, peaks), curves ROI integration with per-ROI curve stats tables, or ROI persistence/load features.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | L | Interactive ROI creation mode (draw mode vs select mode) | `tools/roi.py:833-885 (start/stop/isStarted/isDrawing)` | Wave 12: `PlotInteractionMode::RoiCreate(RoiDrawKind)` arms a draw shape (silx `start(roiClass)`); the existing `DrawState` machine drives the gesture, `roi_from_draw` maps the finished `DrawParams`→`Roi` (silx `setFirstShapePoints`), the ROI is appended, the mode re-arms continuously (silx default), and a live preview overlay is surfaced during the gesture. Public `set_roi_create_mode(kind)` toggles draw vs select. Still missing the toolbar `CreateRoiModeAction`/mode-selection widget. On-screen creation is GPU/PlotWidget-UNVERIFIED; the `DrawParams`→`Roi` mapping and the `apply_interaction` wiring are headlessly unit-tested. |
| ◐ | H | L | Manager ROI list as table widget (ROITable/CurvesROIWidget) | `CurvesROIWidget.py:62-400, ROITable:452-860` | egui shows a scrollable list with remove button per ROI and add buttons (Rect/HRange/VRange/Point/Line only). silx ROITable is a rich table with per-ROI stats columns (min/max/sum/mean/etc.), editable |
| ◐ | H | L | ROI statistics calculation (mean, sum, min, max, integral, peaks) | `CurvesROIWidget.py:355-430, ROIStatsWidget.py (full file)` | `roi_stats` computes count/min/max/mean/sum/integral per ROI (image + curve); W15 `curve_roi_counts` adds curve raw/net counts + raw/net area. Still missing peak detection. |
| ✅ | H | L | CurvesROIWidget integration (ROI stats per curve item) | `CurvesROIWidget.py (entire file)` | W15: `CurvesRoiWidget` shows a per-ROI raw/net counts + raw/net area table over the active curve via `PlotWidget::feed_curves_roi_stats`/`show_curves_roi_widget`, reducing each x-span ROI with the pure `curve_roi_counts` (silx `computeRawAndNetCounts`/`computeRawAndNetArea`). |
| ✅ | H | L | ROIStatsWidget (image/curve ROI stats display) | `ROIStatsWidget.py (entire file)` | W15: `RoiStatsWidget` shows a per-ROI stats table (`ROI \| N \| min \| max \| mean \| sum \| integral`) over the active item via `PlotWidget::feed_roi_stats`/`show_roi_stats_widget`, reducing each ROI with `image_roi_stats`/`curve_roi_stats`. |
| ☐ | H | M | ROI per-instance color (independent from manager default) | `items/_roi_base.py:389-405, tools/roi.py:713-742` | silx ROI has setColor()/getColor(); egui Roi struct stores no color. Manager has manager-level color only (ui buttons). |
| ☐ | H | M | ROI label/text display on canvas | `items/_roi_base.py:492-511` | silx draws ROI name as text overlay; egui draw_rois (chrome.rs:492-540) draws no labels; needs text layer + positioning. |
| ☐ | H | M | ROI selection/highlighting (visual feedback) | `tools/roi.py:528-590 (setCurrentRoi/getCurrentRoi/sigCurrentRoiChanged)` | silx highlights selected ROI via setHighlighted(); egui has no selection state in Roi struct or rendering; no visual distinction. |
| ☐ | H | M | ROI contains() point-in-region test | `items/roi.py:61-1599 (all ROI classes have contains method)` | silx ROI.contains(position) checks if point(s) are inside; egui Roi has no contains() method; needed for picking/stats. |
| ☐ | H | M | Manager current ROI tracking (setCurrentRoi/getCurrentRoi) | `tools/roi.py:528-590` | silx tracks selected ROI with signal emission; egui has no per-ROI selection state; no highlight/focus mechanism. |
| ☐ | H | S | ROI name/label (string identifier) | `items/_roi_base.py:77-92, CurvesROIWidget.py:594-640` | silx ROI has getName()/setName(); egui Roi enum has no name field; manager shows auto-generated descriptions only. |
| ◐ | M | L | EllipseROI kind (center, radii, orientation handles) | `items/roi.py:950-1177` | `Roi::Ellipse` (axis-aligned center+radii) exists and Wave 12 adds on-plot creation (`RoiDrawKind::Ellipse`, drawn via the rectangle/ellipse drag, radii from the bbox half-extents). Still missing silx's rotational geometry, axis-major/minor handles, and orientation state — this port's `Roi::Ellipse` does not model `EllipseROI._orientation`. |
| ◐ | M | M | CrossROI kind (point with cross marker) | `items/roi.py:133-185` | `Roi::Cross { center }` exists and Wave 12 adds on-plot creation (`RoiDrawKind::Cross`, single click via `DrawMode::Point`). Still missing marker-symbol control and the composite handle/label management silx attaches to the cross. |
| ◐ | M | M | CircleROI kind (center + radius handles) | `items/roi.py:800-950` | `Roi::Circle { center, radius }` exists and Wave 12 adds on-plot creation (`RoiDrawKind::Circle`, 2-point line drag → center+radius, matching silx's line-shape circle creation). Editing is whole-ROI translate only; still missing the dedicated center/radius edit handles. |
| ☐ | M | M | ROI handle symbols ('+', 's', 'o', custom glyphs) | `items/_roi_base.py:600-680 (addHandle/addLabelHandle/addTranslateHandle), PolygonROI:1244-1274` | silx ROI handles have symbol/style control ('+' for center, 's' for vertices, 'o' for close-polygon indicator); egui draws fixed 6px squares. |
| ☐ | M | M | ROI line style (solid, dash, dot, gap color) | `items/_roi_base.py:600-680, RectangleROI:534-552` | silx ROI inherits LineMixIn with setLineStyle/setLineGapColor; egui draw_rois (chrome.rs) uses fixed solid 1.0 stroke, no dashes/gaps. |
| ◐ | M | M | Manager signals (sigRoiAdded, sigRoiChanged, sigCurrentRoiChanged, sigInteractiveRoiCreated/Finalized) | `tools/roi.py:331-371` | egui emits `RoiChanged` when an edge moves and (Wave 12) `RoiCreated { index }` when an on-plot draw finishes (≈ silx `sigInteractiveRoiCreated`/added). Still missing `sigCurrentRoiChanged` (no selection state) and a distinct create-vs-finalize split (RoiCreated fires once on completion; no separate finalized signal). |
| ◐ | M | M | ROI creation phase UI (preview overlay, mode indicator, close-polygon handle) | `PolygonROI:1241-1256, tools/roi.py:493-510` | Wave 12 surfaces a live preview overlay during the draw gesture (`Interaction.roi_preview` → `draw_overlay`), so the shape-in-progress is shown. Still missing the mode indicator and the polygon 'close' handle silx draws. On-screen preview is GPU-UNVERIFIED. |
| ☐ | M | S | ROI line width customization | `items/_roi_base.py:600-680, RectangleROI:534-552` | silx setLineWidth()/getLineWidth(); egui hardcodes border stroke 1.0 and fill alpha 24; no width config. |
| ◐ | M | S | ROI fill color and fill enable/disable | `items/Shape.py (shape draw), RectangleROI:531-539` | egui draws fixed semi-transparent fill (24 alpha); silx allows setFill(True/False). No configurable fill enable. |
| ☐ | M | S | Manager ROI color default (setColor/getColor) | `tools/roi.py:782-797` | Manager stores default color; egui manager does not track or apply manager-level ROI color (buttons only). |
| ☐ | M | S | ROI context menu (edit, delete, mode selection) | `tools/roi.py:625-642` | silx right-click on ROI shows menu with remove/mode-select options; egui manager shows no context menu. |
| ◐ | L | L | ArcROI kind (circular arc with start/end angle) | `items/_arc_roi.py (full file)` | `Roi::Arc { center, inner/outer radius, start/end angle }` exists and Wave 12 adds on-plot creation (`RoiDrawKind::Arc`, 2-point line drag; `arc_from_two_points` is a faithful port of silx `_circleEquation`/`_createControlPointsFromFirstShape`). Still missing the interactive sub-mode selection (start-angle vs radius editing) and the dedicated angle/radius handles. |
| ◐ | L | L | BandROI kind (rotatable rectangular band) | `items/_band_roi.py (full file)` | `Roi::Band { begin, end, width }` exists and Wave 12 adds on-plot creation (`RoiDrawKind::Band`, 2-point line drag → begin/end with a default width). Still missing the rotation, collision, and edge-snapping handles silx attaches to the band. |
| ☐ | L | M | HorizontalLineROI kind (single y coordinate) | `items/roi.py:373-435` | HorizontalLineROI is a single-y line spanning x; silx shows as YMarker. siplot does not distinguish from general HRange for single-line case. |
| ☐ | L | M | VerticalLineROI kind (single x coordinate) | `items/roi.py:437-510` | VerticalLineROI is a single-x line spanning y; silx shows as XMarker. siplot does not distinguish from general VRange for single-line case. |
| ◐ | L | M | ROI interaction modes (select, edit, focus constraints) | `items/_roi_base.py:135-230 (InteractionModeMixIn, RoiInteractionMode enum)` | Wave 12 adds a select-vs-create mode toggle and, within editing, both edge-drag and whole-ROI translate (`RoiGrab::Edge`/`RoiGrab::Translate`). Still missing silx's per-shape interaction sub-modes (e.g. Arc start-angle vs radius editing) — those remain a single fixed edit behaviour per kind. |
| ☐ | L | M | ROI edge position constraints (vertical/horizontal snapping, ratio lock) | `items/_roi_base.py and specific ROI classes (e.g., Band with collision)` | silx ROIs can constrain edge movement (e.g., Band preventing overlap); egui move_edge() does basic min/max clamping only. |
| ☐ | L | M | ROI save/load from file (dictdump format) | `CurvesROIWidget.py:194-210, roi save/load methods` | silx can persist ROI list to disk; egui has no serialization support. |
| ☐ | L | S | ROI keyboard shortcuts / text input for named creation | `tools/roi.py (mode actions with text labels)` | silx actions have keyboard shortcuts and naming UI; egui buttons have no shortcuts. |

## Mask tools (image + scatter)  — 0✅ 8◐ 19☐

The siplot mask tools implementation is minimal and incomplete compared to silx. Currently only supports basic pencil/eraser drawing with circular brush on 2D images (boolean mask, no mask levels). Missing: multi-level masks (256 levels with exclusive/single modes), all drawing shapes (rectangle, polygon, ellipse), line drawing with variable width, threshold/range masking, undo/redo history, mask load/save (npy/edf/csv/tif/h5/msk formats), mask transparency/alpha control, mask color customization, invert/clear operations per level, NaN masking, pencil width slider, scatter mask variant entirely, and mask display with colormap overlay.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ☐ | H | L | Multi-level masks (1-255 levels) | `_BaseMaskToolsWidget.py:279, 583-585` | siplot stores mask as Vec<bool> (single-level). silx supports 256 levels (uint8) with exclusive/single modes for overlapping or single-level masks. spin box for level selection, color per level, tr |
| ☐ | H | L | Scatter mask variant (1D point masking) | `ScatterMaskToolsWidget.py (entire file, 643 lines)` | silx has ScatterMaskToolsWidget for masking scatter point data. Updates scatter points by indices when shapes are drawn. Supports same drawing modes but operates on point indices, not image pixels. Sa |
| ☐ | H | M | Drawing tool: Rectangle | `MaskToolsWidget.py:805-826, _BaseMaskToolsWidget.py:307-317, shapes.polygon_fill_mask` | silx supports rectangle drawing via plot interaction with origin, width, height conversion from plot to data coords. siplot only has pencil/eraser. |
| ☐ | H | M | Drawing tool: Polygon | `MaskToolsWidget.py:840-847, _BaseMaskToolsWidget.py:319-326, shapes.polygon_fill_mask` | silx uses shapes.polygon_fill_mask() and handles vertex input via plot interaction. siplot missing entirely. |
| ☐ | H | M | Drawing tool: Ellipse | `MaskToolsWidget.py:828-838, _BaseMaskToolsWidget.py:351-361, shapes.ellipse_fill` | silx supports ellipse with separate row/col radii. siplot missing. Would need circular/elliptical region fill algorithm. |
| ☐ | H | M | Undo/Redo history | `_BaseMaskToolsWidget.py:144-194 (resetHistory, commit, undo, redo), 609-629 (undo/redo actions)` | silx maintains history stack (default 10 deep) with commit() on each operation. siplot no history tracking at all. sigUndoable/sigRedoable signals drive UI button enable state. |
| ☐ | H | M | Threshold-based masking (below, between, above) | `_BaseMaskToolsWidget.py:265-294 (updateBelowThreshold, updateBetweenThresholds, updateAboveThreshold), 848-937 (threshold UI), 1204-1225 (apply handler)` | silx compares image data values against thresholds to auto-mask regions. UI has three mode buttons, min/max text fields, 'Set min-max from colormap' button, Apply button. siplot no threshold maskin |
| ☐ | H | M | Mask save to file (npy/edf/tif/h5/csv/msk) | `MaskToolsWidget.py:104-141 (save method: edf/tif/npy/h5/msk), 698-785 (_saveMask dialog and format handling)` | silx supports numpy (npy), EDF, TIFF, HDF5 (with dataset selection dialog), Fit2D mask (msk). CSV for scatter. siplot has no file I/O at all. |
| ☐ | H | M | Mask load from file (npy/edf/tif/h5/csv/msk) | `MaskToolsWidget.py:589-629 (load method), 630-677 (_loadMask dialog)` | silx dialog with filter for each format, auto-detects by extension, HDF5 has dataset selection. Warns if mask resized/cropped to fit image. siplot no load at all. |
| ☐ | H | M | Scatter mask disk/circle drawing | `ScatterMaskToolsWidget.py:137-148 (updateDisk), 585-612 (pencil event handling)` | For scatter, disk test: (y-cy)^2 + (x-cx)^2 < radius^2 to find and mask point indices. siplot no scatter mask. |
| ☐ | H | M | Scatter mask polygon selection | `ScatterMaskToolsWidget.py:107-122 (updatePolygon with point-in-polygon test)` | silx uses Polygon.is_inside(y, x) to test each point. siplot no scatter mask. |
| ☐ | H | S | Mask level spinbox (1-255) with tooltip | `_BaseMaskToolsWidget.py:583-592` | silx has spinbox 'Mask level: [dropdown]' with tooltip explaining levels. siplot no level concept (boolean mask). |
| ◐ | M | M | Mask visibility overlay with colormap | `MaskToolsWidget.py:371-392 (_updatePlotMask creates MaskImageData item), _BaseMaskToolsWidget.py:984-1010 (_setMaskColors)` | silx renders mask as a separate MaskImageData item with colormap (linear, vmin=0, vmax=255) to show all levels. Current level highlighted at full opacity, others at half opacity. siplot calls plot. |
| ◐ | M | M | Mask display sync on active item change | `MaskToolsWidget.py:402-433 (showEvent/hideEvent), 434-443 (_activeImageChanged)` | silx detects active image change in plot, updates mask geometry and display automatically. siplot requires manual reset_geometry() call by user. |
| ◐ | M | S | Drawing tool: Pencil/Brush with variable width | `_BaseMaskToolsWidget.py:822-846 (pencilSpinBox 1-1024, slider 1-50), MaskToolsWidget.py:854-876` | siplot has brush_size as u32 and slider 1-50, but no spin box alternative, no text input, no sync between slider and numeric input (silx syncs via _pencilWidthChanged at line 1057-1069). |
| ◐ | M | S | Drawing tool: Eraser (unmask) | `_BaseMaskToolsWidget.py:806-810 (maskStateGroup radio buttons Mask/Unmask, Ctrl modifier toggles)` | siplot has Eraser tool but no Ctrl-modifier toggle for Mask/Unmask mode toggle. silx always draws at current level (1-255), eraser removes only that level (line 195-196). siplot boolean so no le |
| ◐ | M | S | Clear mask (per level or all) | `_BaseMaskToolsWidget.py:198-205 (clear method), 655-670 (Clear/Clear All actions)` | siplot has clear() which fills entire mask with false. silx clear(level) sets all pixels with that level to 0 only. silx has separate Clear Current Level and Clear All Levels buttons. |
| ☐ | M | S | Invert mask (per level) | `_BaseMaskToolsWidget.py:207-218, 645-653 (invert action and handler)` | silx invert(level) swaps masked/unmasked regions at that level. siplot has no invert operation. Keyboard shortcut Ctrl+I in silx. |
| ☐ | M | S | Mask NaN/non-finite values | `_BaseMaskToolsWidget.py:296-304 (updateNotFinite), 941-952 (button)` | silx button 'Mask not finite values' masks NaN and infinite pixels. siplot missing entirely. |
| ☐ | M | S | Mask transparency/alpha slider | `_BaseMaskToolsWidget.py:554-577 (slider range 3-10, affects alpha in _setMaskColors)` | silx has horizontal slider to control mask overlay opacity from transparent (3) to opaque (10). siplot has hardcoded color in new() line 36 with fixed semi-transparent alpha. |
| ◐ | M | S | Mask interaction mode vs pan/zoom | `_BaseMaskToolsWidget.py:1115-1161 (mode activation sets plot.setInteractiveMode()), 1096-1106 (interaction mode changed handler)` | silx seamlessly switches plot to drawing mode when tool is activated, disables pan/zoom. Example must manually set PlotInteractionMode::Select. No auto-switch in widget itself. |
| ☐ | M | S | Mask/Unmask mode toggle with Ctrl modifier | `_BaseMaskToolsWidget.py:790-810 (radio buttons), 1169-1178 (_isMasking checks Ctrl)` | silx has Mask/Unmask radio buttons and Ctrl-modifier toggle. siplot has Eraser tool but no radio button state or Ctrl handling. |
| ☐ | M | S | Load colormap range button | `_BaseMaskToolsWidget.py:883-892, 880-896 (MaskToolsWidget override)` | Button 'Set min-max from colormap' copies colormap vmin/vmax (or auto min/max) to threshold range fields. siplot no threshold masking. |
| ◐ | M | S | Pencil line interpolation between drag events | `MaskToolsWidget.py:849-876 (updateLine if lastPencilPos != current)` | silx draws lines between consecutive pencil positions via updateLine(). siplot draws only circular regions at current position, gaps if pointer jumps. |
| ☐ | L | S | Custom mask color per level (per-level RGB) | `_BaseMaskToolsWidget.py:394-398 (_defaultColors, _overlayColors arrays), 1026-1046 (setMaskColors, getMaskColors)` | silx allows user to set custom RGB color for each mask level (1-255), defaulting to gray. siplot single fixed color (red in example). |
| ☐ | L | S | Pan/Browse tool (no drawing) | `_BaseMaskToolsWidget.py:720-722 (PanModeAction added)` | silx includes explicit pan button (Browse) to disable drawing and return to pan/zoom. siplot only has tool selection without explicit pan mode. |
| ◐ | L | S | Pencil size sync (spinbox + slider) | `_BaseMaskToolsWidget.py:825-846, 1057-1069` | silx has spinbox (1-1024 range) and slider (1-50 range) that stay in sync via _pencilWidthChanged. siplot only has slider. |

## Actions, Toolbars, Tool Buttons  — 11✅ 8◐ 23☐

siplot implements a core minimal toolbar with 11 built-in buttons (Reset Zoom, Select/Pan/Zoom modes, X/Y invert, X/Y log scale, Major/Minor grid, Keep aspect ratio, Crosshair cursor) delivered through show_toolbar() / show_toolbar_with() / show_with_toolbar() convenience methods. The ToolbarResponse struct exposes state changes. However, siplot is missing 20+ discrete actions that silx ships: Zoom Back, Zoom In/Out, X/Y axis autoscale, Curve Style toggle, Colormap/Colorbar dialogs, Pan with arrow keys, Show Axis, axis origin menus (no explicit X invert button), Pixel Intensity Histogram, Median Filter variants, Fit dialog, Data aggregation mode selector, Save/Print/Copy I/O actions, and the Ruler measurement tool. Notable fidelity gaps: (1) no zoom history stack for "Zoom Back"; (2) no separate autoscale-per-axis toggles; (3) no curve-style cycling UI; (4) no icon fallback for X invert (only Y invert present); (5) toolbar is hardwired—no composable toolbar builder pattern like silx's ToolBar classes; (6) no I/O actions (Save/Print/Copy); (7) no specialized tool windows (histogram, median filter, fit, colormap dialogs) integrated into toolbar; (8) no axis-enabled menu for zoom mode; (9) profile toolbar is minimal (no dimension or mode toggles); (10) no ruler/measure tool button; (11) no limits toolbar with editable fields.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ◐ | H | L | Save Action (PNG/SVG/data formats) | `actions/io.py:77-747` | siplot provides save_graph() and encode_png() in render::save module but NO toolbar action button. User must call save_graph() explicitly. silx SaveAction opens file dialog, auto-detects format, sa |
| ☐ | H | M | Zoom Back (limits history pop) | `actions/control.py:101-122` | No zoom limits history stack implemented; silx stores history with getLimitsHistory().pop() and provides ZoomBackAction. siplot would need to retain previous limits in a deque and expose a 'zoom_ba |
| ☐ | H | M | X Axis Autoscale (separate toggle) | `actions/control.py:172-197` | silx provides XAxisAutoScaleAction (checkable) that toggles autoscale per axis and calls resetZoom when enabled. siplot resets both axes together; no per-axis autoscale state/UI. |
| ☐ | H | M | Y Axis Autoscale (separate toggle) | `actions/control.py:199-224` | Same as X autoscale: silx YAxisAutoScaleAction toggles Y-only autoscale with resetZoom on enable. siplot has no per-axis autoscale state. |
| ☐ | H | M | OutputToolBar composite (Copy/Save/Print) | `tools/toolbars.py:71-113` | silx OutputToolBar combines Copy/Save/Print actions. siplot has no toolbar-composable I/O actions. |
| ☐ | M | L | Pixel Intensities Histogram Action | `actions/histogram.py:1-180+` | silx PixelIntensitiesHistoAction opens a tool window (PlotToolAction-based) with live pixel value histogram + min/max/mean stats. siplot has no histogram tool window action. |
| ☐ | M | M | Curve Style Cycling (lines/lines+marks/marks) | `actions/control.py:317-350` | silx CurveStyleAction cycles through (False,False) → (True,False) → (True,True) → (False,True) → (False,False) by toggling plot.isDefaultPlotLines() and plot.isDefaultPlotPoints(). No equivalent in eg |
| ☐ | M | M | Colorbar Toggle (ColorBarAction) | `actions/control.py:452-488` | silx ColorBarAction toggles plot.getColorBarWidget().setVisible(). siplot renders colorbar from plot.colormap and chrome request, but no toggle action/button in toolbar to hide it. |
| ◐ | M | M | Zoom Mode Action (with optional axes menu) | `actions/mode.py:45-106, toolbars.py:50-51` | Zoom mode button exists (checkable). silx ZoomModeAction has optional ZoomEnabledAxesMenu to select which axes zoom affects. siplot zoom affects all axes with no menu. |
| ☐ | M | M | Copy to Clipboard Action | `actions/io.py:848-927` | silx CopyAction exports plot image to clipboard. siplot has no copy action or toolbar button. |
| ◐ | M | M | InteractiveModeToolBar composite | `tools/toolbars.py:37-69` | silx provides InteractiveModeToolBar class that can be added to any QToolBar. siplot show_toolbar() is a method on PlotWidget; no standalone toolbar composability. |
| ◐ | M | M | CurveToolBar composite | `tools/toolbars.py:179-227` | silx CurveToolBar provides Reset Zoom, X/Y Autoscale, Grid, Curve Style, Crosshair. siplot has most but no composition pattern; missing Curve Style and per-axis Autoscale. |
| ☐ | M | M | LimitsToolBar (editable X/Y min/max fields) | `tools/LimitsToolBar.py:35-123` | silx provides toolbar with editable FloatEdit widgets for X/Y limits. siplot has no editable limits toolbar. |
| ☐ | M | S | Zoom In (1.1x factor) | `actions/control.py:124-146` | No dedicated Zoom In button in toolbar. Mouse wheel zoom exists in interaction math but no toolbar button. Silx uses applyZoomToPlot() with factor 1.1. |
| ☐ | M | S | Zoom Out (1/1.1x factor) | `actions/control.py:148-170` | No dedicated Zoom Out button. siplot relies on wheel/interaction; no toolbar button equivalent. |
| ☐ | L | L | Print Action | `actions/io.py:747-847` | silx PrintAction opens print dialog and uses qt.printer to render. siplot has no print action or toolbar button. |
| ☐ | L | L | Median Filter Actions (1D/2D) | `actions/medfilt.py:49-150+` | silx MedianFilterAction / MedianFilter1DAction / MedianFilter2DAction open tool window for real-time median filtering. siplot has no median filter dialog or action. |
| ☐ | L | L | ScatterVisualizationToolButton | `PlotToolButtons.py:480-549` | silx provides menu for scatter visualization modes (Points, Grid, Triangulation, Delaunay) + binned stats. siplot has no scatter visualization selector. |
| ☐ | L | M | Pan with Arrow Keys Toggle | `actions/control.py:603-625` | silx PanWithArrowKeysAction toggles plot.isPanWithArrowKeys(). siplot has no API or toolbar button for this feature. |
| ☐ | L | M | Show Axis Toggle | `actions/control.py:627-651` | silx ShowAxisAction toggles plot.setAxesDisplayed(). siplot chrome always renders axes; no API to hide them or toolbar button. |
| ☐ | L | M | Close Polygon Interaction Action | `actions/control.py:653-683` | silx ClosePolygonInteractionAction calls plot.interaction()._validate() when in polygon draw mode. siplot has ROI editor but no polygon-specific close action. |
| ☐ | L | M | Data Aggregation Mode Selector | `actions/image.py:45-104` | silx AggregationModeAction provides None/Max/Mean/Min filter menu for aggregated images. siplot has no aggregation mode selector. |
| ☐ | L | M | ProfileToolButton (1D/2D profiles) | `PlotToolButtons.py:304-392` | silx provides menu to switch 1D vs 2D profile computation. siplot show_profile_toolbar() has None/H/V/L/R but no 1D/2D dimension toggle. |
| ☐ | L | M | SymbolToolButton (marker/size) | `PlotToolButtons.py:458-478` | silx provides menu to set marker symbol and size. siplot has no symbol selector button. |
| ☐ | L | M | RulerToolButton (distance measurement tool) | `tools/RulerToolButton.py:83-180+` | silx RulerToolButton toggles a LineROI for distance measurement with live label. siplot has no ruler tool button. |
| ◐ | L | S | Grid Toggle (both/major modes) | `actions/control.py:284-315` | siplot toggles major grid and minor grid separately (two buttons), both shown in toolbar. silx GridAction has mode param (both vs major). siplot default is major-grid-only with optional minor ov |
| ☐ | L | S | OpenGL Backend Toggle | `actions/control.py:685-753` | silx OpenGLAction switches between OpenGL and Matplotlib backends. siplot uses wgpu exclusively; no backend selection UI needed. |
| ◐ | L | S | ImageToolBar composite | `tools/toolbars.py:115-177` | silx ImageToolBar provides Reset Zoom, Colormap, Aspect, X/Y axis origin buttons. siplot has these but no composition pattern. |
| ◐ | L | S | AspectToolButton (menu: keep/don't keep) | `PlotToolButtons.py:57-125` | silx provides AspectToolButton with menu containing two actions. siplot is a simple toggle button. |
| ◐ | L | S | XAxisOriginToolButton (menu: invert/non-invert X) | `PlotToolButtons.py:193-199` | silx provides menu-style button. siplot is a simple toggle. |
| ☐ | L | S | ProfileOptionToolButton (sum/mean) | `PlotToolButtons.py:227-302` | silx provides menu button to switch profile aggregation (sum vs mean). siplot has no ProfileOptionToolButton in toolbar. |

## Composite views (ImageView/ScatterView/StackView/CompareImages/ComplexImageView/ImageStack)  — 15✅ 0◐ 29☐

siplot implements ImageView, ScatterView, StackView, and CompareImages at a basic functional level with core visualization features. ImageView has side histograms and axis sync but lacks RadarView (position overview), profile toolbar integration, and histogram access API. ScatterView has value-coloured scatter, an X/Y/Data/Index position-info panel, and programmatic selection-mask accessors, but is still missing the full on-plot mask-drawing widget and per-point statistics. StackView supports frame browsing plus 3D volume browsing (perspective/axis selection, automatic transposition, and per-dimension axis labels) but still lacks the 3D-profile toolbar and per-axis calibration. CompareImages has 4 basic modes (A/B/split/subtract) but lacks vertical/horizontal line separators, SIFT keypoint alignment, composite RGB modes, alignment modes (origin/center/stretch), and the full visualization mode set. ComplexImageView and ImageStack are entirely absent. Overall, these are lightweight versions capturing the core interaction model but missing several advanced features, UX details, and specialized tooling from silx.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ☐ | M | L | ImageView: colorbar display | `ImageView.py:501` | No ColorBarWidget; silx displays colorbar to the right of image |
| ☐ | M | L | ScatterView: colorbar widget | `ScatterView.py:83-88` | No ColorBarWidget; silx displays colorbar to right of scatter plot |
| ✅ | M | L | StackView: perspective selection (axis selection for 3D browsing) | `StackView.py:364-397` | `StackPerspective` + `perspective_ui` combo browses dimension 0, 1, or 2; `set_volume`+`stack_frame` auto-transpose per perspective |
| ☐ | M | M | ImageView: show/hide side histograms | `ImageView.py:552-559` | No toggle for histogram visibility; histograms always displayed |
| ☐ | M | M | ImageView: valueChanged signal (pixel/histogram hover) | `ImageView.py:381-390,585-646` | No callback/signal for cursor position over image/histograms; silx emits (row, col, value) or (NaN, col, histo_value) |
| ✅ | M | M | ScatterView: position info panel (X, Y, Data, Index) | `ScatterView.py:90-101` | `show_position_info` shows X/Y/Data/Index, snapping X/Y/value/index to the picked scatter point (pixel-space pick, "-" when none) |
| ✅ | M | M | StackView: 3D transposition and dimension swapping | `StackView.py:409-441` | `stack_frame` re-slices the volume per perspective (silx transpose (1,0,2)/(2,0,1)); `set_perspective` rebuilds frames + axis labels |
| ☐ | M | M | CompareImages: vertical line separator (vline mode) | `CompareImages.py:124-133,397-407` | No draggable vertical line; silx allows vertical marker to split and independently pan/crop left/right images |
| ☐ | M | M | CompareImages: horizontal line separator (hline mode) | `CompareImages.py:135-144,397-407` | No draggable horizontal line; silx allows horizontal marker to split and independently crop top/bottom |
| ☐ | L | L | ImageView: RadarView (position overview widget) | `ImageView.py:486-490,494-500` | No RadarView/mini-map widget in bottom-right corner; silx shows full data range + current viewport as draggable rect |
| ☐ | L | L | ImageView: profile toolbar integration | `ImageView.py:451-453,692-697` | No ProfileToolBar access; silx allows interactive line/rectangle/cross profile extraction with embedded/popup windows |
| ☐ | L | L | ScatterView: mask tools widget (ScatterMaskToolsWidget) | `ScatterView.py:116-122` | No ScatterMaskToolsWidget; silx allows drawing/editing per-point selection masks on scatter data |
| ☐ | L | L | ScatterView: scatter profile toolbar | `ScatterView.py:141,306` | No ScatterProfileToolBar; silx allows line profile extraction over scatter points |
| ☐ | L | L | StackView: profile 3D toolbar (extract profiles over all frames) | `StackView.py:84,948-951` | No Profile3DToolBar; silx allows extracting line/rectangle profiles that project across all frames |
| ☐ | L | L | StackView: calibration support (per-axis scale/origin) | `StackView.py:551,565-566` | No calibration API; silx accepts Calibration objects for each 3D axis to transform pixel→data coords |
| ☐ | L | L | CompareImages: composite RGB modes (red-blue-gray channels) | `tools/compare/core.py:60-61` | No COMPOSITE_RED_BLUE_GRAY or COMPOSITE_RED_BLUE_GRAY_NEG modes; silx combines A into red channel, B into blue, with optional gray overlay |
| ☐ | L | L | CompareImages: alignment modes (origin/center/stretch/auto) | `tools/compare/core.py:66-72` | No AlignmentMode enum; silx supports resampling/registration: origin (no transform), center (center A/B), stretch (scale to match), auto (SIFT keypoints) |
| ☐ | L | L | CompareImages: SIFT keypoint detection and alignment | `CompareImages.py:45,350,517-518` | No keypoint detection; silx detects SIFT features, shows them as scatter overlay, computes affine transform |
| ☐ | L | L | ComplexImageView: complex-valued image display | `ComplexImageView.py:259-310` | No ComplexImageView widget; silx supports amplitude/phase/real/imag/complex-magnitude modes for 2D complex data |
| ☐ | L | L | ImageStack: lazy frame loading from URLs | `ImageStack.py:148-200` | No ImageStack widget; silx loads HDF5/image URLs on demand with threading, prefetch queue, and progress overlay |
| ☐ | L | L | ImageStack: URL selection table and browser | `ImageStack.py:65-128,337-392` | No URL browser UI; silx provides table to add/remove URLs, slider to navigate them, with toggleable visibility |
| ☐ | L | L | ImageStack: prefetch queue for smooth browsing | `ImageStack.py:251-293` | No prefetch mechanism; silx preloads next N frames in background threads |
| ☐ | L | M | ImageView: getHistogram() API | `ImageView.py:699-725` | No public API to retrieve cached histogram data; silx returns dict with data + extent |
| ☐ | L | M | ImageView: profile window behavior (popup/embedded) | `ImageView.py:392-401,656-690` | No ProfileWindowBehavior enum; silx allows embedded side profiles for h/v/cross or popup-only modes |
| ☐ | L | M | ImageView: aggregation mode action (for multi-band images) | `ImageView.py:434-438,529-548` | No AggregationModeAction; silx supports mean/sum aggregation for multi-frame data |
| ✅ | L | M | ScatterView: getSelectionMask() and setSelectionMask() | `ScatterView.py:412-418` | `selection_mask`/`set_selection_mask` give programmatic per-point selection (u8 levels); set is length-validated + history-committed |
| ✅ | L | M | StackView: frame number / dimension labels | `StackView.py:799-827` | `set_dimension_labels`/`dimension_labels` (silx setLabels/getLabels); custom labels for 3D dims rotate onto X/Y axes per perspective |
| ☐ | L | M | StackView: aggregation mode for multi-band frames | `StackView.py:301-305` | No AggregationModeAction; silx supports mean/sum for multi-component image data |
| ☐ | L | M | CompareImages: affine transformation tracking | `CompareImages.py:880-889` | No getTransformation() API; silx returns AffineTransformation (tx, ty, sx, sy, rot) from alignment |
| ☐ | L | M | ComplexImageView: amplitude range dialog (max / delta in log10) | `ComplexImageView.py:50-155` | No _AmplitudeRangeDialog; silx allows interactive adjustment of complex magnitude display range (displayed_max, log10_delta) |
| ☐ | L | S | CompareImages: keypoint visibility toggle | `CompareImages.py:346-359` | No setKeypointsVisible() API; silx renders SIFT keypoints on top with separate colormap |
| ☐ | L | S | CompareImages: status bar with coordinate/value info | `CompareImages.py:191-193` | No CompareImagesStatusBar; silx shows pixel value, position, alignment mode in status bar |
| ☐ | L | S | ComplexImageView: complex display mode toolbar | `ComplexImageView.py:157-256` | No _ComplexDataToolButton; silx provides dropdown to select amplitude/phase/real/imag/complex mode |
| ☐ | L | S | ImageStack: waiting/loading overlay | `ImageStack.py:43` | No WaitingOverlay; silx shows progress spinner while loading frames |

## Stats, Legends, Profile, Fit, Position-Info, Print, Selection Dialogs  — 11✅ 4◐ 16☐

siplot implements core stats tracking (min/max/mean for X/Y and image scalars, with finite-value filtering), legend display with single-click row selection, eye-icon visibility toggles, and a right-click context menu (Wave 9), profile extraction helpers for horizontal/vertical/line/rect ROI types with automatic window placement mirroring silx, fit widget supporting linear and gaussian estimation, and limits dialog for axis control. Major gaps: no StatsWidget table UI, no profile-over-stack support, only 2 fit models vs silx's 10+, no PositionInfo readout bar, no RadarView overview, no print preview, no item selection dialog, and stats do not compute center-of-mass, coordinate min/max, integral, or delta.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ◐ | H | M | Stats engine: min/max/mean computation | `stats/stats.py:783-814` | siplot computes min/max/mean for X/Y curves and image scalars (ValueStats), with proper finite-value filtering, but missing: (1) delta (max - min), (2) center-of-mass (weighted sum), (3) coordinate |
| ☐ | M | L | Stats context framework (data masking, bounds clipping, ROI filtering) | `stats/stats.py:143-600` | silx has a full Stats class + _StatsContext hierarchy that clips data to plot limits or ROI bounds before computing stats. siplot only computes stats over the entire dataset at add/update time. No  |
| ☐ | M | L | StatsWidget UI (scrollable table of stat rows, per-item, update mode toggle) | `StatsWidget.py:200-700` | silx has a full table widget showing stats for active or all items, with rows for min/max/mean/com/coords, auto/manual update toggle, and formatters. siplot has show_stats(handle) and show_active_s |
| ☐ | M | L | Stats: ROI-scoped statistics (compute stats within a selected ROI) | `stats/stats.py:68-140; ROIStatsWidget.py` | Stats not computed per ROI. siplot tracks stats at add/update time only. |
| ☐ | M | M | Profile: line width / averaging method (mean vs sum) | `tools/profile/rois.py: _DefaultImageProfileRoiMixIn line_width, method properties` | No line width or method selection UI. siplot always averages (mean). silx supports line_width (pixels to average) and method selection (mean/sum). |
| ◐ | M | M | Fit: gaussian fit (amplitude, center, width, background) | `silx.math.fit.fittheories: GaussianArea, etc.` | siplot has GaussianEstimateFit which is a direct analytical peak-based estimate (amplitude, center, sigma, background). silx has both analytical AND iterative least-squares Gaussian fitting. egui v |
| ☐ | M | M | Stats: on-limits computation (mask data to current X/Y viewport range) | `stats/stats.py:216-300; StatsWidget with onLimits checkbox` | Stats always computed over full dataset. No option to mask to visible limits. |
| ☐ | M | S | Fit: fit range selection UI (xmin, xmax for fitting region) | `silx.gui.fit: FitWidget with xmin/xmax input fields` | egui FitWidget fits entire dataset. No UI to select a fitting range. |
| ☐ | L | L | Profile: profile-over-stack (slice extraction across stack dimension) | `tools/profile/rois.py:1058-1165 (ProfileImageStack* classes)` | No support for stack profiles (extracting multiple images' worth of profile lines and showing them as separate curves in the profile window). |
| ☐ | L | L | RadarView: miniature overview of full data extent with draggable viewport rect | `tools/RadarView.py:139-300` | No overview widget showing the full data range with a draggable rect indicating current view limits. |
| ☐ | L | L | Print preview: send plot to printable page with movable/resizable rect | `PrintPreviewToolButton.py:24-120` | No print preview or print button. Would require integrating with system print dialog and rendering to PDF/printer. |
| ✅ | L | M | Legend context menu (set active, map-to-Y-left/right, toggle points/lines, rename, remove) | `LegendSelector.py:697-845 (LegendListContextMenu)` | Wave 9: right-click on a legend row pops an `egui::Response::context_menu`. Curve rows get Set Active / Map to Y Left / Map to Y Right / checkable Points / checkable Lines / Rename / Remove; non-curve rows get a Set Active / Rename / Remove subset. Points/Lines toggle losslessly via a UI-only restore cache; rename via an `egui::Window`. Backend was already primitively complete. **GPU/UI-UNVERIFIED:** the menu render, right-click interaction, and rename window need a GPU `RenderState` no crate test constructs; only the pure style transforms are unit-tested. silx's copy-color / toggle-colormap actions are not ported (egui has no equivalent clipboard color affordance here). |
| ☐ | L | M | Fit: Lorentzian fit (amplitude, center, width, background) | `silx.math.fit.fittheories: Lorentzian` | No Lorentzian fitting. |
| ☐ | L | M | Fit: Pseudo-Voigt fit (Gaussian + Lorentzian blend) | `silx.math.fit.fittheories: PseudoVoigt` | No Pseudo-Voigt fitting. |
| ☐ | L | M | PositionInfo readout bar (X, Y, plus custom converters) | `tools/PositionInfo.py:64-250` | No toolbar-mounted label showing current mouse coordinates in data space, with customizable converters (e.g. polar coords, distances). |
| ☐ | L | M | ItemsSelectionDialog: multi-select table of plot items filtered by kind | `ItemsSelectionDialog.py:40-200` | No modal dialog for selecting multiple plot items from a table (used by fit tool, stats tool). Would require a dedicated dialog UI. |
| ◐ | L | S | Profile: cross profile (horizontal + vertical lines from point) | `tools/profile/rois.py:663-700; _ProfileCrossROI` | siplot has add_horizontal/vertical_profile_curve helpers but no dedicated cross ROI display UI. Can extract both profiles but no UI to show both curves simultaneously in the profile window. |
| ◐ | L | S | Fit widget UI: show fit results table (chi-squared, parameters, errors) | `silx.gui.fit.FitWidget: results table with parameter names, values, errors` | Shows parameter names and values in a grid. Missing: (1) chi-squared or goodness metric, (2) error estimates for each parameter, (3) covariance matrix display. |
| ☐ | L | S | Stats: center-of-mass (weighted sum of positions) | `stats/stats.py:881-910` | No COM computation in siplot. |
| ☐ | L | S | Stats: coordinate of min/max (argmin/argmax with axis lookup) | `stats/stats.py:841-878` | No argmin/argmax computation. |
| ☐ | L | S | Stats: integral (sum of all values, optionally weighted by axis) | `stats/stats.py via silx.math.combo` | No integral/sum stat. |
