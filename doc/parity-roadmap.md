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

**CurvesROIWidget integration (main, not pushed):** ≈220 Done · ~101 open — the last open H row closed: per-curve ROI
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
| ✅ Done | M | M | Per-axis timezone support | items/axis.py:278-294 (get/setTimeZone) | Full silx surface (`a61a944`,`0c78553`,`895cee3`): `TimeZone{Utc, FixedOffset{seconds_east}, Named(tz::TimeZoneRef)}` + `TimeZone::named(name)`/`local()`; `dtime_ticks` `_tz` layout (`calc_ticks_tz`/`format_tick(s)_tz`, `from`/`to_epoch_seconds_tz`) over the single instant-aware `offset_at`, `Plot::x_time_zone`/`set_x_time_zone` threaded through chrome. `Utc`=`"UTC"`, `FixedOffset`=`datetime.timezone(timedelta)`, `Named`/`local`=arbitrary `dateutil.tz` zone / `None` via the bundled IANA tz DB (`tzdb`/`tz-rs`, DST-aware). Nuances: `local()` resolves the system zone once at construction (not re-resolved per render); the `to_epoch` local→UTC inversion uses the standard two-step refine (DST fold ≈ default). Tested incl. EST/EDT + a spring-forward window |
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
| ✅ Done | M | L | Scatter mode-specific picking | scatter.py:804-860 | All modes wired into the live pick (`ScatterView::show_position_info` dispatches by mode: REGULAR_GRID cell→index, BINNED_STATISTIC bin→nearest-member via `nearest_candidate_in_data`, POINTS/SOLID nearest-pixel). IRREGULAR_GRID now closed by the triangle-mesh render redesign: `irregular_grid_triangles` builds a flat-shaded quadrilateral cell per point (silx `_quadrilateral_grid_as_triangles`), and `irregular_grid_pick` maps the cell under the cursor back to its point (silx picked-vertex `// 4`). See row 1086. |
| ✅ Done | M | M | Image per-pixel validity mask before upload | items/image.py:209-251 | `ScalarMask` + Plot2D wiring (`8ef46c2`) exist; `ScalarMask::apply` sets masked pixels to `f32::NAN`, now rendered transparent by the shader's non-finite → nan_color branch (`0356d5c`, with row 1140). Composite GPU render verified structurally (WGSL validates against naga); on-screen pixels GPU-unverified |
| ✅ Done | M | L | Marker custom-callback constraint | items/marker.py:208-235 | Arbitrary `setConstraint(fn)` form ported as `Marker::drag_with(to, constraint)` — the closure receives the cursor's data target `(x,y)` and returns the filtered position, exactly like silx's callable (`setPosition` does `x,y = getConstraint()(x,y)`). `drag` now delegates to `drag_with` via `apply_constraint`, so `drag_with` is the single owner of "filter target → update kind coords". The closure stays call-time state (not stored) so `Marker` keeps `Clone`/`Debug`/`PartialEq`. H/V presets still on the marker as `MarkerConstraint`. Headless tests: point filtered, vline/hline single-axis, non-draggable no-op, preset delegation |
| ✅ Done | M | M | Histogram filled-region picking | histogram.py:244-290 (`__pickFilledHistogram`) | Wave 7: pure `pick_histogram(edges, values, baseline, x, y) -> Option<usize>` — strict bbox gate (x over edge span, y over `[min(0,vmin), max(0,vmax)]`), `searchsorted(edges, x, side="left")-1` (clamped) bin index via `partition_point`, inclusive y test against the bar `[baseline, value]`/`[value, baseline]`. None on malformed/all-NaN/out-of-box/no-hit |
| ☐ Missing | M | L | Image per-pixel alpha map | items/image.py:462-500 (get/setAlphaData) | `ImageData` has only global `alpha:f32`; no per-pixel alpha array (needs shader) |
| ✅ Done | L | L | Marker text anchor/alignment | items/marker.py | `TextAnchor` now wired into `chrome::draw_markers`: label placement goes through the pure, tested core — `TextAnchor::pixel_offset` ports silx `_TextWithOffset.__get_xy` (left→+x/right→−x, top→+y-down/bottom→−y), then `rect_offset` positions the galley so the named anchor lands on the marker point. Per-kind defaults match silx `addMarker`: point & vline `ha="left"`→`TopLeft`, hline `ha="right",va="top"`→`TopRight` (text at the right edge). Point/vline padding `(5,3)`/`(size/2+3,3)`, hline `(5,3)`. Anchor/offset core headlessly tested; the painter draw stays GPU-unverified |
| ✅ Done | L | M | Shape overlay-flag separate data layer | items/shape.py:54-73 (`isOverlay`) | `draw_shapes`/`draw_lines` take an `overlay: bool` filter; `PlotWidget` runs two passes — non-overlay shapes/lines render in the base data layer (above grid, UNDER the overlay items: ROIs, markers, crosshair), overlay shapes/lines (default) render in the overlay layer on top of the chrome (silx base-render vs `_drawOverlays`). `Line` shares the `_OverlayItem` base so it gets the same split. Field/setter model tested; the render-layer split is GPU/painter-unverified |
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
| ✅ Done | M | M | EllipseROI orientation/rotation | items/roi.py:875-1177 | `Roi::Ellipse` gained an `orientation` field (radii.0 along θ, radii.1 perpendicular; θ=0 ≡ the prior axis-aligned shape). All geometry is orientation-aware: vertex/handle positions (axis0 at θ, axis1 at θ+π/2), `contains` (project into the ellipse frame), `screen_rect` (rotated AABB), and the 27-point outline (silx rotated parametric form, mapped data→pixel). Dragging an axis handle off-axis sets that semi-axis AND rotates, mirroring silx `EllipseROI.handleDragUpdated`. Rotation geometry, contains-under-rotation, handle-follow, and AABB are headlessly tested |
| ✅ Done | M | M | CircleROI dedicated handle UI | items/roi.py:727-950 | Dedicated edit handles exist: `edges()` → `[Vertex(0) center, Vertex(1) perimeter]`, `move_edge` (V0 translate, V1 sets radius), rendered + hit-tested via `draw_roi_handles`/`roi_grab_at`. Tests `circle_handles_drag_center_and_radius`, `circle_perimeter_resize_works_under_inverted_y`. Matches frozen row 1171 ✅ (stale ◐ here from before Wave 12 handle-editing landed). |
| ✅ Done | M | M | CrossROI marker symbols | items/roi.py:133-185 | `1cbda70`: the drag handle is a square (silx `addHandle()` default symbol "s"); the full-span cross-hairs honor the ROI appearance's color/width/line-style. (silx's transient label-handle management is the manager-path concern, not the geometry) |
| ◐ Partial | M | L | ArcROI interaction sub-modes | items/_arc_roi.py | Both sub-modes' geometry now exist as pure cores: PolarMode editing is wired into the live handle drag (`Roi::move_edge`), and ThreePointMode is ported headlessly — `arc_from_three_points` (faithful `_createGeometryFromControlPoints`: closed-circle / collinear / general branches), `arc_control_points` (derive start/mid/end on the central radius, silx `_updateHandles`/`_updateMidHandle`), and `arc_three_point_drag` (move a control point → re-fit center+radius+angles, weight preserved). `arc_from_two_points` now delegates to `arc_from_three_points` (one geometry owner). Remaining ◐: the live canvas toggle between Polar and ThreePoint editing (per-ROI sub-mode state + button + `move_edge` dispatch) is GPU/UI and unwired — the live drag is PolarMode only. 4 headless tests |
| ✅ Done | M | S | Manager `sigCurrentRoiChanged` signal | tools/roi.py:346-347 | Wave 13: `PlotEvent::CurrentRoiChanged { previous, current }` emitted by `set_current_roi` only when the selection actually changes (mirrors silx `sigCurrentRoiChanged`) |
| ✅ Done | M | M | ROI handle-symbol customization | items/_roi_base.py:735-759 | `draw_roi_handles` renders the silx `addHandle` role symbols: `+` for translate/center handles (role "translate") and a 6px square `s` for vertex/edge handles (role "default"). The `o` glyph is silx's `PolygonROI._handleClose` (role "user", transient creation marker, items/roi.py:1244) from the `RegionOfInterestManager` creation path; siplot creates polygons via the `PlotInteraction` draw path and renders its faithful close target — the `SelectPolygon.updateFirstPoint` fill=None box (`draw_polygon_first_point`) |
| ✅ Done | M | M | ROI line style (dash/dot) on canvas | items/_roi_base.py (LineMixIn) | Wave 13: `ManagedRoi.line_style` (`RoiLineStyle::{Solid,Dashed,Dotted}`) resolved into `RoiAppearance.line_style` and drawn by the live path; manager combo edits it |
| ✅ Done | M | S | ROI line width on canvas | items/_roi_base.py:245 | Wave 13: `ManagedRoi.line_width` resolved into `RoiAppearance.line_width` on the live `draw_rois` path (was hardcoded 1.0pt); manager drag-value edits it |
| ✅ Done | M | S | Manager default-color application | tools/roi.py:782-797 | Wave 13: added `Plot::roi_color` (silx-default red) applied as the per-ROI fallback; manager color buttons now write `managed.color` and apply on the live plot |
| ✅ Done | M | M | ROI edge-position constraints | items/roi.py:666-718 + per-class | All silx-real edge constraints are enforced: Rect/Range handles flip around the fixed opposite (silx `RectangleROI._setBound` min/max) instead of collapsing; Arc keeps `inner<=outer` and `inner>=0` (`.max(0.0)` in `move_edge`); Band keeps `width>=0` and the orthogonal width-handle constraint (normal projection `.max(0.0)`, silx `__handleWidthUp/DownConstraint`). silx's ROI classes have no ratio-lock, grid-snapping, or inter-ROI collision behavior to port |
| ◐ Partial | L | L | BandROI sub-modes (Bounded/Unbounded) | items/_band_roi.py:172-250 | **Mis-attribution corrected:** silx BandROI has no "rotation sub-mode" — band orientation is already free via the begin/end handles (siplot `move_edge` Vertex 0/1). silx's actual sub-modes are `BoundedMode` (drawn as the 4-corner polygon, siplot's default via `band_corners`) and `UnboundedMode` (drawn as three parallel infinite lines). The Unbounded geometry core is now ported: `Roi::band_lines() -> BandLines` (faithful `BandGeometry.slope`/`intercept`/`edgesIntercept`), modeled as a `Sloped`/`Vertical` sum type so no field carries silx's `inf`-slope dual meaning. Remaining ◐: the live Bounded↔Unbounded render toggle (drawing the three view-clipped lines + a mode button) is GPU/UI and unwired. 4 headless tests |
| ✅ Done | L | M | ROI creation preview polish | tools/roi.py:493-510, 1101-1175 | Live overlay + polygon close target (`draw_polygon_first_point`, silx `SelectPolygon.updateFirstPoint`) present, and the mode indicator now lands: `PlotInteractionMode::roi_creation_message`/`PlotWidget::roi_creation_message` produce silx's `InteractiveRegionOfInterestManager.getMessage` string ("Select {SHORT_NAME}s ({n} selected)") for a host status bar (headlessly tested). Closes frozen row 1175. |
| ✅ Done | L | S | ROI fill enable/disable toggle | items/roi.py:531-539 | Wave 13: `ManagedRoi.fill` plumbed to `RoiAppearance.fill`; `set_roi_fill` API + manager checkbox toggle the interior fill on the live plot (silx `setFill`) |
| ✅ Done | L | S | Distinct Horizontal/VerticalLineROI kinds | items/roi.py:366-510 | `Roi::HLine { y }`/`Roi::VLine { x }`: single-Y/single-X full-span line ROIs distinct from HRange/VRange bands (one edge, one handle, exact-coord `contains`). `RoiDrawKind::HLine`/`VLine`→`DrawMode::HLine`/`VLine` (silx `SelectHLine`/`SelectVLine`), chrome draws a styled full-span line. Geometry/grab/draw-build tested; GPU draw unverified |
| ✅ Done | L | M | ROI right-click context menu | tools/roi.py:625-642 (`_feedContextMenu`) | Per-ROI canvas menu (silx `_createMenuForRoi`): ROI-name title + "Make current" (`set_current_roi`) + "Remove" (`remove_roi`); the target ROI is captured via `roi_grab_at` and held in `ui.data` for the menu's lifetime, and the high-level owner applies the signalled intent so CurrentRoiChanged/RoisCleared fire. For Arc/Band ROIs the menu now nests an "Interaction mode" submenu (silx `createMenuForInteractionMode`) over the ported `InteractionModeMixIn` model, applied via `set_roi_interaction_mode` → `RoiInteractionModeChanged`. Closes frozen row 1179. |
| ✅ Done | L | L | ROI save/load from file | CurvesROIWidget.py:194-210 (dictdump) | Data-layer save/load + widget wiring ported: `core::roi_io` `encode_rois`/`decode_rois` (hand-written line-oriented text format, no serde — same manual approach as the `.npy` mask path) round-trip every `Roi` variant plus name/color/line width+style/gap color/fill; the transient `selected` flag is omitted (silx `ROI.toDict` likewise). `save_rois(path,…)`/`load_rois(path)` are the file wrappers (silx `CurvesROIWidget.save`/`load(filename)`). `PlotWidget::save_rois_to_path`/`load_rois_from_path` are the testable widget-level wrappers (load replaces the set via `clear_rois`+`add_managed_roi`, emitting `RoisCleared` + N×`RoiAdded`); `save_rois_dialog`/`load_rois_dialog` open the native rfd file picker like the figure `save_dialog` (GPU/UI-unverified shims). 10 tests (9 `roi_io` round-trip/error cases + a GPU-backed `PlotWidget` save→mutate→load round-trip in `tests/roi_events.rs`). |
| ✅ Done | L | M | ROI keyboard shortcuts + naming UI | tools/roi.py:1072-1098 | Faithful port of silx `InteractiveRegionOfInterestManager.eventFilter`: pure `roi_key_action(key, command) -> RoiKeyAction` maps Enter→Validate and Delete/Backspace/Ctrl+Z(⌘Z)→UndoLast. **Mis-attribution corrected:** silx binds *no* per-shape keys ("R for Rect" was speculative) and has no named-creation dialog — ROI naming is via the manager (`ManagedRoi.name`, already present). Wired in `plot_widget` RoiCreate: the key dispatch runs through `roi_key_action`; UndoLast signals `roi_removed = last` to the single removal owner (`high_level::remove_roi`, like the context-menu Remove). Mapping headlessly tested; the live key/event wiring is GPU/UI-unverified |

### colormap-colorbar-mask

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✅ Done (W15) | H | M | ColormapDialog Stddev3/Percentile from raw pixels | ColormapDialog.py:240-280 | `ColormapDialog::apply` now autoscales from the active image's raw pixels (`Plot2D::get_image_pixels_raw`) via the pure `autoscale_range` (delegates to `AutoscaleMode::range` with the dialog's mode + percentiles) — Stddev3 (mean ± 3·std clamped to data range) and Percentile compute the real distribution range instead of falling back to MinMax; falls back to aggregated stats min/max only when no raw scalar pixels are retained |
| ✅ Done (W15) | H | M | Mask drawing tool: Rectangle | MaskToolsWidget.py:805-826 | On-plot rectangle draw: `MaskTool::draw_mode` arms a `DrawState` driven by `handle_shape_draw`; `rect_params_to_cells` (silx `int()` truncation) → `update_rectangle` on finish; `ImageView::handle_mask_shape_draw` paints the rubber-band preview (reuses `paint_draw_preview`/`feed_draw_state`) |
| ✅ Done (W15) | H | M | Mask drawing tool: Polygon | MaskToolsWidget.py:840-847 | On-plot polygon draw (click-to-add, snap-close) via the shared shape-draw wiring: `MaskTool::draw_mode` arm + `polygon_vertices_to_cells` (cast int64, swap `(x,y)`→`(row,col)` per silx `[:, (1,0)]`) → `update_polygon`/`polygon_fill_mask` on finish |
| ✅ Done (W15) | H | L | Mask drawing tool: Ellipse | MaskToolsWidget.py:828-838 | On-plot ellipse draw via the shared shape-draw wiring: `MaskTool::draw_mode` arm + `ellipse_params_to_cells` (center cast int64, y-semi→`radius_r`, x-semi→`radius_c`; radii stay float per silx) → `update_ellipse` on finish |
| ◐ Partial | M | M | Colormap catalog: matplotlib-dynamic loading | colors.py:938-955 | `6b0b2f9`: catalog grown 15 → 45 by shipping the full `colorous` gradient set (d3-scale-chromatic / ColorBrewer overlap with matplotlib) statically. True runtime matplotlib-dynamic loading is impossible without a Python/matplotlib dependency (out of scope); the static catalog is the faithful substitute |
| ✅ Done | M | M | ColormapDialog histogram display | ColormapDialog.py:1227-1295 (`computeHistogram`), :831-848 (display) | Wave 6: pure `compute_histogram` (silx `nbins=clamp(2,min(256,⌊√N⌋))`, finite-min/max or supplied range, log-space binning with edges → linear) + `set_histogram`/`histogram`/`clear_histogram` (silx `setHistogram`/`getHistogram`). Dialog auto-derives from the active image's raw pixels (recompute only on (re)open / normalization change); `draw_histogram_panel` renders normalized gray bars + a colormap gradient strip across `[vmin,vmax]` + vmin/vmax markers. Pure compute + API headlessly tested; on-screen panel GPU-unverified |
| ✅ Done | M | M | Mask threshold UI (below/between/above) | _BaseMaskToolsWidget.py:265-294, :848-937, :1182-1225 | Wave 7: threshold row in the ImageView mask-draw toolbar — `ThresholdMode` selector + min/max fields shown per mode (min for below/between, max for between/above, silx minLineEdit/maxLineEdit visibility) + `Mask below/between/above` Apply button calling the tested `update_threshold` (below→min, above→max, between→both) at the current level, commit + re-upload. `MaskToolsWidget` holds `threshold_mode`/`threshold_min`/`threshold_max` (silx group defaults: below checked, edits at 0; unit-tested). Toolbar row GPU-unverified |
| ◐ Partial | M | L | Mask file save (npy/edf/tif/h5/msk) | MaskToolsWidget.py:104-141 | `.npy` AND `.edf` encode now ported: `render::save::encode_mask_edf` (hand-written fabio-style ASCII `{…}` header + raw `uint8` C-order body, 512-aligned, no new dep) with `MaskToolsWidget::write_edf`/`save_edf`/`save_mask_edf` mirroring the npy path. Remaining: no file dialog; TIFF/HDF5/msk formats need external crates (`tiff`/`hdf5`/fabio), deferred under the no-new-dep pattern |
| ◐ Partial | M | L | Mask file load (npy/edf/tif/h5/msk) | MaskToolsWidget.py:589-629 | `read_npy` AND `read_edf` decode+apply a 2D uint8 mask through the shared `apply_loaded_mask` crop/pad+commit owner (`render::save::decode_mask_edf` reads the fabio `{…}` header for Dim_1/Dim_2/DataType, body taken right after `}`); `load_edf`/`load_mask_edf` mirror the npy file wrappers. Missing: file dialog, TIFF/HDF5/msk formats (external-crate-bound, deferred), HDF5 dataset selection |
| ◐ Partial | M | S | Mask transparency/alpha slider UI | _BaseMaskToolsWidget.py:554-577 | Wave 7: Transparency slider in `MaskToolsWidget::show_toolbar` (the coloured-overlay toolbar) routes through the tested `set_transparency` (silx `transparencySlider` → `_setMaskColors`: selected level at this alpha, others at half) and re-renders the overlay LUT. Slider GPU-unverified. N/A for ImageView's mask (rendered as NaN holes, no overlay alpha) |
| ✅ Done | M | S | Mask Mask/Unmask toggle + Ctrl modifier | _BaseMaskToolsWidget.py:790-810 | `d85c751`: `mask_state` Mask/Unmask radio for shape draws + `effective_do_mask = base ^ ctrl` (silx `_isMasking()`); Ctrl (egui `command`, = Cmd on macOS per Qt) inverts all draws, captured once per pencil stroke. Pencil/eraser keep their brush direction. Truth-table tested; toolbar GPU-unverified |
| ✅ Done | M | S | Mask invert per-level UI | _BaseMaskToolsWidget.py:207-218 | Wave 7: "invert" button in the ImageView mask-draw toolbar calls the tested `MaskToolsWidget::invert`. `d85c751` adds the silx Ctrl+I shortcut (egui `COMMAND`+I) in `MaskToolsWidget::show_toolbar`. Button/shortcut GPU-unverified |
| ✅ Done | M | S | Mask not-finite (NaN/Inf) button | _BaseMaskToolsWidget.py:296-304 | `MaskToolsWidget::mask_not_finite` (silx `updateNotFinite`: stencil `!isfinite`, tested) + Wave 7 "mask non-finite" button in the ImageView mask-draw toolbar (masks the active image's NaN/Inf pixels at the current level, commits, re-uploads; enabled when mask geometry matches the image). Button/overlay GPU-unverified |
| ✅ Done | L | S | Mask per-level color override UI | _BaseMaskToolsWidget.py:394-398, 973-1042 | API done earlier (`set_mask_colors`/`reset_mask_colors`, faithful `setMaskColors`/`resetMaskColors`). This adds the getter `current_mask_color` (silx `getCurrentMaskColor`: per-level override if set, else base color, tested) and the `MaskToolsWidget::show_toolbar` UI: a color swatch reading/writing the current level's override + a "Reset color" button. Getter tested; toolbar swatch GPU-unverified |

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
| ◐ Partial | M | M | ComplexImageView amplitude-range dialog | ComplexImageView.py:50-155, items/complex.py:62-82, :199-212 | Wave 7: `ComplexMode::Log10AmplitudePhase` + `amplitude_phase_log_rgba(data, max, delta)` port silx `_complex2rgbalog` (clamp to displayed max `smax`, `log10(\|z\|+1e-20)`, shift by `a.max()-delta`, normalize over `delta` decades) — amplitude → HSV value (siplot convention). `set_amplitude_range_info(max, delta)` / `amplitude_range_info()` = silx `_setAmplitudeRangeInfo`/`_getAmplitudeRangeInfo`, defaults max=None autoscale + delta=2 (`DEFAULT_AMPLITUDE_DELTA`). Composite math unit-tested. `87cd1d4` adds `show_amplitude_range_controls` (autoscale checkbox + Displayed Max + delta, silx `_AmplitudeRangeDialog`) with the tested `data_max_amplitude` seed. UI/GPU render dispatch GPU-unverified |
| ◐ Partial | L | L | StackView 3D-profile toolbar | StackView.py:948-951 | Data layer wired: `StackView::stack_aligned_profile`/`stack_line_profile` extract a profile from every frame along the browsed dimension over the loaded volume + current perspective (silx `Profile3DToolBar`'s `ProfileImageStack{HorizontalLine,VerticalLine,Line}ROI`), returning a `StackProfile`. Reuses the headlessly-tested row-253 cores. Remaining ◐: the `Profile3DToolBar` buttons + a side plot showing the stacked-profile image (GPU/UI; StackView needs a wgpu RenderState no test builds, so the methods are GPU-unverified wrappers) |
| ✅ Done | L | M | StackView calibration (per-axis scale/origin) | StackView.py:551-566 | `core::Calibration` {None, Linear} + `StackView::set_calibrations`/`calibrations`/`calibrations_axes`/`image_z` mirror silx `getCalibrations(array/axes)` + `_getImageOrigin/Scale/Z`; displayed image placed via the calibrated origin/scale (`add_image_with_geometry`). Pure helpers tested; GPU placement unverified |

### actions-toolbars

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ◐ Partial | M | M | Print: native dialog + printer submission | actions/io.py:747-845 | `print_graph()` GPU-readback shim exists; no native printer dialog or print-to-device backend |
| ✅ Done | M | M | ColormapAction floating-dialog toggle | actions/control.py:352-448 | `f3a9c54`: `ColormapDialog::toggle_button` is a checkable toolbar action that reflects/flips `dialog.open` (silx `ColormapAction`); wired into the `high_level_colormap_dialog` example toolbar row. Toolbar GPU-unverified |
| ✅ Done | M | M | ScatterToolBar viz-mode selector button | tools/toolbars.py:273-362 | `4477bc2`: `ScatterView::show_toolbar` adds a visualization-mode ComboBox (`ScatterVisualization::ALL`, silx order) that calls `set_visualization` on the picked mode. Toolbar GPU-unverified |
| ✅ Done | M | S | ProfileOption + line-width/method (sum vs mean) | PlotToolButtons.py:227-301 | `340b80c`: `ProfileWindow` shows a Width DragValue + Mean/Sum ComboBox that drive `profile_for_roi` through the line-width/method extractors (no longer mean-only). Window UI GPU-unverified |
| ✅ Done | M | S | ProfileToolButton (1D vs 2D selector) | PlotToolButtons.py:304-391 | Standalone `widget::tool_buttons::ProfileToolButton`: a reusable dropdown button with the silx 1D/2D dimension state (`set_dimension` rejects out-of-range, `dimension`), the silx STATE action/tooltip strings, and an egui popup `ui() -> Option<u8>` (silx `sigDimensionChanged`). State core headlessly tested; the popup render is GPU/UI-unverified |
| ✅ Done | M | M | SymbolToolButton (marker symbol + size) | PlotToolButtons.py:394-477 | Standalone `widget::tool_buttons::SymbolToolButton`: a reusable dropdown with a size slider (silx range `1..=20`, default 6.0) over `Symbol::ALL`, emitting `SymbolToolChange::{Symbol,Size}` (silx `_markerChanged`/`_sizeChanged`). `set_size` clamps to the slider range; state core headlessly tested; the popup render is GPU/UI-unverified |
| ✅ Done | M | M | ScatterVisualizationToolButton | PlotToolButtons.py:550+ | `4477bc2`: the visualization-mode selector now lives in the `ScatterView` toolbar (`ScatterVisualization::ALL` ComboBox → `set_visualization`). Toolbar GPU-unverified |
| ✅ Done | L | L | SaveAction PDF/EPS/JPEG | PlotWidget.py:3232-3242 | All three exported, hand-written with no new dep. EPS/PDF are SVG-style raster-embedding substitutes for silx's matplotlib vector output: `render::save::encode_eps` (PostScript `colorimage`, Y-flip matrix, ASCII-hex body) and `encode_pdf` (single-page `/DeviceRGB` `/ASCIIHexDecode` image XObject with a byte-offset xref). JPEG is a hand-written baseline JFIF encoder (`render::jpeg::encode_jpeg` — T.81 Annex K tables IJG-scaled to quality 90, 4:4:4, canonical Huffman, 0xFF stuffing), the same on-disk container PIL writes for silx. `SaveFormat::Eps`/`Pdf`/`Jpeg` + `from_extension` + dispatch + the native save-dialog filters wired. PNG/PPM/SVG/TIFF/JPEG/EPS/PDF supported; only `.ps` remains unmapped |
| ✅ N/A | L | S | Y2AxisAutoScaleAction | actions/control.py:172-230 | Not a silx feature: silx ships only `XAxisAutoScaleAction` + `YAxisAutoScaleAction` (no Y2 variant). The capability already exists (`set_y2_autoscale`, honored by the autoscale path); a Y2 toolbar button would be a non-silx UI extension, deliberately deferred (Wave 7 note: a third autoscale button bloats the row). Marked N/A rather than fabricate a silx counterpart. |
| ✅ Done | L | S | ClosePolygonInteractionAction | actions/control.py:653-683 | `DrawState::validate` ports silx `SelectPolygon._validate`: closes the in-progress polygon at its committed vertices regardless of cursor position (same `len > 2` gate as the snap-close path; no-op when idle / too few vertices / non-polygon mode). Wired in `apply_interaction` to the Enter key while a `RoiCreate(Polygon)` draw is active, so a polygon finishes without snapping back to the first point. `validate` unit-tested; the key wiring is GPU-unverified |
| ☐ — | L | S | OpenGLAction (backend selector) | actions/control.py | N/A: siplot is wgpu-only (intentional) |
| ✅ Done | L | M | LimitsToolBar (editable X/Y min/max fields) | tools/toolbars.py | `PlotWidget::show_limits_toolbar` renders the silx X/Y min/max FloatEdit row; edits apply via `set_graph_x_limits`/`set_graph_y_limits` with `ordered_limits` swapping reversed bounds (silx `_xFloatEditChanged`). Closes frozen row 1241. |

### stats-profile-fit-positioninfo

| status | P | E | feature | silx ref | gap |
|---|---|---|---|---|---|
| ✅ Done | M | M | Stats context (viewport/ROI masking, live binding) | stats/stats.py:143-600 | Live binding done (stale "pure engine only" note corrected). Viewport masking: `StatScope::OnLimits{x_range,y_range}` (the on-visible-data toggle) clips to the visible rectangle each recompute. Auto-recompute on viewport change: `StatsWidget::recompute` takes the current viewport every frame and (in `UpdateMode::Auto`) recomputes — the immediate-mode equivalent of silx's `_updateStats` on `sigVisibleDataChanged`. ROI masking is the separate `ROIStatsWidget` path (`image_roi_stats`/`curve_roi_stats`, `roi.contains` mask, rows above) — faithful to silx `_StatsContext`, which itself carries only the onlimits clip, no per-pixel user mask. Qt item-change signals are N/A (immediate mode rebuilds the rows from `item_records` each frame). |
| ✅ Done | M | M | StatsWidget live item binding | StatsWidget.py:200-700 | Bound to the live plot both ways (stale "not docked / no live binding" note corrected): `feed_active_stats`/`show_active_stats_widget` track the **active** item, and `feed_all_stats`/`show_all_stats_widget` track **all** items with retained scalar data (one row each, legend-labelled) — silx `setDisplayOnlyActiveItem`'s two modes, chosen by which method the host calls. Each recomputes from the live retained data every frame (Auto) honoring the on-visible-data viewport clip; items without retained scalar data (RGBA images, triangles, shapes, markers) are skipped. Composed pure cores (`retained_data_to_stats_input`, `Stats::for_curve`/`for_image`, `recompute` scope) headlessly tested; the `PlotWidget` feed + table render need a wgpu RenderState no test builds, so they are GPU/PlotWidget-unverified. |
| ✅ Done | M | M | Profile line width / method (mean vs sum) | tools/profile/rois.py:220-260, core.py:204-270, image/bilinear.pyx:391-466 | Wave 7: `ProfileMethod{Mean,Sum}` + `aligned_profile_values(.., position, roi_width, horizontal, method)` ports silx `_alignedFullProfile` (band of `roi_width` pixels, silx start/end placement, mean÷band or sum) for H/V; `rect_profile_values(.., method)` reduces the rectangle band by mean or sum; `line_profile_band(.., linewidth, method)` + `bilinear_sample` port silx `BilinearImage.profile_line`/`c_funct` for free lines (perpendicular bilinear band of `linewidth` px, mean/sum, edge-clamp interpolation, strict band bounds). All three extractors data-layer complete + tested against silx expectations. `340b80c` wires the UI width/method selectors (ProfileWindow Width DragValue + Mean/Sum ComboBox) to the extractors (row 232). Window UI GPU-unverified |
| ✅ Done | M | M | Fit range-selection UI (xmin/xmax) | FitWidget.py:336-361 | `ac45cd8`: a "Fit range" checkbox seeds `fit_range` from the finite x-extent (`default_fit_range_of`) and exposes xmin/xmax DragValues. Deferred: interactive on-plot range-drag (panel numeric UI only). Panel GPU-unverified |
| ◐ Partial | M | M | PositionInfo snapping to nearest item | tools/PositionInfo.py:179-292 | Wave 7: `snap_to_nearest(cursor_px, candidates, threshold_px) -> Option<Snap>` ports the silx picking kernel (:236-292) — global-nearest point within `threshold²` (silx `<=`, ties to later, non-finite skipped), `SNAP_THRESHOLD_DIST = 5` logical px. Pure pixel-space math, unit-tested. The snapping-mode candidate selection is now also a pure core: `SnappingMode` (the silx `SNAPPING_*` bitfield, `|`-combinable) + `snapping_candidates(mode, &[SnapItem]) -> Vec<usize>` port `_updateStatusBar`'s item selection (:196-244) — `CURVE`/`SCATTER` engage gate, `ACTIVE_ONLY` vs all-items (with silx's real asymmetry: histograms join CURVE only in the all-items path, and active-only ignores the visible filter), and `SYMBOLS_ONLY`. 6 headless tests. Remaining ◐ (live plot/GPU state): wiring `snapping_candidates`→data→pixel projection (via `Transform`)→`snap_to_nearest` into the live `PositionInfo`, plus the red/normal label styling on the snap/no-snap result |
| ✅ Done | M | M | FitWidget UI extras (editable params/constraints/peak-search/bg) | silx.gui.fit.FitWidget | Results table renders. **Background-model selector now wired**: `fit_widget.rs` `BACKGROUND_CHOICES` combo (silx `bgtheories.THEORY` order) → `core::fitting::fit_peak_with_background` (estimate the silx background theory, subtract, fit the peak on the residual, redraw the `background + peak` total; composes rows 254/255/256). The `Background::None` path is byte-identical to the prior fit. DEVIATION from silx: silx refines the analytic-background coefficients (Constant/Linear/Polynomial) *simultaneously* with the peak in one `leastsq` (`is_background=True` theories concatenated into one parameter vector); here the background is estimated once and held fixed — a free analytic-background constant would be collinear with siplot's single-`PeakModel` baked-in trailing bg parameter (singular covariance). Removing that baked-in constant for a faithful simultaneous fit is a `PeakModel` redesign tracked separately. 6 headless tests (`None`≡plain fit byte-identical, constant/linear peak recovery, `BACKGROUND_CHOICES` silx order, `background_label` round-trip+fallback, bad input). Combo GPU-unverified. **Multi-peak UI now wired**: `FitModelChoice::MultiGaussian` (silx `sum_gauss` theory) dispatches to `core::fitting::fit_multi_gaussian_full` — auto peak search seeded by `guess_fwhm` (row 256 cores), fit all peaks at once, packaged as an `IterativeFitResult` with per-peak `Height i`/`Center i`/`FWHM i` names + covariance so the generic results table renders N×3 params/errors with no special-casing. The background combo does not apply to it (the multi-gaussian model has no per-peak constant; silx `StripBackgroundFlag` off by default). 1 headless test (names/errors/curve align + both peaks recovered); combo GPU-unverified. **Per-parameter constraints UI now wired**: `core::fitting::fit_peak_constrained` fits a single `PeakModel` under a `Constraint` vector via `leastsq_constrained` (row 255); FitWidget keeps a per-model `constraints` vector (`ensure_constraints_len` clears it to all-`Free` on model change, silx-faithful) + a collapsing per-parameter constraint editor. `perform_fit_choice`'s no-background single-peak path has a 3-way precedence: all-`Free` → original `IterativeFit` (byte-identical), else → `fit_peak_constrained`; a selected background still takes precedence and ignores constraints. The constraint combo now exposes the **full** silx per-parameter code set `FREE`/`POSITIVE`/`QUOTED`/`FIXED`/`FACTOR`/`DELTA`/`SUM` (`UI_CONSTRAINT_KINDS` = silx `Parameters.code_options` minus the group-only `ADD`/`SHOW`/`IGNORE`, which a single-model fit has no analogue for): picking `QUOTED` reveals min/max `DragValue`s, and `FACTOR`/`DELTA`/`SUM` reveal a reference-parameter picker + the multiplier/offset/sum field. The selection logic is a pure, headlessly-tested seam — `constraint_kind` (code of an existing `Constraint`), `make_constraint` (silx default-seeding: `QUOTED [0,1]` placeholder, `FACTOR` ×1 / `DELTA` +0 / `SUM` 0 to the best reference), and `default_related_reference` (silx `getRelatedCandidates`: first other parameter not itself tied/ignored; `None`→selection rejected like silx `setCodeValue`). All seven feed the existing `fit_peak_constrained`/`fit_peak_from` → `leastsq_constrained` path (row 255). 5 fit-widget headless tests (`code_options` labels, payload-drop, default-seeding, no-candidate rejection, reference-skips-tied) on top of the core constraint tests (all-free recovers, FIXED holds bit-identical, count-mismatch rejects); editor widgets GPU-unverified. **Editable initial-param input now wired**: `core::fitting::fit_peak_from` is the single owner of the constrained-fit-from-explicit-`p0` path (`fit_peak_constrained` estimates `p0` then delegates); FitWidget keeps an editable `initial_params: Option<Vec<f64>>` (populated from each fit, reset on data/model change via `ensure_constraints_len` + `set_data`) shown as an `Initial` value column in the now-3-column `Parameters` table. The no-background single-peak precedence is now `(initial_params, all_free)`: `(None, true)` → original `IterativeFit` (byte-identical), `(Some p0, _)` → `fit_peak_from` (start from edited values), `(None, false)` → `fit_peak_constrained`. 1 headless test (`fit_peak_from` recovers from an off `p0`; estimate-seeded ≡ `fit_peak_constrained`); table GPU-unverified. The prior residual (`QUOTED` min/max + `FACTOR`/`DELTA`/`SUM` editor fields) is now closed — the full constraint editor is wired (see the constraints paragraph above) |
| ✅ Done | L | S | Profile cross-profile dual-curve display | tools/profile/rois.py:663-700 | `profiles_for_roi` returns a `Vec<ProfileCurve>`; a `Roi::Cross` yields BOTH the horizontal (row-through-cy) and vertical (col-through-cx) profiles, drawn together as two legended curves ("h profile"/"v profile") in one `ProfileWindow` (silx `ProfileImageCrossROI`'s hline+vline sub-ROIs). `curve_handles: Vec` rebuilds when the curve count changes. Extraction tested; window GPU-unverified |
| ✅ Done | L | L | Profile over stack (slice across 3D dim) | tools/profile/rois.py:1058-1165 | New `high_level`: `StackProfile{frame_count,profile_len,values}` (row-major `[frame, position]`) + private `stack_profile_with` core (profiles every frame via `stack_frame`, requires a uniform per-frame `profile_len`, `None` on mismatch/empty) behind pub `stack_aligned_profile` (band profile per frame via `aligned_profile_values`; silx `ProfileImageStackHorizontalLineROI`/`...VerticalLineROI`) and `stack_line_profile` (line-segment profile per frame via `line_profile_values`; silx `ProfileImageStackLineROI`). 4 headless tests; profile-window display GPU-unverified |
| ✅ Done | L | L | Fit background subtraction (const/linear/poly/strip/snip) | silx.math.fit.bgtheories.py + filters strip.c/snip1d.c | New `core::background`: `strip_background` (Jacobi peak-stripping, anchors + border-preserve, faithful to `strip.c`), `snip_background` (descending-window min clip, `snip1d.c`; silx applies no LLS), `polyfit`/`poly_eval` (numpy.polyfit/poly1d order via normal equations). `Background` enum {None, Constant=min(y), Linear, Strip{w,niter,factor}, Snip{w}, Polynomial{deg}} mirrors silx `bgtheories.THEORY` + defaults (StripWidth 2 / Iter 5000 / factor 1.0; SnipWidth 16; Linear/Poly fit on the strip background per `EstimatePolyOnStrip`). `compute`/`subtract` return the background curve / peak residual. 13 headless tests; wired into FitWidget via `fit_peak_with_background` + the background combo (row 251) |
| ✅ Done | L | L | Fit parameter constraints (POSITIVE/FIXED/QUOTED/…) | fitmanager.py:421-430 + leastsq.py (CFREE..CIGNORED) | `core::fitting::leastsq_constrained` + `Constraint` enum {Free, Positive, Quoted{min,max}, Fixed, Factor{ref,factor}, Delta{ref,delta}, Sum{ref,sum}, Ignored} port silx's full constraint-code path: free-set classification with `derivfactor` (QUOTED `B·cos` Jacobian scaling), per-step CPOSITIVE `abs` / CQUOTED `A+B·sin(arcsin+Δ)` reparametrisation, `get_parameters` dependent-relation expansion (Factor/Delta/Sum/Ignored), `noigno` model-input reduction, and the silx two-pass covariance (FIXED/IGNORED → zero row/col + value² diagonal) + `get_sigma_parameters` uncertainties (new `LeastSqResult.uncertainties`). The shared `chisq_alpha_beta_constrained` is reused by the fit and covariance passes. Unconstrained `leastsq` left byte-identical (sibling, shares `invert_matrix`). FitWidget wiring landed via row 251 (`fit_peak_constrained` + the per-parameter constraint editor, FREE/POSITIVE/FIXED subset exposed). 9 headless tests (all-free≡unconstrained, Fixed hold + 100% uncertainty, Positive enforce/recover, Quoted clamp/recover, Factor/Delta/Sum ties, bad-spec rejects). One deliberate deviation: a single constraint-expanded base evaluation for the forward difference (silx uses raw `model(*parameters)`), equal for clean in-domain inputs |
| ✅ Done | L | L | Fit multi-peak search | silx.math.fit.peaks (`seek`/`peak_search`) + fittheories `estimate_height_position_fwhm` + funs.c `sum_gauss` | Discovery primitive (`core::peaks`): `peak_search`/`peak_search_range` port silx C `seek` (2nd-derivative-of-Gaussian smoothing, significance state machine incl. doublet split; faithful incl. the init/loop gfactor off-by-one and the `data[0]=data[1]` quirk); `guess_fwhm` ports the strip-then-half-max width (floor 4). **Simultaneous fit now wired** (`core::fitting`): `multi_gaussian_model` ports `sum_gauss` (flat `(h,c,fwhm)` triples, no bg, `dhelp≤20` guard); `estimate_multi_gaussian` ports `estimate_height_position_fwhm` (peak_search seeds + `5·xspan/n` FWHM seed, ForcePeakPresence global-max fallback, a 4-iter preliminary constrained refine with centre quoted to ±½·search-fwhm width, then default-config final constraints height/FWHM Positive + centre Free); `fit_multi_gaussian` runs the full `leastsq_constrained` over all peaks at once. 12 headless tests (7 peaks + 5 multi-fit: model=Σ-singles, 2-peak/1-peak recovery, seed/constraint shape, empty/mismatch). FitWidget multi-peak UI is row 251 |
| ✅ Done | L | L | Fit non-peak theories (step up/down, slit, arctan) | fittheories.py THEORY + funs.c (`sum_stepdown`/`sum_stepup`/`sum_slit`), functions.pyx `atan_stepup` | `core::fitting` gains `stepdown_model`/`stepup_model`/`slit_model`/`atan_stepup_model` (erfc/erf edges byte-faithful to `funs.c`; `erf` via Abramowitz-Stegun 7.1.26, ≤1.5e-7, exact at 0) + estimators `estimate_step{up,down}`/`estimate_slit`/`estimate_atan_stepup` (silx edge-kernel convolution; slit centre from half-max crossings on a `min(y)` baseline). `PeakModel::{StepDown,StepUp,Slit,AtanStepUp}` flow through `IterativeFit`; `FitModelChoice` gains 4 iterative choices. silx has no exp/poly *fit* theory (poly is a background — row 254). 12 headless tests; FitWidget UI GPU-unverified |
| ✅ Done | L | L | Standalone RadarView overview widget | tools/RadarView.py:139-300 | `RadarView` is a public, exported, standalone-usable widget (`RadarView::new`/`ui`, geometry unit-tested) AND embedded in `ImageView` with the full silx binding (data-extent + viewport sync, drag → `set_limits`). The prior "no standalone widget" note was backwards: the widget exists; what had been deferred (the live-plot binding) has since landed. Paint GPU-unverified |
| ☐ Missing | L | L | Print preview dialog | PrintPreviewToolButton.py | No print-preview page with movable/resizable rect |
| ✅ Done | L | L | ItemsSelectionDialog reuse | ItemsSelectionDialog.py; actions/fit.py:237-241 | Dialog gained the silx fit-tool capabilities it lacked: `set_available_kinds` (silx `setAvailableKinds`, offers only the requested kinds) + `SelectionMode::Single` (silx `setItemsSelectionMode(SingleSelection)`, ≤1 selected enforced by one owner). `examples/high_level_fit_widget.rs` now reuses it as a single-select curve+histogram picker that feeds `FitWidget::set_data` (silx `_initFit`→`_setFittedItem`). Capabilities/invariant unit-tested; example wiring GPU-unverified |

### Top candidate next waves (re-baselined 2026-06-04)

1. ~~**ROI on-canvas styling + selection feedback**~~ — **DONE (Wave 13, main, not
   pushed).** Closed the top H cluster via the structural fix: `plot.rois` is now
   `Vec<ManagedRoi>` (geometry + appearance) = single source of truth, the one live
   `chrome::draw_rois` path resolves each ROI's color/name/selection/width/style/fill,
   and `RoiManagerWidget` was reworked to a stateless editor over that one collection
   (its second `Vec<ManagedRoi>` removed). Closed rows: per-instance color, name label,
   selection highlight, `sigCurrentRoiChanged`, line style, line width, manager
   default-color, fill toggle. *Still open (separate rows, not this wave):* ROI
   right-click context menu. (Handle-symbol customization, edge-position
   constraints, and EllipseROI orientation/rotation are since done.)
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
- **Documented deviations (silx-cited, not guessed):** `Roi::Ellipse` now models `EllipseROI._orientation` (an
  `orientation` field; rotation geometry complete); X/Y bands use `Roi::VRange`/`HRange` (silx shows single-line `VerticalLineROI`/
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


## PlotWidget core, axes, frame, ticks  — 9✅ 0◐ 0☐

siplot has strong coverage of core axis features: linear/log/inverted axes, dual Y2 axis, axis labels with active-curve-label fallback (`resolved_axis_label`), grid modes (major/major+minor/none) with an independent grid color, nice-number tick layout with minor ticks, TIME_SERIES/datetime axes (DST-aware named IANA zones via `TimeZone`), per-axis autoscale (`set_x/y/y2_autoscale`, honored by `reset_zoom_to_data_range`), data-margin expansion ratios for reset-zoom (`DataMargins`, log-aware), axis-visibility toggle (`set_axes_displayed`), axis constraints, keep-aspect-ratio, and auto-zoom-to-data. The silx dirty/replot lifecycle is resolved-not-a-gap (siplot is immediate-mode with persistent GPU buffers; `DirtyState` is a faithful port, set not consumed). `Plot::data_range()` mirrors silx `getDataRange` and is kept live by `recompute_data_bounds` (every content change pushes the raw per-axis bounds into the cache), so this section is fully covered.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | M | Per-axis autoscale control (Axis.setAutoScale / isAutoScale) | `PlotWidget.py:2935-2957, items/axis.py:310-324` | `core/plot.rs` per-axis flags `x_autoscale`/`y_autoscale`/`y2_autoscale` (silx `Axis.setAutoScale`/`isAutoScale`) with matching getters/setters (`:596-625`); `reset_zoom_to_data_range` (`:842-875`) refits only axes whose flag is on, and forces autoscale on a log axis whose lower limit is ≤ 0 (silx). Tested `:1149`/`:1165`. |
| ✅ | H | S | Data range computation (getDataRange for bounds auto-expand) | `PlotWidget.py:908-918, resetZoom:3331-3350` | `core/plot.rs` `DataRange` (silx `_PlotDataRange`, each member `(min,max)` or `None`) + `data_range()`/`set_data_range()` cache (silx `getDataRange`); `reset_zoom_to_data_range` consumes a `DataRange`. The cache is kept live in `recompute_data_bounds` — the single content-change funnel (silx `_notifyContentChanged`→`_updateDataRange`): every add/update/remove pushes the raw per-axis min/max via `set_data_range(raw_data_range_from_bounds(..))`, so `Plot::data_range()` reflects the data on all paths instead of reading all-`None`. A single point reads as `(v, v)` un-padded, matching silx getDataRange; the non-degenerate span + data margins stay a refit-time concern in `data_range_from_bounds`. Tested: `raw_data_range_from_bounds_keeps_raw_bounds_unpadded`, `recompute_data_bounds_populates_live_data_range_cache`. |
| ✅ | M | L | TIME_SERIES / datetime axis tick mode (TickMode enum) | `items/axis.py:43-48, 296-308, _utils/dtime_ticklayout.py (entire file)` | `core/dtime_ticks.rs` ports the layout kernel: `DtUnit` (silx `DtUnit`), `best_unit` (`bestUnit`), `calc_ticks_tz`/`format_ticks_tz` (`calcTicks`/`formatDatetimes`), `DateTime` + `TimeZone` (Utc/FixedOffset). `core/plot.rs` `enum TickMode { Numeric, TimeSeries }` + `set_x_tick_mode`/`x_tick_mode` (silx `getXAxis().setTickMode`, X-only) + `set_time_zone`/`time_zone` (`setTimeZone`). Wired live: `plot_widget.rs:338 draw_axes_with_x_tick_mode` → `chrome.rs:283` formats datetime labels when `TimeSeries` on a linear axis (ignored on log, per silx). Tested (`chrome.rs:1552`, `plot.rs:1413`). |
| ✅ | L | M | Data margin ratios for resetZoom (setDataMargins / getDataMargins) | `PlotWidget.py:3251-3270, resetZoom:3352-3397 margin expansion logic` | `core/plot.rs` `DataMargins { x_min,x_max,y_min,y_max }` per-side ratios (silx `setDataMargins`) + `data_margins`/`set_data_margins` (`:653-661`); `DataMargins::expand_axis` expands in log space and skips non-positive log bounds (silx `addMarginsToLimits`); `reset_zoom_to_data_range` applies them only to refit axes (`:854-875`). Tested `:1452-1524` (linear/log/log-skip/refit-only). |
| ✅ N/A | L | M | Dirty flag and replot lifecycle (_setDirtyPlot, replot, autoreplot) | `PlotWidget.py:719-730, 3309-3311, 3279-3289` | Resolved W15 — not a gap. silx's `_dirty`/`replot`/`autoreplot` are a retained-backend deferral mechanism; siplot is immediate-mode (egui owns the frame loop) with persistent GPU buffers, so redraw is always-on and uploads are event-driven. `DirtyState` exists as a faithful port (set, not consumed). Not a correctness issue. |
| ✅ | L | M | Axis label fallback to active curve label | `items/axis.py:187-218 (setLabel, _setCurrentLabel; PlotWidget handles active curve label swapping)` | `core/plot.rs` `resolved_axis_label(default, active)` (`:190`) + `x_axis_label`/`y_axis_label`/`y2_axis_label` (`:764-777`): the active curve's per-axis label wins over the default, falling back when empty. Wired live — `high_level.rs` `active_axis_label_overrides`/`sync_active_axis_labels` (`:2170`/`:3174`) route the active curve's labels onto `Plot::active_*_label`, and `plot_widget.rs:405` renders `displayed_x_label()`. Tested `:1263-1293`. |
| ✅ | L | S | Axis visibility toggle (setAxesDisplayed / isAxesDisplayed) | `PlotWidget.py:2838-2855` | `core/plot.rs` `axes_displayed` flag + `set_axes_displayed` (marks the plot dirty; silx `setAxesDisplayed`/`isAxesDisplayed`, `:666-676`). Wired: `plot_widget.rs:277` reads it, `:286` sets `axes_hidden`, and `:382`/`:404` skip frame/ticks/labels when hidden (chrome `:111`). Tested `:1336`/`:1385`. |
| ✅ N/A | L | S | Overlay-only replot optimization | `PlotWidget.py:719-730 (dirty='overlay')` | Resolved W15 — not a gap. siplot is immediate-mode with persistent GPU buffers (data uploaded on add/update, not per frame); there is no per-frame full upload for dirty='overlay' to skip. Parity achieved differently. |
| ✅ | L | S | Separate foreground (axes/frame) and grid colors | `PlotWidget.py:setForegroundColor, setGridColor (separate calls); backends/_PlotFrameCore.py splits axis vs grid stroke` | `core/plot.rs` `foreground` and `grid_color` are independent `Option<Color32>` (silx `setForegroundColor`/`setGridColor`, `:381-384`/`:805-816`); chrome `Theme::with_overrides(fg, grid)` (`chrome.rs:52`) takes both and defaults the grid to a faint tint of the foreground when unset; `plot_widget.rs:331` passes `plot.grid_color` through, so axis/frame stroke and grid lines render with independent colors. |

## Interaction, events, panzoom, limits history  — 29✅ 0◐ 0☐

siplot implements the full pan/zoom interaction surface: pan and wheel zoom (both log-axis-aware — performed in log10 space with float32-safe clamps), box zoom, interaction-mode switching (select/pan/zoom), double-click reset, crosshair cursor, arrow-key panning, and an unbounded limits-history (zoom-back) stack. All silx draw modes are present — Rectangle/Ellipse/Line/HLine/VLine/Polygon/FreeHand/Point via `DrawMode`/`DrawParams` — with live previews. The signal surface is modeled as the `PlotEvent` enum (plus `PlotPointerEvent` for raw pointer events): mouseClicked/mouseDoubleClicked, hover (`ItemHovered` with kind/label/data+pixel position/draggable), drawingProgress/drawingFinished, limitsChanged (x/y/y2 tuples), curveClicked/imageClicked/itemClicked (markerClicked folds into the generic item-pick), and the full marker drag lifecycle. silx's `selectable` hover flag is the one documented omission (constant-true in siplot's active-on-click model). The silx interaction state machine (`StateMachine`/`ClickOrDrag`) is achieved differently via imperative `apply_interaction` dispatch + `DrawState` — resolved-not-a-gap.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | M | Pan respects logarithmic axes with proper math | `PlotInteraction.py:233-290` | `interaction.rs` `pan_axis` (`:42`) shifts a `Scale::Log10` axis in log10 space (`d_log = delta_px·(log_max−log_min)/extent_px`, silx `dx = log10(xData) − log10(lastX)`) and linearly otherwise; `pan` (`:79`) selects per-axis `x_scale`/`y_scale`. Non-positive log bounds fall back to a no-op. |
| ✅ | H | M | Wheel zoom respects logarithmic axes | `panzoom.py:80-119` | `interaction.rs` `scale1d_range` (`:100`) zooms a log axis in log10 space and clips to the positive float32 range (`FLOAT32_MINPOS`..`FLOAT32_SAFE_MAX`; silx substitutes `FLOAT32_MINPOS` for a non-positive bound); `zoom_about` (`:148`) passes `x_scale == Scale::Log10` per axis (silx `scale1DRange`). |
| ✅ | M | L | Limits history (undo/redo stack for zoom history) | `LimitsHistory.py:34-82` | `core/plot.rs` `limits_history: Vec<LimitsHistoryEntry>` (left + optional y2 ranges) pushed before each zoom/box-zoom/pan; `zoom_back` pops the most recent (silx `LimitsHistory`, unbounded stack). |
| ✅ | M | L | Draw mode (polygon, rectangle, ellipse, line, freehand, etc.) | `PlotInteraction.py:1648-1683` | `interaction.rs` `enum DrawMode` (Rectangle/Ellipse/Line/HLine/VLine/Polygon/FreeHand/Point) + `enum DrawEvent { InProgress, Finished }` (silx drawingProgress/drawingFinished) via `DrawState`; wired through `show_with_draw`/`feed_draw_state`, surfaced as `PlotEvent::DrawingFinished`. |
| ✅ | M | L | Polygon drawing interaction (select mode) | `PlotInteraction.py:485-621` | `interaction.rs` polygon mode: point-by-point vertices, snaps the last vertex to the first within `close_threshold_px` (silx onMove); close target rendered by `draw_polygon_first_point` (silx `updateFirstPoint`). |
| ✅ | M | M | Pan via arrow keys (left/right/up/down directional input) | `PlotInteraction.py (search PlotWidget for arrow handling)` | `interaction.rs` `apply_pan` + `enum PanDirection` (silx `applyPan`) for arrow-key panning. |
| ✅ | M | M | Zoom enforces float32 safety limits during zoom | `PlotInteraction.py:241-250, panzoom.py:44-47` | `interaction.rs` `FLOAT32_SAFE_MIN/MAX` (= silx ±1e37) + `FLOAT32_MINPOS`; zoom clamps linear and log limits into the safe range (`:53-135`) so span subtractions cannot overflow f32. |
| ✅ | M | M | Signal: mouseClicked with button, position (data and pixel coords) | `PlotInteraction.py:168-199, PlotEvents.py:58-70` | `interaction.rs` `PlotPointerEvent::Clicked { button: MouseButton, data:(f64,f64), pixel:(f32,f32) }` (+ DoubleClicked/Moved); emitted by `detect_pointer_event`, surfaced as `PlotResponse.pointer_event`. |
| ✅ | M | M | Selection area visualization (semi-transparent overlay during zoom/draw) | `PlotInteraction.py:98-141 (setSelectionArea), 421-430 (draw area), 526-528, etc.` | `interaction.rs` `enum FillMode { Hatch, Solid, None }` + `SelectionStyle` + `hatch_lines`; `draw_selection_polygon` (solid = color@half-alpha, hatch via clipped diagonals, dashed outline) is shared by box-zoom AND every draw-mode preview (`draw_overlay`/`paint_draw_preview`). |
| ✅ | M | M | Rectangle drawing interaction (select mode) | `PlotInteraction.py:767-807` | `interaction.rs` Rectangle two-point phase → `DrawParams::Rectangle { x, y, width, height }` (normalized corners) with a 4-corner preview ring. |
| ✅ | M | M | Floating-point overflow protection during pan/zoom | `PlotInteraction.py:233-289, panzoom.py:114-118` | Same `FLOAT32_SAFE_MIN/MAX` clamps apply on the pan/zoom paths in `interaction.rs` (`:53-135`), clipping results into the f32-safe range rather than only checking `is_valid()`. |
| ✅ | L | L | Signal: drawingProgress (real-time feedback during shape drawing) | `PlotInteraction.py:529-532, 789-792, 903-906, etc., PlotEvents.py:34-55` | `PlotEvent::DrawingProgress { mode, points }` (silx `drawingProgress`) carries the current rubber-band's data-space vertices; emitted each frame the preview advances (`high_level.rs:3128`) from `DrawEvent::InProgress`. |
| ✅ | L | L | Signal: drawingFinished (on shape completion) | `PlotInteraction.py:545-548, 800-803, 911-914, etc., PlotEvents.py:34-55` | `PlotEvent::DrawingFinished { mode, params: DrawParams }` (silx `drawingFinished`) carries the resolved shape parameters; emitted on draw completion (`high_level.rs:3132`) from `DrawEvent::Finished`. |
| ✅ N/A | L | L | Interaction state machine (ClickOrDrag, StateMachine) | `Interaction.py:87-198, PlotInteraction.py:153-209` | Resolved — not a gap. Implementation structure, not a user feature; siplot achieves the same interaction behavior with imperative `apply_interaction` dispatch + `DrawState`. Functional parity achieved differently. |
| ✅ | L | M | Signal: hover (mouseMoved) with item label, type, draggable/selectable flags | `PlotInteraction.py:1135-1154, PlotEvents.py:73-85` | `PlotEvent::ItemHovered { handle, kind, label, x, y, xpixel, ypixel, draggable }` (silx `hover`/`prepareHoverSignal`) emitted on bare-hover over a pickable item (`high_level.rs:3381`). silx's `selectable` flag is intentionally omitted — every pickable item activates on click, so it would be a constant `true` carrying no information. |
| ✅ | L | M | Signal: markerClicked with marker details and position | `PlotInteraction.py:1223-1241, PlotEvents.py:88-139` | A marker click is surfaced via the generic `PlotEvent::ItemClicked { handle, button }` (`high_level.rs:3343`, doc'd as covering "marker, scatter, or shape" — silx `markerClicked` + the generic item-pick path); read the kind via `item_kind` and the position via `marker_position(handle)`. |
| ✅ (W14) | L | M | Signal: markerMoving/markerMoved (marker drag feedback) | `PlotInteraction.py:1276-1299, 1350` | Now split: `MarkerMoved{handle}` each frame during the drag (silx `markerMoving`) plus a distinct on-release `MarkerDragFinished{handle}` (silx `markerMoved`), bracketed by `MarkerDragStarted{handle}`. Each event carries the handle; position is read via `PlotWidget::marker_position`. |
| ✅ (W14) | L | M | Signal: curveClicked with curve indices and position | `PlotInteraction.py:1243-1261, PlotEvents.py:159-173` | `PlotEvent::CurveClicked{handle,index,x,y,button}` carries the nearest picked vertex index + data position + button. (silx's full xdata/ydata arrays are reachable via the handle, not inlined.) |
| ✅ (W14) | L | M | Signal: imageClicked with pixel (col, row) index and position | `PlotInteraction.py:1263-1272, PlotEvents.py:142-156` | `PlotEvent::ImageClicked{handle,col,row,button}` emitted from the `pick_topmost` owner path over the existing `image_index`/`pick_image_pixel` picker. |
| ✅ | L | M | Cursor shape change (resize cursors for draggable markers/handles) | `PlotInteraction.py:1165-1184` | ROI-edge resize cursors were already set; Wave 11 adds the draggable-marker size cursor (marker_cursor: VLine→SizeHor, HLine→SizeVer, free Point→SizeAll, constrained Point→the free axis), shown on hover and during drag, taking precedence over the ROI-edge cursor. Matches silx CURSOR_SIZE_HOR/VER/ALL. |
| ✅ | L | M | Selection area color and fill mode (hatch, solid, none) | `PlotInteraction.py:98-141` | Closed W15: box-zoom + draw preview share one selection-overlay renderer honoring FillMode Hatch/Solid/None; box-zoom uses silx Zoom `fill="none"` dashed outline. |
| ✅ | L | M | Ellipse drawing interaction (select mode) | `PlotInteraction.py:681-765` | `interaction.rs` `DrawMode::Ellipse` → two-point drag resolved to `DrawParams::Ellipse { center, semi-axes }` (silx `SelectEllipse`, `:369`); handled in `feed_draw_state` (`:503`/`:550`) with a live preview, surfaced via `DrawingFinished`. |
| ✅ | L | M | Freehand/polyline drawing (select mode) | `PlotInteraction.py:955-1110` | Done: `DrawMode::FreeHand` accumulates vertices (DrawState); the mask pencil footprint preview circle is now drawn at the cursor (W15, `ImageView::draw_brush_preview` + `pencil_preview_circle`). |
| ✅ | L | M | Axis constraints (minXRange, maxXRange, minYRange, maxYRange) | `panzoom.py:222-366` | Closed W15: `AxisConstraints::apply` mirrors silx `ViewConstraints` allow_scaling=True normalization — max-range capped to the position window, and a view wider than the window snaps to it (adaptive expansion) |
| ✅ | L | S | Signal: mouseDoubleClicked with position | `PlotInteraction.py:168-177, PlotEvents.py:58-70` | `PlotPointerEvent::DoubleClicked { button, data, pixel }` (silx `prepareMouseSignal("mouseDoubleClicked")`, `interaction.rs:1617`/`:1653`); emitted from `detect_pointer_event` (`plot_widget.rs:1289`) and surfaced as `PlotResponse.pointer_event`. Tested `:2466`. |
| ✅ | L | S | Signal: limitsChanged with x/y/y2 range tuples | `PlotEvents.py:176-184` | `PlotEvent::LimitsChanged { x: (f64,f64), y: (f64,f64), y2: Option<(f64,f64)> }` (silx `limitsChanged`) carries the new left x/y ranges and the optional right (y2) range; emitted on pan/zoom/programmatic update (`high_level.rs:3277`). |
| ✅ | L | S | Line drawing interaction (select mode) | `PlotInteraction.py:809-840` | `interaction.rs` `DrawMode::Line` → `DrawParams::Line { start, end }` (silx `SelectLine`, `:375`); two-point drag handled in `feed_draw_state` (`:503`/`:550`) with a preview, surfaced via `DrawingFinished`. |
| ✅ | L | S | Horizontal line drawing (select mode) | `PlotInteraction.py:885-918` | `interaction.rs` `DrawMode::HLine` → `DrawParams::HLine { y }` (silx `SelectHLine`, `:378`); one-point capture handled in `feed_draw_state` (`:507`/`:554`), surfaced via `DrawingFinished`. |
| ✅ | L | S | Vertical line drawing (select mode) | `PlotInteraction.py:920-953` | `interaction.rs` `DrawMode::VLine` → `DrawParams::VLine { x }` (silx `SelectVLine`, `:380`); one-point capture handled in `feed_draw_state` (`:507`/`:554`), surfaced via `DrawingFinished`. |

## Items: Curve, Histogram, Scatter  — 14✅ 0◐ 0☐

siplot covers curve rendering (solid/dashed lines, single symbols, error bars, fill + baseline) and per-vertex colors, with correct line-style scaling and custom dash patterns. All 17 silx point symbols are implemented (only the `'♥'` Heart glyph is unported). Histograms render as filled step curves with left/center/right alignment tracking (`HistogramAlign`/`histogram_edges`) and filled-region picking (`pick_histogram`: bbox + searchsorted bin + baseline≤y≤value). Scatter supports all silx visualization modes — Points, Solid (Delaunay), RegularGrid, IrregularGrid, and BinnedStatistic (2D binning with mean/count/sum over a rows×cols grid) — plus per-point alpha, and mode-specific picking. Active-curve highlight styling is applied at render (Wave 10) via silx's `getCurrentStyle` per-field merge. IRREGULAR_GRID now renders as silx's vertex-indexed quadrilateral triangle mesh (`irregular_grid_triangles`), so its picking maps the cell under the cursor to its source point (`irregular_grid_pick`, silx vertex `// 4`) — the render redesign that closed rows 152/1086. No remaining ◐ in this section.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | M | Curve symbols (circle, square, cross, plus, triangle) | `core.py:722-820 (SymbolMixIn), supported list includes o,d,s,+,x,.,etc.` | `core/items.rs` `Symbol` now has all 17 silx symbols (Circle/Diamond/Square/Plus/Cross/Point/Pixel/VerticalLine/HorizontalLine + 4 Tick + 4 Caret) plus the egui-extra Triangle; `Symbol::ALL`/`name()`/`from_code` map them to silx codes; rendered in `markers.wgsl` (`code()` 0-17). Only silx's `'♥'` Heart glyph is unimplemented. |
| ✅ | M | L | Scatter visualization mode: Solid (Delaunay triangulation) | `scatter.py:283-296, core.py:1271-1275 (Visualization.SOLID)` | `core/scatter_viz.rs` `delaunay` (Bowyer-Watson) + `solid_triangles`; `ScatterView::rebuild_visualization` Solid arm → `add_triangles_data` → `chrome::draw_triangles` (egui Mesh, per-vertex Gouraud colors). Render path is egui-tessellated, not a wgsl shader; GPU-unverified on-screen. |
| ✅ | M | L | Scatter visualization mode: IrregularGrid (vertex-indexed triangle mesh) | `scatter.py:283-296, 682-797, core.py:1286-1293 (Visualization.IRREGULAR_GRID)` | `scatter_viz.rs` `irregular_grid_triangles` arranges the points onto the detected grid (`arrange_irregular_grid_points`), builds the dual cell-corner grid (`quadrilateral_grid_coords`, interior corners = mean of 4 neighbours) and emits one flat-shaded quad (2 triangles) per point (silx `_quadrilateral_grid_as_triangles`) → IrregularGrid arm of `rebuild_visualization` → `add_triangles_data` → `chrome::draw_triangles`. Replaces the prior interpolated-image render (the `irregular_grid_image` barycentric raster remains as a standalone core primitive). Headlessly tested; egui-tessellated, GPU-unverified on-screen. |
| ✅ | M | M | Curve highlight/selection state with different style | `curve.py:196,280-311 (getCurrentStyle), core.py:1875-1905 (HighlightedMixIn)` | Wave 10: the active curve renders with a distinct highlight style. `current_curve_style` is silx `getCurrentStyle`'s per-field merge (highlight field if Some, else the curve's own); default active style = line width 2 (silx `DEFAULT_PLOT_ACTIVE_CURVE_LINEWIDTH`), color unchanged. Applied as a render-time overlay (retained base stays the single source of truth), gated to `PlotItemKind::Curve`. **GPU-UNVERIFIED:** the on-screen thicker line needs a GPU `RenderState` no crate test constructs; only the pure merge is unit-tested. |
| ✅ | M | M | Histogram bin alignment (left, center, right) | `histogram.py:53-85 (_computeEdges), setData(align='center'/'left'/'right'), getAlignment` | W15: `HistogramAlign` + pure `histogram_edges(positions, align)` mirror silx `_computeEdges`; public `add_histogram_aligned`/`_with_legend` accept N positions + N counts + alignment. Headlessly tested per alignment. |
| ✅ | M | M | Scatter visualization mode: RegularGrid (image-like grid rendering) | `scatter.py:283-296, core.py:1277-1282 (Visualization.REGULAR_GRID)` | `scatter_viz.rs` `detect_regular_grid`/`guess_z_grid_shape` auto-detect the grid shape; `regular_grid_image` renders via the image pipeline (grid arm of `rebuild_visualization`). Tested. GPU-unverified on-screen. |
| ✅ | M | M | Scatter per-point alpha transparency | `scatter.py:1009,1024,1051-1060 (setData with alpha parameter, __alpha field)` | `ScatterView.alpha: Option<Vec<f64>>` + `with_alpha`/`set_alpha`/`clear_alpha`; `compose_per_point_alpha` applies `colormap.alpha × per_point × global` (clamped) in the Points and Solid arms. Grid modes intentionally ignore per-point alpha (matches silx). |
| ✅ | M | M | Scatter picking with mode-specific logic | `scatter.py:804-860 (pick with special handling per Visualization mode)` | `ScatterView::show_position_info` dispatches the live pick by visualization mode (silx `Scatter.pick` + `_pickScatterData`): REGULAR_GRID → `regular_grid_pick` (cursor cell → index by major order, no radius), BINNED_STATISTIC → `BinnedStatistic::pick` then `nearest_candidate_in_data` (nearest bin point, highest-index tie-break, silx `indices[::-1]`+argmin), POINTS/SOLID → nearest-pixel `scatter_pick_pixels`, IRREGULAR_GRID → `irregular_grid_pick` (point-in-cell over the retained quadrilateral mesh, triangle `t` → point `t/2`, silx picked-vertex `// 4`). The IRREGULAR_GRID gap is closed by the triangle-mesh render redesign (row 1081): each point now owns a vertex-indexed cell, so the silx `// 4` mapping has a direct equivalent. Closes row 152. |
| ✅ | M | M | Plot item selection/highlight state tracking | `core.py:1875-1905 (HighlightedMixIn with setHighlighted/isHighlighted)` | Wave 10: the active-item selection is now propagated to the renderer for curves. `set_active_item` reverts the previous active curve to its base style and applies the highlight overlay to the new one (silx `setHighlighted(False)`/`(True)` in `_setActiveItem`), through a single owner `sync_curve_highlight`; `set_active_curve_style`/`set_active_curve_handling` mirror silx. Image/scatter selection still has no highlight style (silx highlights only `kind=='curve'`). |
| ✅ | L | L | Histogram picking (filled region sensitivity) | `histogram.py:244-290 (pick, __pickFilledHistogram with bounds check)` | `pick_histogram(edges, values, baseline, x, y)` ports silx `__pickFilledHistogram`: data-space bounding box (`_getBounds`, y includes 0), `searchsorted(side="left")-1` bin index via `partition_point`, then the `baseline ≤ y ≤ value` fill check. Tested (locate-bin/outside-bbox/nonzero-baseline). |
| ✅ | L | L | Scatter visualization mode: BinnedStatistic (2D histogram with statistics) | `scatter.py:283-296, core.py:1295-1301 (Visualization.BINNED_STATISTIC)` | `binned_statistic(x, y, values, rows, cols)` (scatter_viz.rs) bins points into a rows×cols grid with per-bin mean/count/sum (`BinnedStatistic`); rendered through the colormapped-image pipeline and pickable via `BinnedStatistic::pick`. Tested. |
| ✅ | L | S | Scatter visualization parameter: grid_major_order (row/column) | `core.py:1303-1308, 1346 (VisualizationParameter.GRID_MAJOR_ORDER)` | `GridMajorOrder::{Row, Column}` (scatter_viz.rs) drives `regular_grid_pick` index math (`row*cols+col` vs `row+col*rows`) and `RegularGrid.order`; auto-detected by `detect_regular_grid`. Tested. |
| ✅ | L | S | Scatter visualization parameter: binned_statistic_function (mean/count/sum) | `core.py:1325-1329, 1347 (VisualizationParameter.BINNED_STATISTIC_FUNCTION)` | `BinnedStatisticFunction::{Mean, Count, Sum}` selects the per-bin reduction via `BinnedStatistic::select`. Tested. |
| ✅ | L | S | Scatter visualization parameter: binned_statistic_shape (grid dimensions) | `core.py:1325,1333 (VisualizationParameter.BINNED_STATISTIC_SHAPE)` | The grid dimensions are the `rows`/`cols` arguments to `binned_statistic` (returns `None` when either is 0); `BinnedStatistic` stores the rows×cols arrays. Tested. |

## Items: Image, Marker, Shape, Complex  — 11✅ 0◐ 1☐

siplot implements core image (scalar colormapped + RGBA direct), marker (point/vline/hline), and shape (polygon/rectangle/polyline/hline/vline) drawing. Colormaps support 8 built-in names with linear/log/sqrt/gamma normalization and a 256-entry LUT. Images support origin/scale placement, GPU tiling for large datasets, per-pixel validity masking (NaN→transparent), nearest/linear data interpolation, and max/mean/min aggregation downsampling. Markers support point symbols, draggability with horizontal/vertical/custom-callback constraints, text labels with 9-way anchor positioning, and line styling. Shapes support fill, dashed-gap outline styling, and a data-vs-overlay layer flag. Complex images render via `ComplexImageView` (all 7 silx modes), 3D stacks via `ImageStack` (in-memory frame browser), and infinite lines via the `Line` item (`y = slope·x + intercept`). Remaining gap: per-pixel image alpha (needs an alpha texture + shader sample — GPU, design sign-off). Concave-polygon fill is now closed (triangulated mesh fill).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ☐ | M | M | Image per-pixel alpha map | `items/image.py:462-500 (ImageData.getAlphaData/setAlphaData, alternative image with alpha)` | GENUINELY MISSING (GPU). siplot's image `Params` exposes only a scalar `alpha: f32` (`image.wgsl`); `gpu_image.rs` has no per-pixel alpha array, alpha texture, or shader sample. Implementation needs: a per-pixel alpha field on `ImageData`, a new alpha texture + bind-group entry + sampler (also through the GPU tiling path), and an `image.wgsl` alpha-texture multiply. GPU-visually unverifiable headlessly — needs design sign-off (analogous to the nan_color shader work). |
| ✅ | M | M | Image masking (per-pixel validity mask) | `items/image.py:209-251 (getMaskData/setMaskData), 273-284 (getValueData applies mask as NaN)` | `core/items.rs` `ScalarMask` (`get_mask_data`/`set_mask_data`/`apply` = getValueData→NaN); wired via `apply_image_mask` + `Plot2D::try_add_masked_image`. Masked pixels become `f32::NAN`, rendered transparent by the shader's non-finite → nan_color branch (`image.wgsl`, commit `0356d5c`). Tested. On-screen pixels GPU-unverified. (Same closure as row 153.) |
| ✅ | M | M | Marker draggability (isDraggable/drag callback) | `items/marker.py:52-67 (MarkerBase with DraggableMixIn), 177-206 (drag method, setPosition)` | Wave 11: a draggable marker (is_draggable) is grabbed on primary drag (topmost via marker_at), follows the cursor through Marker::drag (constraint-aware), persists to the backend item, and reports via PlotResponse.marker_moved / PlotEvent::MarkerMoved. Public set_marker_position / marker_position (silx setPosition/getPosition). No separate sigDragStarted/sigDragFinished split (single move signal). |
| ✅ | L | L | Image aggregation/downsampling modes (max/mean/min) | `items/image_aggregated.py:46-138 (ImageDataAggregated.Aggregation enum with NONE/MAX/MEAN/MIN, _getLevelOfDetails LOD reduction)` | `AggregationMode::{None, Max, Mean, Min}` (gpu_image.rs) + `aggregate_blocks(field, w, h, bx, by, mode)` reduce a field by integer block factors (silx `ImageDataAggregated` MAX/MEAN/MIN); wired as the image spec's `aggregation` + `aggregation_block` fields applied on the display path. Default None matches silx. Reducer tested (max/min/mean/none, odd dims). |
| ✅ | L | L | Complex image modes (7 variants: absolute/phase/real/imaginary/amplitude_phase/log10_amplitude_phase/square_amplitude) | `items/complex.py:105-378 (ImageComplexData class, ComplexMode enum with 7 modes, mode-specific colormaps and conversions)` | `ComplexImageView` + `ComplexMode` (complex_image_view.rs) implement all 7 silx modes (Absolute/Phase/Real/Imaginary/SquareAmplitude/AmplitudePhase/Log10AmplitudePhase, plus an extra Log10Amplitude): scalar modes feed the colormapped pipeline, the amplitude-phase composites build HSV RGBA (hue=phase, value=normalized amplitude, with a settable log10 displayed-max). Conversions tested. |
| ✅ | L | L | Shape fill concavity (concave polygon fill) | `items/shape.py (silx defers to backend; matplotlib/pygfx fill concave correctly)` | Closed: `core::shape::triangulate_simple_polygon` (ear clipping, orientation-normalized, `N-2` triangles for a simple polygon, defensive fan on degenerate/self-intersecting input) lets the chrome fill `Shape`/`Roi` polygons as a triangulated `egui::Mesh` (`chrome::fill_polygon`) instead of egui's convex-only `Shape::convex_polygon`. Concave polygons now fill to their true outline, matching silx's matplotlib/pygfx backends (which fill concave correctly). Convex shapes (rect, ellipse 27-gon, band quad, diamond marker) are unchanged. 5 headless tests (triangle, convex quad, concave-tiles-exactly-not-hull, CW-input normalization, <3-points). Mesh draw GPU-painter-unverified. |
| ✅ | L | M | Image interpolation mode (nearest vs linear sampling) | `backends/BackendMatplotlib.py:805 (interpolation='nearest' hardcoded; silx backends via matplotlib accept 'nearest'/'bilinear'/etc.)` | `InterpolationMode::{Nearest, Linear}` (gpu_image.rs) is exposed as the `ImageData.interpolation` field (`with_interpolation` setter, default Nearest = silx) and carried into `image.wgsl` via `code()` (0/1) where the shader branches between nearest and bilinear scalar sampling. Mode plumbing tested; on-screen sampling GPU-unverified. |
| ✅ | L | M | Image stack (3D array, show one frame at a time) | `items/image.py:593-669 (ImageStack class, setStackData/getStackData/setStackPosition)` | `ImageStack` (image_stack.rs) browses an in-memory ordered list of 2D `Frame`s: a current-frame index driven by a first/prev/next/last slider (silx `setStackPosition`), a selectable frame table, per-frame visibility, a waiting overlay for empty slots, and display of the current frame through the `Plot2D` backend. Lazy URL/HDF5 frame loading is a separate gap (rows 1286-1287). Navigation state tested; render GPU-unverified. |
| ✅ | L | M | Marker constraint function (horizontal/vertical/custom drag filter) | `items/marker.py:208-235 (getConstraint/_setConstraint with _horizontalConstraint/_verticalConstraint), 273-292 (Marker subclass overrides for constraint strings)` | Wave 11 wired the horizontal/vertical presets (MarkerConstraint::Horizontal/Vertical) into the drag and the size cursor (Horizontal→SizeVer, Vertical→SizeHor). The arbitrary custom-callback form is now `Marker::drag_with(to, fn)` — silx's callable `constraint(x,y)->(x,y)`; `drag` delegates to it so both paths share one update owner. Closure is call-time state (keeps the Marker derives). |
| ✅ | L | M | Shape overlay flag (data layer vs separate overlay layer) | `items/shape.py:54-73 (_OverlayItem with isOverlay/setOverlay)` | `Shape::is_overlay`/`Line::is_overlay` (`with_overlay` setter) now drive a two-pass render: `draw_shapes`/`draw_lines` take an `overlay: bool` filter; `PlotWidget` draws non-overlay items in the base data layer (above grid, under ROIs/markers/crosshair) and overlay items in the overlay layer on top of the chrome (silx base-render vs `_drawOverlays`). Field/setter tested; render-layer split GPU/painter-unverified. |
| ✅ | L | M | Line item (infinite y = slope·x + intercept) | `items/shape.py:289-393 (Line class, distinct from Shape, infinite line with slope/intercept, computed vertices via visible bounds tracking)` | `Line` (core/shape.rs) is a distinct infinite-line item: `y = slope·x + intercept` (non-finite slope → vertical `x = intercept`), constructible from slope/intercept or two points (silx `setSlopeInterceptFromPoints`), clipped to the visible `[xmin, xmax]` bounds at draw time. Tested. |
| ✅ | L | S | Marker text anchor/alignment (positioning relative to marker point) | `backends/BackendMatplotlib.py:951-1047 addMarker + _TextWithOffset.__get_xy:304-340` | `TextAnchor` (9 variants) wired into `chrome::draw_markers` via pure `rect_offset` + `pixel_offset` (the latter ports silx's alignment-dependent offset sign). Per-kind silx defaults: point/vline `TopLeft` (`ha="left"`), hline `TopRight` (`ha="right",va="top"`, text at the right edge). Core tested headlessly; painter draw GPU-unverified |

## Backend Render (WgpuBackend vs BackendPygfx)  — 36✅ 2◐ 2☐

The WgpuBackend implements the core Backend trait with substantial coverage of BackendPygfx: additive item methods (addCurve, addImage, addTriangles, addShape, addMarker with the full silx symbol set + drag constraints), state management (limits, axes, colors, margins), and core rendering (transforms, data-to-pixel, picking). `save_graph` exports PNG/PPM/TIFF/SVG (DPI accepted-but-ignored, matching silx). Remaining true gaps: GPU line joins/caps (round/bevel/miter — `design.md` §7·§13 B1, butt-only today) and async GPU stats/histogram reduction (functional autoscale/stats/histogram exist on CPU; the GPU-compute performance path is not mirrored). Several silx backend methods — setGraphGrid, setGraphCursor, time-series axis, postRedisplay — live at siplot's `Plot`/chrome/widget layer rather than the GPU backend: an architectural split, not missing features (marked ✅ N/A below).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ◐ | H | L | addMarker: point/hline/vline marker with symbol, text, line constraints | `BackendBase.py:211-267, BackendPygfx.py:1397-1465` | Markers (point/hline/vline) with symbol + text + drag constraints are implemented: `MarkerConstraint` (None/Horizontal/Vertical) + `apply_constraint` replace silx's Qt drag-filter callback (data-driven). Label background rect (silx `bgcolor`/`setBackgroundColor`) IS wired: `Marker::bgcolor` + `with_bgcolor`, rendered by `chrome::draw_marker_label` as a `rect_filled` behind the galley (`chrome.rs:1123-1129`), live via `draw_markers` (`plot_widget.rs:453`). Residual: custom text font (silx `setFont`/QFont) is not ported — marker labels render in a fixed 11 pt proportional font (egui `FontId` cannot express QFont weight/italic without registered font variants). |
| ✅ | H | M | Symbol support: circle, square, cross, plus, diamond, etc. | `BackendBase.py:116-126, BackendPygfx.py:85-98 (_SYMBOL_MAP)` | `MarkerSymbol` (Circle/Point/Pixel/Plus/Cross/Diamond/Square) and `items::Symbol` (adds Triangle/TickLeft/TickRight) cover the full silx `_SYMBOL_MAP` set ('o'/'.'/','/'+'/'x'/'d'/'s'). |
| ✅ | M | L | saveGraph: PNG export at specified DPI and size | `BackendBase.py:372-382, BackendPygfx.py:2588-2610` | `save_graph`/`save_graph_with_format` + `SaveFormat` export PNG, PPM, TIFF, and SVG (`encode_png`/`encode_ppm`/`encode_tiff`/`encode_svg`). The DPI parameter is accepted-but-ignored, matching silx (noted there too). |
| ☐ | L | L | Line joins: round/bevel/miter for connected segments | `BackendPygfx.py uses pygfx LineMaterial (no explicit join config visible)` | Deferred design-doc item (§7·§13 B1). Butt caps + gaps visible at sharp turns; high-res curves hide this. |
| ☐ | L | L | Line caps: butt, round, square at line endpoints | `BackendPygfx.py uses pygfx LineMaterial` | Deferred (§7·§13 B1). Only butt caps implemented; round/square caps listed as future work. |
| ✅ N/A | L | L | Grid on/off: toggle major/both grid display | `BackendBase.py:543-549, BackendPygfx.py:2754-2756` | Resolved differently — grid display is `Plot::grid` (`GraphGrid::None`/`Major`/`MajorAndMinor`), drawn by the chrome layer. silx exposes `setGraphGrid` on the backend; siplot keeps it on `Plot`/chrome (the GPU backend draws data, not the grid) — an architectural split, not a missing feature. |
| ✅ N/A | L | M | Crosshair cursor (setGraphCursor): show/hide crosshair, set color/width/style | `BackendBase.py:289-310, BackendPygfx.py:1948-1990 (_updateCrosshair)` | Resolved at the widget layer — `Plot::crosshair`/`set_graph_cursor` toggle + `chrome::draw_crosshair` (`plot_widget.rs:457`) draw a crosshair + coordinate readout following the pointer (appearance via the chrome `Style`). A backend-trait method is, by design, not where siplot puts this. |
| ✅ N/A | L | M | Time-series axis: display datetime objects on X axis | `BackendBase.py:468-489, BackendPygfx.py:2704-2714` | Implemented at the chrome layer (`TickMode::TimeSeries` + `core/dtime_ticks`, wired in `chrome.rs`; see the PlotWidget-core TIME_SERIES row), not the GPU backend — the chrome-layer split the gap note itself describes ("chrome-layer responsibility in both silx and siplot"). |
| ◐ | L | M | GPU stats/histogram: async GPU compute for min/max/histogram (streaming data) | `BackendPygfx.py:1579-1622 (_WgpuComputeHelper, _AsyncCompute, _computeGpuDataStats, _computeGpuHistogram)` | The functional results exist on CPU — colormap autoscale (`AutoscaleMode::range`: MinMax/Stddev3/Percentile), curve/image stats (`core/stats`), and histograms (`pick_histogram`, `scatter_viz::binned_statistic`). silx's async GPU reduction (atomic + readback) for streaming-data performance is not mirrored. |
| ✅ N/A | L | S | postRedisplay / request_draw: request a repaint on next frame | `BackendBase.py:363-365 (postRedisplay calls replot)` | Resolved-not-a-gap — egui is immediate-mode and redraws every frame, so `postRedisplay`/`replot` has no consumer by design (same basis as the dirty/replot lifecycle rows). |

## Colormap, ColorBar, and AlphaSlider  — 14✅ 1◐ 0☐

siplot implements colormaps with a 45-entry catalog covering silx's full named set (gray, reversed gray via `reversed()`, red, green, blue, temperature, jet, hsv, the viridis family, plus 30+ colorous gradients standing in for matplotlib's catalog), all 5 silx normalizations (Linear/Log/Sqrt/Gamma/Arcsinh), NaN-color configuration, and the three silx autoscale modes (MinMax/Stddev3/Percentile, with configurable percentile bounds via `AutoscaleMode::range`). The `ColorBarWidget` is a self-contained drawer with min/max labels, log/linear ticks, a rotated legend, and both vertical/horizontal orientations; the `ColormapDialog` adds name/normalization/gamma/autoscale/range controls plus a data-distribution histogram; an `AlphaSlider` widget exposes 0-255 opacity. `Colormap` supports custom Nx4 LUTs (`from_colors`/`set_lut`), an editable flag, copy/equality, and `set_from`. Remaining ◐: the active-item alpha-binding slider subclasses.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | L | Colormap catalog names | `/Users/stevek/codes/silx/src/silx/gui/colors.py:444-455, /Users/stevek/codes/silx/src/silx/math/colormap.py:53-65` | `ColormapName` is a 45-entry static catalog (`ALL`) covering silx's entire documented named set (`setName`: "at least" `gray`, `reversed gray`, `temperature`, `red`, `green`, `blue`, `jet`, `viridis`, `magma`, `inferno`, `plasma`). `Gray`/`Red`/`Green`/`Blue` are exact `single_channel_ramp`s, `Temperature` is transcribed channel-by-channel from `_create_colormap_lut`'s `arange` slices (`temperature_lut`, tested), `reversed gray` = `Colormap::reversed()` (LUT reverse), and `Jet`/`Hsv` + 30+ colorous gradients (Blues/Greens/Warm/Cool/Cubehelix/diverging/cyclical) stand in for matplotlib's catalog. Residual (not a gap in silx's contract): siplot ships a fixed static catalog rather than resolving arbitrary matplotlib names at runtime — it carries no matplotlib dependency (analogous to silx loading the viridis family from bundled `.npy` resources). |
| ✅ | H | M | Autoscale mode selection | `/Users/stevek/codes/silx/src/silx/gui/colors.py:318-331, 563-586` | `AutoscaleMode::{MinMax, Stddev3, Percentile}` (colormap.rs) with `AutoscaleMode::range(data, percentiles)` computing each mode faithfully — MinMax, mean±3·std clamped to data range (silx STDDEV3), and `nanpercentile` (silx PERCENTILE) — plus silx's empty/non-finite/inverted fallbacks. ColormapDialog exposes the mode selector. Tested. |
| ✅ | H | M | Autoscale percentile bounds | `/Users/stevek/codes/silx/src/silx/gui/colors.py:588-599, /Users/stevek/codes/silx/src/silx/math/colormap.py:355-368` | `Colormap.autoscale_percentiles: (f64, f64)` (default `DEFAULT_PERCENTILES = (1.0, 99.0)`) + `set_autoscale_percentiles` (silx `setAutoscalePercentiles`, each clamped to `[0,100]`, ordered low≤high); consumed by `AutoscaleMode::Percentile`. ColormapDialog has the low/high inputs. Tested. |
| ✅ | M | L | ColormapDialog interface | `/Users/stevek/codes/silx/src/silx/gui/dialog/ColormapDialog.py:24-300` | `widget/colormap_dialog.rs`: name (45 colormaps), normalization, vmin/vmax, gamma, AND the two cited gaps now closed — data-distribution histogram (`set_histogram`/`get_histogram` + auto-compute from the active image, per-norm cache; silx `setHistogram`/`_computeNormalizedHistogram`) drawn behind the range, plus autoscale-mode selector (`AutoscaleMode::{MinMax, Stddev3, Percentile}` = silx's full set, with `(low,high)` percentile inputs; silx `setAutoscaleMode`). |
| ✅ Done | M | M | NaN color configuration | `/Users/stevek/codes/silx/src/silx/gui/colors.py:506-518, 337` | `0356d5c`: `Colormap.nan_color` ([u8;4], default fully-transparent white #FFFFFF00) now reaches the GPU — `image.wgsl` detects non-finite samples and returns the sRGB→linear `nan_color` premultiplied by `alpha`, so NaN/masked pixels render transparent (also fixes ScalarMask, which sets masked pixels to f32::NAN). std140 size (128 B) + sRGB transfer unit-tested; GPU-visually unverified |
| ✅ | M | M | Colorbar drawing with ticks | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:296-920` | `widget/colorbar.rs` `ColorbarWidget`: 256-control-point gradient strip (silx `_NB_CONTROL_POINTS`), `paint_ticks_and_labels` draws major nice/decade ticks **with labels** plus minor sub-ticks (`tick_layout`/`optimal_nb_ticks` at `DEFAULT_TICK_DENSITY`), respecting the colormap normalization, in both vertical and horizontal orientations. |
| ✅ | M | M | Colorbar min/max labels | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:364-452` | `widget/colorbar.rs` `paint_end_labels` (silx `ColorScaleBar._updateMinMax`): formatted `vmax` at the top / `vmin` at the bottom (vertical) or left/right (horizontal) via `format_end_label`. The Qt hover-tooltip-with-full-precision is a Qt-widget affordance not applicable to a painted egui widget; the formatted end labels themselves are present. |
| ✅ | M | S | Normalization types | `/Users/stevek/codes/silx/src/silx/gui/colors.py:292-316` | All 5 present: `Normalization::{Linear, Log, Sqrt, Gamma, Arcsinh}` (`core/colormap.rs:19-35`). Arcsinh (silx `ARCSINH`) transforms `v.asinh()` on the CPU path (`:63`, `code()==4`), in `image.wgsl` (`norm==4u`, `asinh(raw)`), and in the colorbar tick layout (`colorbar.rs:857`); tested (`colormap.rs:1144-1169`). |
| ✅ | L | L | ColorbarWidget standalone | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:44-263` | `ColorBarWidget` (colorbar.rs) is a self-contained, exported widget: `new(colormap, vmin, vmax)` + `with_legend`/`with_orientation` builders, drawing the gradient strip, ticks/labels, end labels, and legend into any allotted rect. Caller syncs it to the active item's colormap (the silx signal-sync is the host's job). |
| ✅ | L | M | Colorbar legend label | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py:44-200, 266-294` | `ColorBarWidget.legend` + `with_legend` + `paint_legend` draw a rotated title running along the bar (silx `_VerticalLegend`, 270°); empty legend hides the strip (`split_legend`). |
| ✅ | L | M | Colorbar orientation (vertical/horizontal) | `/Users/stevek/codes/silx/src/silx/gui/plot/ColorBar.py (vertical only in examples but widget is orientation-agnostic)` | `ColorBarOrientation::{Vertical, Horizontal}` is a `ColorBarWidget` field (`with_orientation` builder, default Vertical); the bar, ticks, end labels, and legend strip all branch on it (silx Qt::Vertical/Qt::Horizontal). |
| ◐ | L | M | AlphaSlider widget | `/Users/stevek/codes/silx/src/silx/gui/plot/AlphaSlider.py:86-250` | The base widget is done: `AlphaSlider` (alpha_slider.rs) is a 0..=255 opacity slider exposing `u8`/`f32` alpha (`value/255`, silx `valueChanged`) with `AlphaSliderOrientation` and a `ui() -> Response`. Remaining ◐: the item-binding subclasses (`ActiveImageAlphaSlider`/`NamedItemAlphaSlider`) that track a specific plot item's alpha are deferred (need the plot model). |
| ✅ | L | M | Custom colormap registration | `/Users/stevek/codes/silx/src/silx/math/colormap.py:158-176, /Users/stevek/codes/silx/src/silx/gui/colors.py:343-386` | Per-instance custom LUTs (`Colormap::from_colors(&[[u8;4]], vmin, vmax)`, `with_lut`/`set_lut`, silx `setColormapLUT`) AND a process-global named-LUT registry: `register_colormap(name, &[[u8;4]], cursor_color: Option)` (silx `registerLUT`/`register_colormap` — resamples via `resample_lut`, overriding allowed, empty colors → `false`), `Colormap::from_registered(name, vmin, vmax)` resolves a registered name to its LUT (silx `Colormap(name=…)`), `registered_colormaps()` (silx `get_registered_colormaps`), `registered_colormap_cursor_color()` (silx `get_colormap_cursor_color`). Backed by a `OnceLock<RwLock<BTreeMap>>`, the siplot analogue of silx's module-level `_AVAILABLE_LUTS`/`_COLORMAP_CACHE`. 5 headless tests. |
| ✅ | L | S | Colormap copy/comparison/serialization | `/Users/stevek/codes/silx/src/silx/gui/colors.py:399-423, 960-1050` | `Colormap` derives Clone (copy) + PartialEq (`__eq__`) and has `set_from` (silx `setFromColormap`, editable-guarded). State round-trip now ported in `core::colormap_io` (silx `saveState`/`restoreState`): `encode_colormap`/`decode_colormap` (hand-written line-oriented text, no serde — same approach as `roi_io` and the `.npy` mask path) + `save_colormap`/`load_colormap` file wrappers. Because siplot's `Colormap` stores a resolved LUT and keeps no name (unlike silx, which serializes the name + registry), the state serializes the LUT itself → lossless `decode(encode(c)) == c` for catalog, `from_colors`, and registered LUTs alike; `lut` is the only required line, every other field defaults when absent (silx version-gated `restoreState` defaults), unknown keys ignored. 8 headless tests. |
| ✅ | L | S | Colormap editability flag | `/Users/stevek/codes/silx/src/silx/gui/colors.py:351, 659-674` | `Colormap.editable: bool` (default true) with `is_editable`/`set_editable` (silx `setEditable`); the editable-guarded setters (`set_lut`, `set_autoscale_percentiles`, `set_from`) become no-ops returning `false` when frozen (silx `NotEditableError`). |

## ROI system (creation, editing, manager, statistics)  — 25✅ 5◐ 0☐

siplot implements a ROI core with all 11 silx shape kinds (Rect, HRange, VRange, Point, Line, Polygon, Cross, Circle, Ellipse [oriented], Arc, Band), supporting on-plot interactive creation for every kind (Wave 12: arm a draw shape, draw it, the ROI is appended), edge dragging via handle-detection, and whole-ROI translate — all in pure data space. Wave 13 unified `plot.rois` to a single `Vec<ManagedRoi>` (geometry + appearance), closing the visual/behavioral gap: per-ROI color, on-canvas naming/labels, selection highlighting (single-owner `Plot::set_current_roi` + `sigCurrentRoiChanged`), handle-symbol roles, and style properties (line width, style, gap color, fill) all reach the live plot; `Roi::contains()` is implemented for every variant; and ROI statistics (curve raw/net counts + area, image/curve stats tables via CurvesROIWidget/ROIStatsWidget) landed in W15. Remaining gaps are narrower: the per-shape interaction sub-modes (Arc PolarMode↔ThreePointMode, Band Bounded↔Unbounded live toggle) and a richer manager table widget. ROI persistence/load is complete: `core::roi_io` text serialization + `PlotWidget::save_rois_to_path`/`load_rois_from_path` + native `save_rois_dialog`/`load_rois_dialog`.

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | L | Interactive ROI creation mode (draw mode vs select mode) | `tools/roi.py:833-885 (start/stop/isStarted/isDrawing)` | Wave 12: `PlotInteractionMode::RoiCreate(RoiDrawKind)` arms a draw shape (silx `start(roiClass)`); the existing `DrawState` machine drives the gesture, `roi_from_draw` maps the finished `DrawParams`→`Roi` (silx `setFirstShapePoints`), the ROI is appended, the mode re-arms continuously (silx default), and a live preview overlay is surfaced during the gesture. Public `set_roi_create_mode(kind)` toggles draw vs select. Still missing the toolbar `CreateRoiModeAction`/mode-selection widget. On-screen creation is GPU/PlotWidget-UNVERIFIED; the `DrawParams`→`Roi` mapping and the `apply_interaction` wiring are headlessly unit-tested. |
| ◐ | H | L | Manager ROI list as table widget (ROITable/CurvesROIWidget) | `CurvesROIWidget.py:62-400, ROITable:452-860` | egui shows a scrollable list with remove button per ROI and add buttons (Rect/HRange/VRange/Point/Line only). silx ROITable is a rich table with per-ROI stats columns (min/max/sum/mean/etc.), editable |
| ✅ | H | L | ROI statistics (min/max/mean/sum/integral, raw/net counts, COM/coords) | `CurvesROIWidget.py:355-430, ROIStatsWidget.py (full file)` | `roi_stats` computes count/min/max/mean/sum/integral per ROI (image + curve) plus center-of-mass and first-min/first-max **data coordinates** (silx `StatCOM`/`StatCoordMin`/`StatCoordMax`): value-weighted COM, first-extremum tie-break matching numpy argmin/argmax, 2D `(x, y)` for image ROIs / x-only for curve ROIs. `curve_roi_counts` adds curve raw/net counts + raw/net area (silx `computeRawAndNetCounts/Area`); `RoiStatsWidget` renders all columns incl. COM / coord min / coord max. "Peaks" was a misattribution — not a silx ROI stat (peak-search lives in the fit cluster, `peak_search`); neither `CurvesROIWidget` nor the `stats` framework (`StatMin/Max/Delta/CoordMin/CoordMax/COM`) computes peaks. Reductions are pure and headlessly tested (image value-weighted COM = (13/6, 8/3) on the 4×4 ramp with coord min/max; curve x-only COM + coords); the widget table is GPU-unverified. |
| ✅ | H | L | CurvesROIWidget integration (ROI stats per curve item) | `CurvesROIWidget.py (entire file)` | W15: `CurvesRoiWidget` shows a per-ROI raw/net counts + raw/net area table over the active curve via `PlotWidget::feed_curves_roi_stats`/`show_curves_roi_widget`, reducing each x-span ROI with the pure `curve_roi_counts` (silx `computeRawAndNetCounts`/`computeRawAndNetArea`). |
| ✅ | H | L | ROIStatsWidget (image/curve ROI stats display) | `ROIStatsWidget.py (entire file)` | W15: `RoiStatsWidget` shows a per-ROI stats table (`ROI \| N \| min \| max \| mean \| sum \| integral`) over the active item via `PlotWidget::feed_roi_stats`/`show_roi_stats_widget`, reducing each ROI with `image_roi_stats`/`curve_roi_stats`. |
| ✅ | H | M | ROI per-instance color (independent from manager default) | `items/_roi_base.py:389-405, tools/roi.py:713-742` | Wave 13: `ManagedRoi.color: Option<Color32>` (silx `setColor`/`getColor`); the live `chrome::draw_rois` path resolves `managed.color.unwrap_or(plot.roi_color)` via pure `chrome::roi_appearance`; high-level `set_roi_color` + manager color buttons write the per-ROI override. Appearance resolution headlessly tested; on-screen pixels GPU-unverified. |
| ✅ | H | M | ROI label/text display on canvas | `items/_roi_base.py:492-511` | Wave 13: `ManagedRoi.name` flows through `chrome::roi_appearance.name` into `draw_roi`, which renders the name as a label at the ROI anchor (chrome.rs:1006); empty name → no label. High-level `set_roi_name` + manager name editor. Resolution tested; label pixels GPU-unverified. |
| ✅ | H | M | ROI selection/highlighting (visual feedback) | `tools/roi.py:528-590 (setCurrentRoi/getCurrentRoi/sigCurrentRoiChanged)` | Wave 13: `Plot::set_current_roi` (plot.rs:518) is the sole writer of `ManagedRoi.selected` (silx `setHighlighted`); `draw_roi` thickens the selected ROI's stroke (chrome.rs:782). Single-owner invariant (exactly one selected; out-of-range clears; remove adjusts index) headlessly tested; highlight pixels GPU-unverified. |
| ✅ | H | M | ROI contains() point-in-region test | `items/roi.py:61-1599 (all ROI classes have contains method)` | `Roi::contains((x,y))` implemented for every variant (roi.rs:838): Rect/HRange/VRange inclusive bbox, HLine/VLine/Point exact-coord, Cross x\|y, Line unit-square intersection, Polygon/Band point-in-polygon, Circle dist≤r, oriented-Ellipse projection, Arc ring+sweep (`arc_contains`). 13 headless tests (rect/band/point/cross/circle/ellipse×2/polygon/line/arc×2). |
| ✅ | H | M | Manager current ROI tracking (setCurrentRoi/getCurrentRoi) | `tools/roi.py:528-590` | Wave 13: `Plot::current_roi`/`set_current_roi` (plot.rs:509/518) own the selected-ROI index; `RoiManagerWidget` reads `plot.current_roi()` and writes via `plot.set_current_roi()` (no own state); `PlotEvent::CurrentRoiChanged { previous, current }` mirrors silx `sigCurrentRoiChanged`. Selection invariant tested. |
| ✅ | H | S | ROI name/label (string identifier) | `items/_roi_base.py:77-92, CurvesROIWidget.py:594-640` | `ManagedRoi.name: String` (silx `getName`/`setName`); high-level `set_roi_name` and the manager's per-row name editor write it; resolved into the canvas label (row above). |
| ✅ | M | L | EllipseROI kind (center, radii, orientation handles) | `items/roi.py:950-1177` | `Roi::Ellipse { center, radii, orientation }` with on-plot creation (`RoiDrawKind::Ellipse`) and full rotational geometry: the two axis handles sit at θ / θ+π/2 and dragging one off-axis sets that semi-axis and rotates (silx `EllipseROI.handleDragUpdated`); `contains`, `screen_rect`, and the outline are all orientation-aware. Models `EllipseROI._orientation` as `orientation` (radii.0 along it, radii.1 perpendicular). |
| ✅ Done | M | M | CrossROI kind (point with cross marker) | `items/roi.py:133-185` | `Roi::Cross { center }` exists with Wave 12 on-plot creation (`RoiDrawKind::Cross`, single click via `DrawMode::Point`); `1cbda70` makes the drag handle a square (silx `addHandle()` default symbol "s") via `kind()` returning a `Vertex`. silx's transient label-handle management remains a manager-path concern (same as row 173). |
| ✅ | M | M | CircleROI kind (center + radius handles) | `items/roi.py:800-950` | `Roi::Circle { center, radius }` with on-plot creation (`RoiDrawKind::Circle`) AND dedicated edit handles: `edges()` → `[Vertex(0) center, Vertex(1) perimeter]`, `move_edge` (V0 translate, V1 sets radius), rendered+hit-tested via `draw_roi_handles`/`roi_grab_at`. Tests `circle_handles_drag_center_and_radius`, `circle_perimeter_resize_works_under_inverted_y`. |
| ✅ | M | M | ROI handle symbols ('+', 's', 'o', custom glyphs) | `items/_roi_base.py:735-759 (addHandle/addLabelHandle/addTranslateHandle), PolygonROI:1244-1274` | `draw_roi_handles` draws the silx role symbols: `+` for translate/center, square `s` for vertex/edge. The `o` is silx `PolygonROI._handleClose` (a transient role-"user" creation marker from the `RegionOfInterestManager` path); siplot creates polygons via the `PlotInteraction` draw path and shows its faithful close target instead — the `SelectPolygon.updateFirstPoint` fill=None box. |
| ✅ | M | M | ROI line style (solid, dash, dot, gap color) | `items/_roi_base.py:600-680, RectangleROI:534-552` | `ManagedRoi.line_style` (`RoiLineStyle::{Solid,Dashed,Dotted}` → `LineStyle`) emits real dash/dot segments via `chrome::draw_styled_line`; `ManagedRoi.gap_color` fills dash gaps (silx `setLineGapColor`, commit `a6298b9`). Manager UI exposes style combo + gap swatch; `set_roi_line_style`/`set_roi_line_gap_color` setters. GPU-unverified on-screen. |
| ◐ | M | M | Manager signals (sigRoiAdded, sigRoiChanged, sigCurrentRoiChanged, sigInteractiveRoiCreated/Finalized) | `tools/roi.py:331-371` | Now distinct: `RoiAdded { index }` (silx `sigRoiAdded`) fires from every add — `add_roi`/`add_managed_roi` and the interactive create (which emits `RoiAdded` then `RoiCreated`, silx's `sigRoiAdded`→`sigInteractiveRoiFinalized` order). `RoiChanged` (silx `sigRoiChanged`) is now geometry-change-only; `CurrentRoiChanged` = `sigCurrentRoiChanged`; `RoiCreated` = `sigInteractiveRoiFinalized` (draw finished). `RoiAboutToBeRemoved { index }` (silx `sigRoiAboutToBeRemoved`) is now emitted by `remove_roi` *before* the removal (so a listener can still read the dropped ROI); `RoisCleared` is reserved for `clear_rois` (clear-all) — removing the prior dual meaning where a single removal also emitted `RoisCleared`. Regression-tested in `tests/roi_events.rs` (3 cases: about-to-be-removed-before-removal + not-RoisCleared, clear-all, out-of-range no-op). Remaining ◐: no separate `sigInteractiveRoiCreated` (mid-gesture) — siplot builds the ROI only on draw-finish, so there is no mid-draw ROI object to signal (the rubber-band emits `DrawingProgress` instead). |
| ✅ | M | M | ROI creation phase UI (preview overlay, mode indicator, close-polygon handle) | `PolygonROI:1241-1256, tools/roi.py:493-510, 1101-1175` | All three present: (1) live preview overlay during the draw gesture (`Interaction.roi_preview` → `draw_overlay`); (2) the polygon 'close' target — `paint_draw_preview` draws `draw_polygon_first_point` (silx `SelectPolygon.updateFirstPoint` fill=None box) around the first vertex while drawing a polygon; (3) mode indicator — `PlotInteractionMode::roi_creation_message(roi_count)` / `PlotWidget::roi_creation_message()` produce silx's `InteractiveRegionOfInterestManager.getMessage` string ("Select {SHORT_NAME}s ({n} selected)") while a create mode is armed, for a host status bar (silx's status message is app-displayed too). Message + `RoiDrawKind::short_name` are headlessly tested; the on-screen preview/close-target render is GPU-UNVERIFIED. |
| ✅ | M | S | ROI line width customization | `items/_roi_base.py:600-680, RectangleROI:534-552` | `ManagedRoi.line_width` (default 1.0) → `RoiAppearance.line_width` applied as the stroke width (selected → `max(w, 2)`); manager UI `DragValue` (0.1..=20.0); `set_roi_line_width` setter. |
| ✅ | M | S | ROI fill color and fill enable/disable | `items/Shape.py (shape draw), RectangleROI:531-539` | `ManagedRoi.fill` (default false) → `RoiAppearance.fill`; `Some(false)` draws no interior (silx `setFill(False)`), else the translucent tint at the ROI color (matches silx, which fills with the ROI color). Manager UI fill checkbox; `set_roi_fill` setter. Fill color is not separately configurable (derives from outline color), matching silx. |
| ✅ | M | S | Manager ROI color default (setColor/getColor) | `tools/roi.py:782-797` | `Plot::roi_color` (silx default red) is the per-ROI fallback in `draw_rois(.., default_color, ..)`/`roi_appearance` when `managed.color` is None; manager rows show the resolved color and write per-ROI overrides. |
| ✅ | M | S | ROI context menu (edit, delete, mode selection) | `tools/roi.py:625-642` | Right-clicking a ROI on the canvas opens a per-ROI menu (silx `_createMenuForRoi`): ROI-name title, "Make current" (→ `set_current_roi`, silx `sigCurrentRoiChanged`), "Remove" (→ `remove_roi`), and — for `InteractionModeMixIn` ROIs (Arc/Band) — a nested "Interaction mode" submenu (silx `createMenuForInteractionMode`) of the ROI's `available_interaction_modes`, the current one radio-checked, each tooltipped with its silx description, applied via `set_roi_interaction_mode` → `RoiInteractionModeChanged`. The target ROI is captured at right-click via `roi_grab_at` and held in `ui.data` for the menu's lifetime; the canvas only signals intent (`Interaction.roi_removed`/`roi_make_current`/`roi_set_interaction_mode`) and the high-level owner performs the mutation + event. "edit" = on-canvas handle drag (Wave 12, separate). The mode-change API + event are GPU-backed-tested in `tests/roi_events.rs`; the menu render itself is GPU-unverified. |
| ◐ | L | L | ArcROI kind (circular arc with start/end angle) | `items/_arc_roi.py (full file)` | `Roi::Arc { center, inner/outer radius, start/end angle }` exists; Wave 12 adds on-plot creation (`RoiDrawKind::Arc`, 2-point line drag) and PolarMode handle editing (mid/weight/start/end). The ThreePointMode geometry is now ported too — `arc_from_three_points` (faithful `_createGeometryFromControlPoints`), `arc_control_points`, and `arc_three_point_drag` (control-point reshape, re-fits the circumcircle, preserves weight); `arc_from_two_points` delegates to it. The mode is now selectable via the right-click "Interaction mode" submenu + `set_roi_interaction_mode` (row 1179). Remaining ◐: the live handle dispatch does not yet swap ThreePointMode↔PolarMode editing based on the selected mode (GPU/UI, unwired). |
| ◐ | L | L | BandROI kind (rotatable rectangular band) | `items/_band_roi.py (full file)` | `Roi::Band { begin, end, width }` exists with on-plot creation (`RoiDrawKind::Band`) and silx's width constraints (`width>=0`, width handles constrained to the band normal). Orientation is free via the begin/end handles (silx has no separate rotate handle). silx's real sub-modes are `BoundedMode` (polygon, siplot default) and `UnboundedMode` (three infinite lines) — the Unbounded geometry is now `Roi::band_lines()` (faithful `BandGeometry` line equations, `Sloped`/`Vertical` sum type). The mode is now selectable via the right-click "Interaction mode" submenu + `set_roi_interaction_mode` (row 1179). Remaining ◐: the live render does not yet swap the bounded polygon for the three unbounded lines based on the selected mode (GPU/UI). |
| ✅ | L | M | HorizontalLineROI kind (single y coordinate) | `items/roi.py:373-435` | `Roi::HLine { y }` is a single-y full-span line, distinct from the `HRange` band: one edge (the line's y), one handle, `contains` = exact-y. `RoiDrawKind::HLine`→`DrawMode::HLine` (silx `SelectHLine` `_plotShape="hline"`), `roi_from_draw` captures the row; chrome draws it as a styled full-width line. Geometry/grab/draw-build tested; GPU draw unverified |
| ✅ | L | M | VerticalLineROI kind (single x coordinate) | `items/roi.py:437-510` | `Roi::VLine { x }` analogue: single-x full-span line, one edge (the line's x), exact-x `contains`. `RoiDrawKind::VLine`→`DrawMode::VLine` (silx `SelectVLine`). Tested; GPU draw unverified |
| ◐ | L | M | ROI interaction modes (select, edit, focus constraints) | `items/_roi_base.py:135-230 (InteractionModeMixIn, RoiInteractionMode enum)` | Wave 12 adds a select-vs-create mode toggle and, within editing, both edge-drag and whole-ROI translate (`RoiGrab::Edge`/`RoiGrab::Translate`). The silx `InteractionModeMixIn` data model is now ported: `RoiInteractionMode` (Arc `3 points`/`Polar`/`Translation`, Band `Bounded`/`Unbounded`, with silx labels + descriptions), `Roi::available_interaction_modes`/`default_interaction_mode`, and a validated `ManagedRoi::interaction_mode`/`set_interaction_mode` that rejects modes foreign to the kind — so the invariant `mode ∈ available_modes` holds by construction; `ManagedRoi::new` seeds the silx default per kind. 5 headless tests. The mode is now selectable through the right-click "Interaction mode" submenu + `PlotWidget::set_roi_interaction_mode` (→ `RoiInteractionModeChanged`, row 1179). Remaining ◐: the live mode-gated handle dispatch (the edit actually swapping ThreePoint↔Polar / bounded↔unbounded behaviour based on the selected mode) is GPU/UI and unwired. |
| ✅ | L | M | ROI edge position constraints | `items/roi.py:666-718 (RectangleROI), _arc_roi.py:889, _band_roi.py:66,383-392` | All silx-real edge constraints are enforced in `move_edge`: Rect/Range handles flip around the fixed opposite (silx min/max `_setBound`) rather than collapsing; Arc keeps `inner<=outer`/`inner>=0`; Band keeps `width>=0` + the orthogonal width-handle constraint. silx has no ratio-lock, grid-snapping, or inter-ROI collision/overlap prevention (the "Band preventing overlap" was a mis-attribution — `_band_roi.py` only clamps width and constrains width handles to the normal). |
| ✅ | L | M | ROI save/load from file (dictdump format) | `CurvesROIWidget.py:194-210, roi save/load methods` | Data layer + widget wiring ported: `core::roi_io` `encode_rois`/`decode_rois` (hand-written line-oriented text, no serde — mirrors the `.npy` mask path) round-trip every `Roi` variant + name/color/line width+style/gap color/fill (the transient `selected` is dropped, as silx `ROI.toDict` does), with `save_rois`/`load_rois` file wrappers (silx `CurvesROIWidget.save`/`load(filename)`). `PlotWidget::save_rois_to_path`/`load_rois_from_path` are the testable widget-level wrappers — load replaces the live set via `clear_rois` then one `add_managed_roi` per ROI, so it emits `RoisCleared` + N×`RoiAdded`; `save_rois_dialog`/`load_rois_dialog` open the native rfd file picker, mirroring the figure `save_dialog` (row 236). 10 tests: 9 `roi_io` round-trip/error cases + a GPU-backed `PlotWidget` save→mutate→load round-trip in `tests/roi_events.rs` asserting the replaced set + the cleared/added events. The rfd dialogs are GPU/UI-unverified shims, as with every native dialog. |
| ✅ | L | S | ROI keyboard shortcuts / text input for named creation | `tools/roi.py:1072-1098 (eventFilter)` | Ported the real silx ROI key bindings via pure `roi_key_action`: Enter→Validate (close polygon), Delete/Backspace/Ctrl+Z→UndoLast (`removeRoi(rois[-1])`). silx has no key→shape map and no named-creation dialog (those were speculative); ROI naming is via the manager (`ManagedRoi.name`). Wired into the RoiCreate block; mapping headlessly tested. |

## Mask tools (image + scatter)  — 25✅ 2◐ 0☐

siplot's mask tools are comprehensive. `MaskToolsWidget` (image) and `ScatterMaskWidget` (scatter points by index) both implement multi-level masks (0 unmasked, 1..=255 levels), the full drawing-tool set (pencil/eraser brush with Bresenham line interpolation + variable width, rectangle, polygon, ellipse), threshold masking (below/between/above), NaN masking, per-level invert/clear, undo/redo history (silx `BaseMask`), per-level color overrides, a transparency slider, the Mask/Unmask + Ctrl toggle, and a pan/browse (no-draw) tool — all rendered by `show_toolbar` and faithful to silx `_BaseMaskToolsWidget`. The colormapped overlay (`mask_overlay_lut`) renders the masked pixels. Remaining ◐: mask file save/load have the `.npy` AND `.edf` paths (`write_npy`/`read_npy`, `write_edf`/`read_edf`, sharing the `apply_loaded_mask` crop/pad owner) but no file dialog and no TIFF/HDF5/csv/msk formats (external-crate-bound, deferred under the no-new-dep pattern).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | L | Multi-level masks (1-255 levels) | `_BaseMaskToolsWidget.py:279, 583-585` | `MaskToolsWidget` masks a per-pixel `u8` grid: 0 unmasked, 1..=255 are up to 254 non-overlapping levels; drawing writes the current `level` (highlighted at full alpha, others half). `ScatterMaskWidget` is the 1D analogue. Tested. |
| ✅ | H | L | Scatter mask variant (1D point masking) | `ScatterMaskToolsWidget.py (entire file, 643 lines)` | `ScatterMaskWidget` (scatter_mask.rs): a multi-level mask over scatter points by index (1D `u8` buffer, one per point), same level/invert/clear/undo semantics as the image mask but over point coordinates (silx `ScatterMaskToolsWidget`). Tested. |
| ✅ | H | M | Drawing tool: Rectangle | `MaskToolsWidget.py:805-826, _BaseMaskToolsWidget.py:307-317, shapes.polygon_fill_mask` | `MaskTool::Rectangle` → `DrawMode::Rectangle`; `handle_shape_draw` + `rect_params_to_cells` (silx int truncation) → `update_rectangle` on finish. Live row 194. Tested. |
| ✅ | H | M | Drawing tool: Polygon | `MaskToolsWidget.py:840-847, _BaseMaskToolsWidget.py:319-326, shapes.polygon_fill_mask` | `MaskTool::Polygon` → click-to-add/snap-close draw; `polygon_vertices_to_cells` + `polygon_fill_mask` (silx `(y,x)` swap) → `update_polygon` on finish. Live row 195. Tested. |
| ✅ | H | M | Drawing tool: Ellipse | `MaskToolsWidget.py:828-838, _BaseMaskToolsWidget.py:351-361, shapes.ellipse_fill` | `MaskTool::Ellipse` → `ellipse_params_to_cells` (center int, y-semi→radius_r, x-semi→radius_c) → `update_ellipse` on finish. Live row 196. Tested. |
| ✅ | H | M | Undo/Redo history | `_BaseMaskToolsWidget.py:144-194 (resetHistory, commit, undo, redo), 609-629 (undo/redo actions)` | `MaskHistory` (bounded snapshot stack, default depth = silx `historyDepth`) with `commit`/`undo`/`redo` mirroring silx `BaseMask` (`_history`/`_redo`, redo cleared on commit, undo needs >1 snapshot); `can_undo`/`can_redo` = silx `sigUndoable`/`sigRedoable`. Tested. |
| ✅ | H | M | Threshold-based masking (below, between, above) | `_BaseMaskToolsWidget.py:265-294 (updateBelowThreshold, updateBetweenThresholds, updateAboveThreshold), 848-937 (threshold UI), 1204-1225 (apply handler)` | `ThresholdMode::{Below, Between, Above}` + `update_threshold` (below→data<min, between→min≤data≤max, above→data>max) at the current level; ImageView toolbar has the mode selector + min/max fields + Apply. Live row 199. Tested. |
| ◐ | H | M | Mask save to file (npy/edf/tif/h5/msk) | `MaskToolsWidget.py:104-141 (save method: edf/tif/npy/h5/msk), 698-785 (_saveMask dialog and format handling)` | `MaskToolsWidget::write_npy` writes a 2D `uint8` `.npy` (silx `save(.., "npy")`); `write_edf`/`save_edf`/`save_mask_edf` now write fabio-style EDF (`render::save::encode_mask_edf`, hand-written, no new dep). Remaining ◐: no file dialog and no TIFF/HDF5/msk encoders (external-crate-bound). Live row 200. |
| ◐ | H | M | Mask load from file (npy/edf/tif/h5/msk) | `MaskToolsWidget.py:589-629 (load method), 630-677 (_loadMask dialog)` | `read_npy` and `read_edf` read a 2D `uint8` mask and apply it through the shared `apply_loaded_mask` owner (crop/pad to the image, committed to undo; silx `load` npy/edf branches); `decode_mask_edf` parses the fabio `{…}` header. Remaining ◐: no file dialog and no TIFF/HDF5/msk decoders (external-crate-bound). |
| ✅ | H | M | Scatter mask disk/circle drawing | `ScatterMaskToolsWidget.py:137-148 (updateDisk), 585-612 (pencil event handling)` | `ScatterMaskWidget::update_disk` masks points inside a disk: stencil `(y-cy)²+(x-cx)² < radius²` (silx `updateDisk`). Tested. |
| ✅ | H | M | Scatter mask polygon selection | `ScatterMaskToolsWidget.py:107-122 (updatePolygon with point-in-polygon test)` | `ScatterMaskWidget` masks points via `update_stencil` over a `point_in_polygon` test of each point (silx `Polygon.is_inside`). Tested. |
| ✅ | H | S | Mask level spinbox (1-255) with tooltip | `_BaseMaskToolsWidget.py:583-592` | `MaskToolsWidget::show_toolbar` renders a `Slider` over `level` 1..=255 (silx `levelSpinBox`; siplot uses a slider rather than a spinbox); the selected level is the highlighted/edited one. |
| ✅ | M | M | Mask visibility overlay with colormap | `MaskToolsWidget.py:371-392 (_updatePlotMask creates MaskImageData item), _BaseMaskToolsWidget.py:984-1010 (_setMaskColors)` | `colormap.rs` `mask_overlay_lut` builds the per-level discrete LUT (selected level full alpha, others half, level 0 transparent — faithful `_setMaskColors`); `mask_tools.rs` `apply()` uploads it as an overlay image at active-image z+1. Tested. GPU-unverified on-screen. |
| ✅ | M | M | Mask display sync on active item change | `MaskToolsWidget.py:402-433 (showEvent/hideEvent), 434-443 (_activeImageChanged)` | `ImageView::set_image` auto-calls `mask.reset_geometry(width,height)` on a dimension change before re-upload — the silx `ImageView`+mask parity target syncs automatically. (The standalone low-level `MaskToolsWidget` still needs a manual `reset_geometry`.) |
| ✅ | M | S | Drawing tool: Pencil/Brush with variable width | `_BaseMaskToolsWidget.py:822-846 (pencilSpinBox 1-1024, slider 1-50), MaskToolsWidget.py:854-876` | `7b6f263`: a 1-50 slider AND a 1-1024 `DragValue` spin box, both binding the same `brush_size` (egui keeps them in lockstep, ≈ silx `_pencilWidthChanged`), in both mask-draw toolbars. GPU-unverified on-screen. |
| ✅ Done | M | S | Drawing tool: Eraser (unmask) | `_BaseMaskToolsWidget.py:806-810 (maskStateGroup radio buttons Mask/Unmask, Ctrl modifier toggles)` | `d85c751`: Mask/Unmask radio (`mask_state`) plus `effective_do_mask = base ^ ctrl` (silx `_isMasking()`); holding Ctrl (egui `command`, = Cmd on macOS per Qt) inverts every draw, captured once per pencil stroke. siplot's mask stays boolean (no 1-255 levels). Toolbar GPU-unverified |
| ✅ | M | S | Clear mask (per level or all) | `_BaseMaskToolsWidget.py:198-205 (clear method), 655-670 (Clear/Clear All actions)` | `mask_tools.rs` `clear()` (current level only, silx `clear(level)`) + `clear_all()` (all levels), surfaced as "Clear" + "Clear All" buttons in `MaskToolsWidget::show_toolbar`. (ImageView's mask toolbar exposes `clear_all` only.) |
| ✅ Done | M | S | Invert mask (per level) | `_BaseMaskToolsWidget.py:207-218, 645-653 (invert action and handler)` | `ImageMask::invert` swaps masked/unmasked pixels (boolean mask, so a single "level"); `d85c751` adds the silx Ctrl+I shortcut via `consume_key(COMMAND, Key::I)` alongside the existing invert button. `invert_swaps_zero_and_current_level_only` tested. Toolbar GPU-unverified |
| ✅ | M | S | Mask NaN/non-finite values | `_BaseMaskToolsWidget.py:296-304 (updateNotFinite), 941-952 (button)` | `mask_tools.rs` `mask_not_finite` (`!v.is_finite()` stencil, silx `updateNotFinite`); "mask non-finite" button in ImageView's and ScatterView's mask toolbars. Tested. (Standalone `MaskToolsWidget::show_toolbar` does not carry this button.) |
| ✅ | M | S | Mask transparency/alpha slider | `_BaseMaskToolsWidget.py:554-577 (slider range 3-10, affects alpha in _setMaskColors)` | `badfbee`/`f3cbd95`: transparency slider in `MaskToolsWidget::show_toolbar` routes through the tested `set_transparency` (silx `transparencySlider` → `_setMaskColors`: selected level at this alpha, others at half) and re-renders the overlay LUT. (Same as row 202.) GPU-unverified on-screen. |
| ✅ | M | S | Mask interaction mode vs pan/zoom | `_BaseMaskToolsWidget.py:1115-1161 (mode activation sets plot.setInteractiveMode()), 1096-1106 (interaction mode changed handler)` | `ImageView::set_mask_draw(true)` switches the image plot to `PlotInteractionMode::MaskDraw` + `MaskTool::Pencil` (and restores Zoom/None on exit), so activating the mask tool disables pan/zoom automatically — `actions::mode::mask_draw_mode`. (The low-level standalone widget still relies on the caller setting the mode.) |
| ✅ Done | M | S | Mask/Unmask mode toggle with Ctrl modifier | `_BaseMaskToolsWidget.py:790-810 (radio buttons), 1169-1178 (_isMasking checks Ctrl)` | `d85c751`: `mask_state` Mask/Unmask radio + Ctrl-modifier inversion (`effective_do_mask = base ^ ctrl`, silx `_isMasking()`). Truth-table tested; toolbar GPU-unverified |
| ✅ | M | S | Load colormap range button | `_BaseMaskToolsWidget.py:883-892, 880-896 (MaskToolsWidget override)` | `0d83368`: "Min-max from colormap" button in ImageView's threshold row copies `colormap.vmin/vmax` (the effective post-autoscale range) into `mask.threshold_min/max`. Threshold masking itself (`update_below/between/above`) was already present. GPU-unverified on-screen. |
| ✅ | M | S | Pencil line interpolation between drag events | `MaskToolsWidget.py:849-876 (updateLine if lastPencilPos != current)` | `mask_tools.rs` `paint_pencil_point` tracks `last_pencil_pos` and calls `update_line` (Bresenham, width = brush size) from the previous sample before stamping the disk, so a fast drag leaves no gap (silx `updateLine`). Tested. |
| ✅ | L | S | Custom mask color per level (per-level RGB) | `_BaseMaskToolsWidget.py:394-398 (_defaultColors, _overlayColors arrays), 973-982 (getCurrentMaskColor), 1026-1046 (setMaskColors, getMaskColors)` | `overrides: Vec<Option<[u8;3]>>` + `set_mask_colors`/`reset_mask_colors` (faithful `setMaskColors`/`resetMaskColors`, per-level or all) flow through `mask_overlay_lut`; `current_mask_color` mirrors `getCurrentMaskColor`. `MaskToolsWidget::show_toolbar` exposes a per-level color swatch + "Reset color" button. Helpers tested; toolbar swatch GPU-unverified. |
| ✅ | L | S | Pan/Browse tool (no drawing) | `_BaseMaskToolsWidget.py:720-722 (PanModeAction added)` | `MaskTool::None` ("○", tooltip "Disable masking") in `show_toolbar` is the explicit no-draw tool (the default), leaving pan/zoom active — silx's Browse/pan button. |
| ✅ | L | S | Pencil size sync (spinbox + slider) | `_BaseMaskToolsWidget.py:825-846, 1057-1069` | A 1-50 `Slider` AND a `DragValue` spin box both bind the same `brush_size` (egui keeps them in lockstep, silx `_pencilWidthChanged`), in `show_toolbar` and the ImageView mask toolbar. Same as row 1209. |

## Actions, Toolbars, Tool Buttons  — 30✅ 1◐ 0☐

siplot ships a rich unified toolbar via `show_toolbar()` / `show_toolbar_with()`, with `ToolbarResponse` exposing every state change. Implemented actions: Reset Zoom, Zoom In/Out, Zoom Back (limits-history pop), Select/Pan/Zoom modes, X/Y axis autoscale toggles, X/Y log scale, X/Y invert, Major/Minor grid, Keep-aspect, Show-axis, Crosshair, Curve Style cycle, Save (PNG/PPM/SVG/TIFF + CSV), Copy (clipboard), Print, Median Filter (1D/2D), Pixel-Intensity Histogram, and the Data-Aggregation mode selector (None/Max/Mean/Min). Colormap and colorbar have dedicated widgets (`ColormapDialog`, `ColorBarWidget`); profile/symbol tool buttons exist (`ProfileToolButton`, `SymbolToolButton`); `LimitsWidget` exposes editable axis-limit fields. silx's separate Qt composite toolbars (Interactive/Output/Curve/Image) are delivered as one immediate-mode `show_toolbar` + standalone widgets — a UI-paradigm difference, not missing features. The `PanWithArrowKeysAction` enable/disable toggle is now wired (`Plot::set_pan_with_arrow_keys` + `actions::control::toggle_pan_with_arrow_keys`, default-on per silx). The `RulerToolButton` now exists as a reusable checkable widget with a tested distance core (◐ — the live line-ROI distance label is host integration, not yet wired end-to-end), and the zoom-axes menu (silx `ZoomEnabledAxesMenu`) is wired. The Curve Style action now mirrors silx's plot-wide `(lines,points)` boolean cycle (`PlotWidget::cycle_curve_style` → `set_default_plot_lines`/`set_default_plot_points`), replacing the earlier concrete-`LineStyle` divergence. Remaining ◐: the `RulerToolButton` live label only. The OpenGL-vs-Matplotlib backend toggle is N/A (single wgpu backend).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | L | Save Action (PNG/SVG/data formats) | `actions/io.py:77-747` | `SaveTarget` (`Figure(SaveFormat)` = PNG/PPM/SVG/TIFF, `CurveCsv` = data) + `from_extension`/`from_path`; the `Save` toolbar button (`ToolbarIcon::Save`) calls `save_graph`/`save_graph_with_format`. The figure GPU readback is the untestable shim. |
| ✅ | H | M | Zoom Back (limits history pop) | `actions/control.py:101-122` | `actions/control.rs` `zoom_back` pops `Plot::limits_history` (unbounded stack, pushed before each zoom/pan); wired to the `ToolbarIcon::ZoomBack` button (silx `ZoomBackAction` / `getLimitsHistory().pop()`). |
| ✅ | H | M | X Axis Autoscale (separate toggle) | `actions/control.py:172-197` | `actions/control.rs` `toggle_x_autoscale` flips `Plot::x_autoscale` and re-fits (silx `XAxisAutoScaleAction`, checkable); surfaced via `ToolbarIcon::AutoscaleX` + `ToolbarResponse::autoscale_x_changed`. |
| ✅ | H | M | Y Axis Autoscale (separate toggle) | `actions/control.py:199-224` | `actions/control.rs` `toggle_y_autoscale` flips `Plot::y_autoscale` and re-fits (silx `YAxisAutoScaleAction`); surfaced via `ToolbarIcon::AutoscaleY` + `ToolbarResponse::autoscale_y_changed`. |
| ✅ | H | M | OutputToolBar composite (Copy/Save/Print) | `tools/toolbars.py:71-113` | The Copy (`rgba_to_clipboard_image`), Save (`save_graph`), and Print (`print_graph`) actions are all present as `ToolbarIcon::Copy`/`Save`/`Print` buttons in the unified `show_toolbar` (siplot uses one toolbar rather than silx's separate QToolBar composites). |
| ✅ | M | L | Pixel Intensities Histogram Action | `actions/histogram.py:1-180+` | `actions/analysis.rs` `pixel_intensity_histogram` (silx Histogramnd + nanmean/nanstd/nansum → `PixelHistogram`); `ImageView::show_pixel_histogram_toolbar` opens a detached tool window (`detached::show_detached`) with live bars + stats + editable bin DragValue. GPU-unverified on-screen. |
| ✅ | M | M | Curve Style Cycling (lines/lines+marks/marks) | `actions/control.py:317-350` | `actions/control.rs` `curve_style_cycle` → `PlotWidget::cycle_curve_style` now mirrors silx's plot-wide `(lines,points)` boolean cycle: `(F,F)→(T,F)`, otherwise `(T,F)→(T,T)→(F,T)→(T,F)` (pure `next_curve_style_state`, silx `CurveStyleAction._actionTriggered`). It applies the new defaults to every curve via `set_default_plot_lines`/`set_default_plot_points` (silx `setDefaultPlotLines`/`setDefaultPlotPoints`: linestyle `"-"`/`" "`, symbol `"o"`/`""` on all `getAllCurves` items, i.e. `PlotItemKind::Curve`). State lives in `PlotWidget::default_plot_lines`/`default_plot_points` (silx init `True`/`False`), exposed via `is_default_plot_lines`/`is_default_plot_points`; toolbar `ToolbarIcon::CurveStyle` wired. Per-curve lines/points toggles remain separately via `set_curve_lines_visible`/`set_curve_points_visible`. Transition tested headlessly; the per-curve GPU re-apply is GPU-unverified. |
| ✅ | M | M | Colorbar Toggle (ColorBarAction) | `actions/control.py:452-488` | `actions/control.rs` `image_colorbar_toggle`/`scatter_colorbar_toggle` flip `show_colorbar`; surfaced as a selectable "colorbar" toolbar control in ImageView/ScatterView. |
| ✅ | M | M | Zoom Mode Action (with optional axes menu) | `actions/mode.py:45-106, toolbars.py:50-51` | Zoom mode button (checkable) plus a "Zoom axes" menu in `show_toolbar` (silx `ZoomEnabledAxesMenu`): X-axis / Y-axis checkboxes (default on) drive `Plot::set_zoom_enabled_axes`/`zoom_x_enabled`/`zoom_y_enabled`. A box zoom is constrained via the pure `interaction::constrain_zoom_axes` (silx `Zoom._getAxesExtent`: a disabled axis keeps its current range), wired at the box-zoom commit (`plot_widget.rs`), `ToolbarResponse::zoom_axes_changed`. siplot box zoom is left-axis only, so there is no y2 entry (silx's third). Constraint + state core tested; menu GPU/UI-unverified. |
| ✅ | M | M | Copy to Clipboard Action | `actions/io.py:848-927` | `PlotWidget::copy_to_clipboard(size)` (`high_level.rs:6379`) renders the figure to RGBA and writes it via `arboard::Clipboard` (`io::rgba_to_clipboard_image`); toolbar `ToolbarIcon::Copy` button wired (`:5552`, "Copy figure to clipboard"). The off-screen GPU render shares save_graph's headless-unverified boundary; the copy + clipboard-write path is implemented. |
| ✅ | M | M | InteractiveModeToolBar composite | `tools/toolbars.py:37-69` | Select/Pan/Zoom mode actions (`actions/mode.rs` `select_mode`/`pan_mode`/`zoom_mode`) render as `ToolbarIcon::Select`/`Pan`/`Zoom` in the unified `show_toolbar` (`ToolbarResponse::interaction_mode_changed`); `show_toolbar_with` allows custom button sets. egui immediate-mode UI has no Qt composable-QToolBar paradigm. |
| ✅ | M | M | CurveToolBar composite | `tools/toolbars.py:179-227` | All constituents exist in the unified `show_toolbar`: Reset Zoom (`Home`), X/Y Autoscale (`AutoscaleX`/`AutoscaleY`), Grid (`Grid`/`MinorGrid`), Curve Style (`CurveStyle`), Crosshair (`Cursor`). The stale "missing Curve Style and per-axis Autoscale" no longer holds. |
| ✅ | M | M | LimitsToolBar (editable X/Y min/max fields) | `tools/LimitsToolBar.py:35-123` | `PlotWidget::show_limits_toolbar` (`high_level.rs`): a `Limits: X: [min][max] Y: [min][max]` row of `DragValue` fields that always reflect the effective limits (silx `limitsChanged` slot) and, on edit, apply via `set_graph_x_limits`/`set_graph_y_limits` ordered min ≤ max (silx `_xFloatEditChanged` swap, `ordered_limits`, tested). Y edits the primary axis. |
| ✅ | M | S | Zoom In (1.1x factor) | `actions/control.py:124-146` | `control::zoom_in` → `apply_zoom(plot, ZOOM_STEP)` with `ZOOM_STEP = 1.1` (silx `applyZoomToPlot` factor) about the view center; toolbar `ToolbarIcon::ZoomIn` button wired (`high_level.rs:5370`); tested (`control.rs:272`). |
| ✅ | M | S | Zoom Out (1/1.1x factor) | `actions/control.py:148-170` | `control::zoom_out` → `apply_zoom(plot, 1.0 / ZOOM_STEP)`; toolbar `ToolbarIcon::ZoomOut` button wired (`high_level.rs:5374`); tested (`control.rs:278`). |
| ✅ | L | L | Print Action | `actions/io.py:747-847` | `print_graph` (rasterize → temp PNG via `print_temp_png_path` → printer) wired to the `ToolbarIcon::Print` button (`ToolbarResponse::print`). The GPU readback + printer submit are native shims (as with Save's figure readback); the in-app print-*preview* dialog is tracked separately (Stats/Print section). |
| ✅ | L | L | Median Filter Actions (1D/2D) | `actions/medfilt.py:49-150+` | `actions/analysis.rs` `median_filter_1d` and `median_filter_2d` (silx `MedianFilter1DAction`/`MedianFilter2DAction`); the `ToolbarIcon::MedianFilter` button toggles it. |
| ✅ Done | L | L | ScatterVisualizationToolButton | `PlotToolButtons.py:480-549` | `4477bc2`: `ScatterView` toolbar ComboBox over `ScatterVisualization::ALL` (silx order) → `set_visualization`. Note: the SOLID/GRID/triangulation *renderers* remain unimplemented (rows 1080/1086); only POINTS renders today, so non-POINTS modes are selectable but render as points. Toolbar GPU-unverified |
| ✅ | L | M | Pan with Arrow Keys Toggle | `actions/control.py:603-625` | `Plot::pan_with_arrow_keys` flag (default `true`, silx `_panWithArrowKeys = True`) with `set_pan_with_arrow_keys` (silx `setPanWithArrowKeys`); `plot_widget.rs` gates the arrow-key handler on it (silx `if self._panWithArrowKeys`); `actions::control::toggle_pan_with_arrow_keys` is the checkable `PanWithArrowKeysAction`. Arrow-key panning itself via `interaction.rs` `apply_pan` + `PanDirection`. |
| ✅ | L | M | Show Axis Toggle | `actions/control.py:627-651` | `actions/control.rs` `show_axis_toggle` flips `Plot::axes_displayed` (silx `ShowAxisAction`/`setAxesDisplayed`); surfaced via `ToolbarIcon::ShowAxis` + `ToolbarResponse::show_axis_changed`. Hidden axes collapse the chrome gutters. |
| ✅ | L | M | Close Polygon Interaction Action | `actions/control.py:653-683` | `DrawState::validate` (silx `_validate`) closes the in-progress polygon at its committed vertices; wired in `apply_interaction` to the Enter key while a `RoiCreate(Polygon)` draw is active. `len > 2` gate matches the snap-close path; no-op otherwise. `validate` unit-tested; key wiring GPU-unverified. |
| ✅ | L | M | Data Aggregation Mode Selector | `actions/image.py:45-104` | `ImageView::set_aggregation` + a `selectable_value` menu (None/Max/Mean/Min, `high_level.rs:8029-8032`) drive `AggregationMode` + GPU `aggregate_blocks` (silx `AggregationModeAction` / `ImageDataAggregated`). |
| ✅ | L | M | ProfileToolButton (1D/2D profiles) | `PlotToolButtons.py:304-392` | `widget::tool_buttons::ProfileToolButton` — a standalone dropdown for the 1D/2D dimension toggle (silx STATE labels/tooltips, `set_dimension`/`dimension`, `ui() -> Option<u8>`). Complements the existing None/H/V/L/R profile-mode selector. State core tested; popup GPU/UI-unverified. |
| ✅ | L | M | SymbolToolButton (marker/size) | `PlotToolButtons.py:458-478` | `widget::tool_buttons::SymbolToolButton` — a standalone dropdown over `Symbol::ALL` with a 1..=20 size slider (default 6.0), emitting `SymbolToolChange`. State core (symbol + clamped size) tested; popup GPU/UI-unverified. |
| ◐ | L | M | RulerToolButton (distance measurement tool) | `tools/RulerToolButton.py:83-180+` | `widget::tool_buttons::RulerToolButton` — a checkable button (silx `setCheckable`/`isChecked`/`toggled`) plus the pure distance formatter `distance_text` (silx `buildDistanceText`: Euclidean norm of the two data endpoints, formatted `" .1f"` + `px`), tested headlessly. Same reusable-widget split as `ProfileToolButton`/`SymbolToolButton`: silx's `_RulerROI` maps to siplot's existing line-ROI draw; while active the host enters that draw and labels the line via `distance_text`. Residual: the live-updating distance label on the drawn line ROI is host integration, not yet wired end-to-end. |
| ✅ | L | S | Grid Toggle (both/major modes) | `actions/control.py:284-315` | `Plot::grid` (`GraphGrid::None`/`Major`/`MajorAndMinor`) toggled via `ToolbarIcon::Grid` + `MinorGrid` (two buttons, `grid_changed`/`minor_grid_changed`), covering silx `GridAction`'s major-vs-both modes. |
| ✅ N/A | L | S | OpenGL Backend Toggle | `actions/control.py:685-753` | N/A — silx `OpenGLAction` switches between its Matplotlib and OpenGL backends; siplot has a single wgpu backend, so there is no backend to toggle (the gap note itself says "no backend selection UI needed"). |
| ✅ | L | S | ImageToolBar composite | `tools/toolbars.py:115-177` | Constituents present: Reset Zoom (`Home`), Colormap (`ColormapDialog`), Aspect (`Aspect`), X/Y axis origin (`InvertX`/`InvertY`), plus a colorbar (`ColorBarWidget`) — via the unified `show_toolbar` + standalone widgets rather than a Qt composite class. |
| ✅ | L | S | AspectToolButton (menu: keep/don't keep) | `PlotToolButtons.py:57-125` | Keep-aspect is a toggle button (`ToolbarIcon::Aspect` "1:1" + `ToolbarResponse::aspect_changed`) over `Plot`'s keep-aspect state — the functional equivalent of silx's two-action menu (toggle vs menu is a UI-shape difference). |
| ✅ | L | S | XAxisOriginToolButton (menu: invert/non-invert X) | `PlotToolButtons.py:193-199` | X-axis inversion is a toggle button (`ToolbarIcon::InvertX` + `ToolbarResponse::x_inverted_changed`) — the functional equivalent of silx's menu-style invert/non-invert button (toggle vs menu is a UI-shape difference). |
| ✅ Done | L | S | ProfileOptionToolButton (sum/mean) | `PlotToolButtons.py:227-302` | `340b80c`: the `ProfileWindow` toolbar row has a Mean/Sum ComboBox driving `ProfileMethod` (functional equivalent of silx's menu button; in-window rather than a discrete tool-button popup). Window UI GPU-unverified |

## Composite views (ImageView/ScatterView/StackView/CompareImages/ComplexImageView/ImageStack)  — 25✅ 4◐ 5☐

siplot implements all six composite views. ImageView has side histograms, axis sync, the `RadarView` position-overview, profile-toolbar integration (embedded + popup `ProfileWindow`), an intensity-histogram API (`active_image_histogram`), and aggregation modes. ScatterView has value-coloured scatter, an X/Y/Data/Index position-info panel, the `ScatterMaskWidget` per-point mask tool, and line-profile extraction across the scatter (`line_profile`, silx `ScatterProfileToolBar`). StackView browses frames plus 3D volumes (perspective/axis selection, auto-transposition, per-dimension labels, and per-frame max/mean/min block aggregation). CompareImages offers A/B, vertical-line (`HalfHalf`) and horizontal-line (`SplitHorizontal`) separators, A−B subtract, red-blue-gray RGB composite modes (`RedBlueGray`/`RedBlueGrayNeg`), and origin/center/stretch image alignment for differently-shaped A and B (`CompareAlignment`: zero-pad to a common max grid, or bilinear-resample B to A's shape). ComplexImageView renders the full `ComplexMode` set with a mode toolbar and amplitude-range controls. ImageStack provides a frame browser with a selectable table, per-frame visibility, and a waiting overlay. CompareImages also has a coordinate/value status bar (`show_status_bar`, silx `CompareImagesStatusBar`). Remaining true gaps concentrate in CompareImages' SIFT/auto-alignment tooling — SIFT keypoint detection + visibility toggle and affine-transform tracking, which silx populates only from the SIFT path (`getTransformation` is `None` for origin/center/stretch); all need a heavy computer-vision dependency — plus the interactive UI residuals on ScatterView's scatter-profile and StackView's full 3D-profile toolbar (◐, extraction cores done), and ImageStack's URL-backed lazy loading / prefetch (file-IO, out of scope).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | M | L | ImageView: colorbar display | `ImageView.py:501` | `ImageView::colorbar()` (silx `getColorBarWidget`) builds a side `ColorBarWidget` from the active colormap; `ImageView::show` renders it (`high_level.rs:8295`) at the image's right (silx grid (0,2)). Visibility via `set_show_colorbar`; tested (`:11467`). |
| ✅ | M | L | ScatterView: colorbar widget | `ScatterView.py:83-88` | `ScatterView::colorbar() -> Option<ColorBarWidget>` (silx `getColorBarWidget`, `None` when no value colormap); `ScatterView::show` renders it to the right (`high_level.rs:9588`). Tracks the scatter value limits; tested (`:11484`). |
| ✅ | M | L | StackView: perspective selection (axis selection for 3D browsing) | `StackView.py:364-397` | `StackPerspective` + `perspective_ui` combo browses dimension 0, 1, or 2; `set_volume`+`stack_frame` auto-transpose per perspective |
| ✅ | M | M | ImageView: show/hide side histograms | `ImageView.py:552-559` | `is_side_histogram_displayed()` / `set_side_histogram_displayed(show)` (silx `isSideHistogramDisplayed`/`setSideHistogramDisplayed`); when hidden, `ImageView::show` (`high_level.rs:8228`) reserves no top/right strip or radar and the image reclaims that space. |
| ✅ | M | M | ImageView: valueChanged signal (pixel/histogram hover) | `ImageView.py:381-390,585-646` | `ImageView::value_changed() -> Option<(f64,f64,f64)>` (silx `valueChanged` `(col,row,value)`), computed by `image_value_at` from `self.cursor` (updated each frame in `show` at `high_level.rs:8305`); `None` off-image / before any pointer move (silx emits nothing then). Tested (`:10759`). |
| ✅ | M | M | ScatterView: position info panel (X, Y, Data, Index) | `ScatterView.py:90-101` | `show_position_info` shows X/Y/Data/Index, snapping X/Y/value/index to the picked scatter point (pixel-space pick, "-" when none) |
| ✅ | M | M | StackView: 3D transposition and dimension swapping | `StackView.py:409-441` | `stack_frame` re-slices the volume per perspective (silx transpose (1,0,2)/(2,0,1)); `set_perspective` rebuilds frames + axis labels |
| ✅ | M | M | CompareImages: vertical line separator (vline mode) | `CompareImages.py:124-133,397-407` | `CompareMode::HalfHalf` (silx `VisualizationMode.VERTICAL_LINE`): a draggable left/right split at the `split` fraction — A left of the separator, B right (`high_level.rs:7097`, render `:7297`). |
| ✅ | M | M | CompareImages: horizontal line separator (hline mode) | `CompareImages.py:135-144,397-407` | `CompareMode::SplitHorizontal` (silx `VisualizationMode.HORIZONTAL_LINE`): a draggable top/bottom split at the `split` fraction — A above the separator, B below (render `high_level.rs:7303`). |
| ✅ | L | L | ImageView: RadarView (position overview widget) | `ImageView.py:486-490,494-500` | `RadarView` mini-map: ImageView wires `set_data_bounds`/`set_viewport_limits` and renders the draggable viewport rect via `RadarView::ui`, dragging back through `set_limits` (silx full-range + current-viewport overview). |
| ✅ | L | L | ImageView: profile toolbar integration | `ImageView.py:451-453,692-697` | `show_profile_toolbar` (`ProfileMode` None/Horizontal/Vertical/Line/Rectangle) drives interactive profile extraction (`horizontal/vertical/line/rect_profile_values`), with profile curves added via `add_horizontal/vertical_profile_curve` (silx `ProfileToolBar`). |
| ✅ | L | L | ScatterView: mask tools widget (ScatterMaskToolsWidget) | `ScatterView.py:116-122` | `ScatterMaskWidget` (the silx `ScatterMaskToolsWidget` equivalent): a 1D per-point u8 mask edited by disk stencil (`update_disk`) or polygon (`update_stencil` + `point_in_polygon`) over scatter data. |
| ◐ | L | L | ScatterView: scatter profile toolbar | `ScatterView.py:141,306` | Extraction wired: `core::scatter_viz::scatter_line_profile` + `ScatterView::line_profile(start, end, n_points)` port silx `ScatterProfileToolBar::_computeProfile` (`tools/profile/rois.py:737-762`) — `numpy.linspace` samples interpolated through the scatter's Delaunay mesh (`LinearNDInterpolator` via `delaunay` + `interpolate`), `None` outside the convex hull (silx `NaN`), returning a `ScatterLineProfile`. Pure extraction headlessly tested (affine field exact along the line, outside-hull all-None, <3-points all-None). Still no `ScatterProfileToolBar` line-ROI tool or side profile-plot display (GPU/UI). |
| ◐ | L | L | StackView: profile 3D toolbar (extract profiles over all frames) | `StackView.py:84,948-951` | Extraction wired (row 222): `StackView::stack_aligned_profile`/`stack_line_profile` project a profile across all frames of the loaded volume via the row-253 cores. Still no `Profile3DToolBar` buttons or side-plot display of the stacked profile (GPU/UI). |
| ✅ | L | L | StackView: calibration support (per-axis scale/origin) | `StackView.py:551,565-566` | `core::Calibration` {None, Linear{constant,slope}} ports the affine subset of `silx.math.calibration` (`get_slope`/`is_affine`/`__call__`); `StackView::set_calibrations([Calibration;3])`/`calibrations()`/`calibrations_axes()`/`image_z()` mirror `getCalibrations(order=array/axes)` + `_getImageOrigin/Scale/Z`. The displayed image add applies the calibrated origin (`calib(0)`) + scale (slope) via `add_image_with_geometry`. Pure axes-ordering + geometry + Z headlessly tested; GPU image placement unverified |
| ✅ | L | L | CompareImages: composite RGB modes (red-blue-gray channels) | `tools/compare/core.py:60-61` | `CompareMode::RedBlueGray`/`RedBlueGrayNeg` (silx `COMPOSITE_RED_BLUE_GRAY`/`_NEG`): pure `red_blue_gray_composite` normalises A and B through the shared colormap to 0..=255 and lays them out R=a, G=a/2+b/2, B=b (NEG inverts each channel), faithful to `__composeRgbImage` (CompareImages.py:744-751). Toolbar buttons + `build_composite` arms wired; channel layout headlessly tested. GPU render is the shared CompareImages shim. |
| ◐ | L | L | CompareImages: alignment modes (origin/center/stretch/auto) | `tools/compare/core.py:66-72` | `CompareAlignment` {Origin,Center,Stretch} (silx `AlignmentMode`): `set_images` now takes separate `(w,h)` shapes for A and B, and the pure `align_compare_images` places them on a common grid per mode — Origin/Center zero-pad to `max×max` via `margin_image` (top-left, or silx `size//2−shape//2` centering), Stretch keeps A and bilinearly resamples B to A's shape via `rescale_array` (silx `__rescaleArray`/`BilinearImage.c_funct`, corner-aligned, edge-clamped). `raw_pixel_data` remaps the cursor per mode (`compare_aligned_coords`). Toolbar alignment selector wired. 8 headless tests (margin origin/center, rescale identity/upscale, align per mode, coord remap). AUTO defers with SIFT (row 1284). GPU render is the shared CompareImages shim. |
| ☐ | L | L | CompareImages: SIFT keypoint detection and alignment | `CompareImages.py:45,350,517-518` | No keypoint detection; silx detects SIFT features, shows them as scatter overlay, computes affine transform |
| ✅ | L | L | ComplexImageView: complex-valued image display | `ComplexImageView.py:259-310` | `ComplexImageView` renders 2D complex data via `ComplexMode` — Absolute/SquareAmplitude/Phase/Real/Imaginary/Log10Amplitude scalar maps + AmplitudePhase/Log10AmplitudePhase HSV-RGBA composites (the full silx set). |
| ☐ | L | L | ImageStack: lazy frame loading from URLs | `ImageStack.py:148-200` | No ImageStack widget; silx loads HDF5/image URLs on demand with threading, prefetch queue, and progress overlay |
| ◐ | L | L | ImageStack: URL selection table and browser | `ImageStack.py:65-128,337-392` | The selectable frame table + browser (slider + per-row toggleable visibility) is implemented (silx `_ToggleableUrlSelectionTable` equivalent, `image_stack.rs`). Residual: frames are held in memory — URL-backed add/remove (`silx.io` `DataUrl`) is out of scope (file-IO). |
| ☐ | L | L | ImageStack: prefetch queue for smooth browsing | `ImageStack.py:251-293` | No prefetch mechanism; silx preloads next N frames in background threads |
| ✅ | L | M | ImageView: getHistogram() API | `ImageView.py:699-725` | `ImageView::active_image_histogram` returns the active image's intensity histogram (data + extent), plus `show_pixel_histogram`/`show_pixel_histogram_toolbar` and a toggleable side histogram (silx `getHistogram`). |
| ✅ | L | M | ImageView: profile window behavior (popup/embedded) | `ImageView.py:392-401,656-690` | Profiles render embedded inline via `show_profile_toolbar` (`ProfileMode` H/V/Line/Rectangle) and in a popup via the standalone `ProfileWindow` widget (silx embedded-vs-popup behavior). |
| ✅ | L | M | ImageView: aggregation mode action (for multi-band images) | `ImageView.py:434-438,529-548` | `ImageView::aggregation`/`set_aggregation` + a None/Max/Mean/Min `selectable_value` menu drive `AggregationMode` + GPU `aggregate_blocks` (silx `AggregationModeAction`). |
| ✅ | L | M | ScatterView: getSelectionMask() and setSelectionMask() | `ScatterView.py:412-418` | `selection_mask`/`set_selection_mask` give programmatic per-point selection (u8 levels); set is length-validated + history-committed |
| ✅ | L | M | StackView: frame number / dimension labels | `StackView.py:799-827` | `set_dimension_labels`/`dimension_labels` (silx setLabels/getLabels); custom labels for 3D dims rotate onto X/Y axes per perspective |
| ✅ | L | M | StackView: aggregation mode for multi-band frames | `StackView.py:301-305` | `StackView::aggregation`/`set_aggregation` + a None/Max/Mean/Min combo and per-axis `bx`/`by` block factors in `show_frame_controls` (silx `AggregationModeAction` → `_stackItem.setAggregationMode`); each displayed frame routes through an `ImageSpec` carrying the calibrated origin/scale + `aggregation`/`aggregation_block`, driving the GPU `aggregate_blocks` reducer. silx's `Aggregation` enum is exactly NONE/MAX/MEAN/MIN (the prior "mean/sum" note was stale — there is no SUM) and siplot matches it. Reuses the tested `aggregate_blocks` primitive; StackView needs a wgpu RenderState no test builds, so the wiring is GPU-unverified (parity with ImageView row 1291). |
| ☐ | L | M | CompareImages: affine transformation tracking | `CompareImages.py:880-889` | No getTransformation() API. silx returns the AffineTransformation (tx,ty,sx,sy,rot) ONLY from the SIFT/AUTO path — `__updateData` sets `self.__transformation = None` and it is reassigned only in `__createSiftData`, so getTransformation is `None` for origin/center/stretch. It is therefore coupled to SIFT (rows 1284/1297) and defers with it; there is no standalone pure affine for the non-SIFT modes. |
| ✅ | L | M | ComplexImageView: amplitude range dialog (max / delta in log10) | `ComplexImageView.py:50-155` | `ComplexImageView::show_amplitude_range_controls` + `set_amplitude_range_info`/`amplitude_range_info` adjust the displayed `(max_amplitude, delta)` (silx `_AmplitudeRangeDialog`: displayed_max + log10 dlogs) for the Log10AmplitudePhase composite. |
| ☐ | L | S | CompareImages: keypoint visibility toggle | `CompareImages.py:346-359` | No setKeypointsVisible() API; silx renders SIFT keypoints on top with separate colormap |
| ✅ | L | S | CompareImages: status bar with coordinate/value info | `CompareImages.py:191-193` | `CompareImages::show_status_bar` (silx `CompareImagesStatusBar`): the cursor data coordinate (tracked from the pointer event in `show`) plus the raw A / B pixel value under it via `CompareImages::raw_pixel_data` — which remaps the display coordinate to each raw image per the alignment mode (`compare_aligned_coords`) before the bounds-checked `compare_pixel_at` lookup (silx `getRawPixelData`) — formatted as silx `_formatData` `%f`. silx's affine-transform label is omitted: silx populates it only from the SIFT path (`getTransformation` is `None` for the origin/center/stretch modes siplot supports — rows 1283/1295). Lookup + format core tested; status-bar render GPU/UI-unverified. |
| ✅ | L | S | ComplexImageView: complex display mode toolbar | `ComplexImageView.py:157-256` | `ComplexImageView::show_mode_toolbar`/`show_mode_controls` present a `ComplexMode` selector (silx `_ComplexDataToolButton` dropdown: amplitude/phase/real/imag/...). |
| ✅ | L | S | ImageStack: waiting/loading overlay | `ImageStack.py:43` | A centred "waiting" spinner overlay is shown when the current frame slot is empty/`None` or hidden (silx `WaitingOverlay`, `image_stack.rs` module doc :20-23). |

## Stats, Legends, Profile, Fit, Position-Info, Print, Selection Dialogs  — 20✅ 0◐ 1☐

siplot implements a full stats engine (min/max/delta/mean/sum/center-of-mass/coord-min/coord-max under `All`/`OnLimits`/`for_curve_roi`/`for_image` scopes), the `StatsWidget` table UI, legend display with single-click row selection, eye-icon visibility toggles, and a right-click context menu (Wave 9), profile extraction helpers for horizontal/vertical/line/rect/cross ROI types (incl. profile-over-stack) with automatic window placement mirroring silx, a fit widget spanning linear/Gaussian/GaussianArea/Lorentzian/PseudoVoigt/multi-Gaussian models with fit-range UI and a results table (value ± error, reduced chi-square), a `PositionInfo` readout bar with custom converters, the `ItemsSelectionDialog`, and a limits dialog for axis control. Remaining gaps are narrow: the RadarView standalone-widget conflict (code has `RadarView::ui`, the live re-baseline still flags it — needs adjudication), and print preview (needs a printer/PDF path).

| | P | E | Feature | silx | gap |
|---|---|---|---|---|---|
| ✅ | H | M | Stats engine: min/max/mean computation | `stats/stats.py:783-814` | `Stats` (stats.rs) computes the full silx `DEFAULT_STATS` set with finite-value filtering: min, max, delta (`max - min`), mean, sum, center-of-mass (`StatCOM`), and coord-min/coord-max (first argmin/argmax mapped to data coords). For curve and image scalars. Tested (`for_curve`/`for_image` cases). |
| ✅ | M | L | Stats context framework (data masking, bounds clipping, ROI filtering) | `stats/stats.py:143-600` | Bounds clipping (`StatScope::{All, OnLimits{x_range,y_range}}` + `Stats::for_curve`/`for_image`) AND ROI filtering (`Stats::for_curve_roi(xs, ys, from, to)`, plus arbitrary-shape per-ROI stats via `roi_stats`/`image_roi_stats`) cover silx `_StatsContext` in full. Verified against source: `_StatsContext`'s mask is ONLY the `onlimits` viewport rectangle (`_ImageContext`/`_CurveContext.clipData`: `int((v-origin)/scale)` index clip) or the `~roi.contains(points)` per-pixel ROI mask — there is **no** separate per-pixel user-mask kwarg (the prior "mask kwarg" note misread `numpy.ma.array(data, mask=clip_mask)`, where `clip_mask` is exactly that onlimits/roi clip). siplot mirrors both: `OnLimits` reproduces the index-window clip and `roi.contains` the per-pixel ROI mask. |
| ✅ | M | L | StatsWidget UI (scrollable table of stat rows, per-item, update mode toggle) | `StatsWidget.py:200-700` | `StatsWidget::ui` (`stats_widget.rs:175`): `ScrollArea::both` table, one row per `(label, item)` input, columns min/coords-min/max/coords-max/COM/mean/sum/delta (silx `DEFAULT_STATS` order) with `format_stat`/`format_coord` formatters, an Auto/Manual update-mode toggle (silx `UpdateMode`) + Manual "Update" button, and a "Visible data only" toggle. Active-vs-all live binding is tracked separately (rows 246/247). |
| ✅ | M | L | Stats: ROI-scoped statistics (compute stats within a selected ROI) | `stats/stats.py:68-140; ROIStatsWidget.py` | `Stats::for_curve_roi(xs, ys, from, to)` computes the full stat set over a curve x-range ROI; `roi_stats` (`image_roi_stats`/`curve_roi_stats`) computes per-ROI stats for arbitrary ROI shapes (image + curve), surfaced by `RoiStatsWidget` (rows above). Tested. |
| ✅ Done | M | M | Profile: line width / averaging method (mean vs sum) | `tools/profile/rois.py: _DefaultImageProfileRoiMixIn line_width, method properties` | `340b80c`: `ProfileWindow` exposes a Width DragValue + Mean/Sum ComboBox feeding the `line_profile_band`/`aligned_profile_values`/`rect_profile_values` extractors (band of N px, mean or sum). No longer mean-only. Window UI GPU-unverified |
| ✅ | M | M | Fit: gaussian fit (amplitude, center, width, background) | `silx.math.fit.fittheories: GaussianArea, etc.` | Both paths present: analytical `GaussianEstimateFit` AND iterative least-squares (`core::fitting::IterativeFit`, Levenberg-Marquardt → `IterativeFitResult` with covariance/chi-square). FitWidget model selector (`fit_widget.rs:366`) offers `IterativeGaussian`/`GaussianArea`/`Lorentzian`/`PseudoVoigt` (`PeakModel`), result via `iterative_result()`. |
| ✅ | M | M | Stats: on-limits computation (mask data to current X/Y viewport range) | `stats/stats.py:216-300; StatsWidget with onLimits checkbox` | `StatsWidget::on_visible_data` / `set_on_visible_data` (silx `setStatsOnVisibleData`) + "Visible data only" toggle; when on, `recompute` builds `StatScope::OnLimits { x_range, y_range }` from the passed viewport and `Stats::for_curve`/`for_image` clip to it. Tested (`recompute_on_visible_data_clips`). |
| ✅ | M | S | Fit: fit range selection UI (xmin, xmax for fitting region) | `silx.gui.fit: FitWidget with xmin/xmax input fields` | FitWidget `show` (`fit_widget.rs:383-401`): a checkbox toggles whole-curve vs a restricted `[xmin, xmax]` window, two `DragValue`s edit the bounds (seeded from the data's finite x extent via `default_fit_range`). Only points inside the window are fitted (`in_range_points`, public `set_fit_range`). |
| ✅ | L | L | Profile: profile-over-stack (slice extraction across stack dimension) | `tools/profile/rois.py:1058-1165 (ProfileImageStack* classes)` | `high_level::StackProfile` + `stack_aligned_profile`/`stack_line_profile` (row 253): one 1D profile per frame stacked row-major into `(frame_count, profile_len)`, via the `stack_profile_with` core over `stack_frame`. Mirrors silx `ProfileImageStack{HorizontalLine,VerticalLine,Line}ROI`. Extraction headlessly tested; profile-window curve display GPU-unverified. |
| ✅ | L | L | RadarView: miniature overview of full data extent with draggable viewport rect | `tools/RadarView.py:139-300` | `RadarView` (radar_view.rs) ports silx `RadarView.py:139-360`: a thumbnail of the data extent (gray fill/white outline) with a draggable blue viewport rect; `RadarView::ui` paints it and emits the dragged limits. Fully bound in `ImageView` (data extent via `set_data_bounds`, viewport via `set_viewport_limits`, drag → `Plot2D::set_limits`). Geometry (mapping/clamp/hit-test) unit-tested; paint GPU-unverified. |
| ☐ | L | L | Print preview: send plot to printable page with movable/resizable rect | `PrintPreviewToolButton.py:24-120` | No print preview or print button. Would require integrating with system print dialog and rendering to PDF/printer. |
| ✅ | L | M | Legend context menu (set active, map-to-Y-left/right, toggle points/lines, rename, remove) | `LegendSelector.py:697-845 (LegendListContextMenu)` | Wave 9: right-click on a legend row pops an `egui::Response::context_menu`. Curve rows get Set Active / Map to Y Left / Map to Y Right / checkable Points / checkable Lines / Rename / Remove; non-curve rows get a Set Active / Rename / Remove subset. Points/Lines toggle losslessly via a UI-only restore cache; rename via an `egui::Window`. Backend was already primitively complete. **GPU/UI-UNVERIFIED:** the menu render, right-click interaction, and rename window need a GPU `RenderState` no crate test constructs; only the pure style transforms are unit-tested. silx's copy-color / toggle-colormap actions are not ported (egui has no equivalent clipboard color affordance here). |
| ✅ | L | M | Fit: Lorentzian fit (amplitude, center, width, background) | `silx.math.fit.fittheories: Lorentzian` | `PeakModel::Lorentzian` (`lorentzian_model`/`estimate_lorentzian`) fits via the iterative LM solver; offered by the FitWidget model selector. Numerics tested in `core::fitting`. |
| ✅ | L | M | Fit: Pseudo-Voigt fit (Gaussian + Lorentzian blend) | `silx.math.fit.fittheories: PseudoVoigt` | `PeakModel::PseudoVoigt` (`pseudo_voigt_model`/`estimate_pseudo_voigt`) fits via the iterative LM solver; offered by the FitWidget model selector. Numerics tested in `core::fitting`. |
| ✅ | L | M | PositionInfo readout bar (X, Y, plus custom converters) | `tools/PositionInfo.py:64-250` | `PositionInfo` (position_info.rs) holds an ordered list of `(label, converter)` pairs (X/Y defaults, a polar Radius/Angle pair, custom numeric converters formatted `%.7g`); `PositionInfo::ui(ui, cursor)` renders the readout bar. Converter logic tested; bar render GPU-unverified. |
| ✅ | L | M | ItemsSelectionDialog: multi-select table of plot items filtered by kind | `ItemsSelectionDialog.py:40-200; actions/fit.py:237-241` | `ItemsSelectionDialog` (Wave 6B-1) groups items by kind with a `KindsSelector`-style filter; `set_available_kinds` (silx `setAvailableKinds`) restricts offered kinds and `SelectionMode::Single/Multi` (silx `setItemsSelectionMode`) toggles single vs multi selection. Reused by the fit tool as a single-select curve+histogram picker (`examples/high_level_fit_widget.rs`, silx `_initFit`). Pure logic unit-tested; UI GPU-unverified. |
| ✅ | L | S | Profile: cross profile (horizontal + vertical lines from point) | `tools/profile/rois.py:663-700; _ProfileCrossROI` | `ProfileWindow::update_profile` over a `Roi::Cross` shows both the horizontal and vertical full profiles through the cross center as two simultaneous legended curves in one window (`profiles_for_roi` → `Vec<ProfileCurve>`, `curve_handles: Vec`). Faithful to `ProfileImageCrossROI`. Extraction tested; window GPU-unverified. |
| ✅ | L | S | Fit widget UI: show fit results table (chi-squared, parameters, errors) | `silx.gui.fit.FitWidget: results table with parameter names, values, errors` | FitWidget `show` renders a per-parameter `value ± error` column (`format_param_value_error`, error = covariance-diagonal sigma via `IterativeFitResult::std_errors`) and a "Reduced chi-square" row (`format_reduced_chisq`). Matches silx's FitWidget results table (which shows value/sigma, not a full covariance matrix). Formatters tested; table render GPU-unverified. |
| ✅ | L | S | Stats: center-of-mass (weighted sum of positions) | `stats/stats.py:881-910` | `Stats::com` (`ComCoord`): `COM = sum(pos·val)/sum(val)`, undefined when the finite-value sum is zero (silx NaN). Curve → `com[0]`; image → `(x, y)`. Tested. |
| ✅ | L | S | Stats: coordinate of min/max (argmin/argmax with axis lookup) | `stats/stats.py:841-878` | `Stats::coord_min`/`coord_max`: data coords of the first min/max (silx `StatCoordMin`/`StatCoordMax`, argmin/argmax to first extremum); image coords mapped `origin + scale·index`. Tested. |
| ✅ | L | S | Stats: integral (sum of all values, optionally weighted by axis) | `stats/stats.py via silx.math.combo` | `Stats::sum` (sum/integral of finite values). Tested. |
