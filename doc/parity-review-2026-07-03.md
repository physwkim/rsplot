# Parity review — 2026-07-03 (workspace round)

Codex-style upstream-parity audit round over the whole workspace:

- **siplot** ← silx `~/codes/silx/src/silx/gui/plot` + `plot3d` (+ `silx/math/fit`)
- **sidm** ← PyDM `~/codes/pydm/pydm`
- **adl2sidm** ← adl2pydm `~/codes/adl2pydm/adl2pydm` + MEDM C `~/codes/epics-extensions/medm/medm`

Baseline: all four roadmap queues were exhausted 2026-06-16/17
(`doc/parity-roadmap.md`, `doc/plot3d-parity-roadmap.md`,
`doc/pydm-parity-roadmap.md`, `doc/adl2sidm-parity-roadmap.md`).
Deltas since then: 0.4.1–0.4.2 plot interaction work (scroll-momentum
guard, `scroll_zoom` flag, context-menu Reset Zoom refit) and the
epics-rs 0.18→0.21 migration in sidm (working tree at audit time).

Round method: 5 parallel read-only sub-agents (A: plot interaction/view,
B: plot items/fit/stats, C: plot3d, D: sidm↔PyDM, E: adl2sidm↔adl2pydm+MEDM),
reference→Rust direction. Agent-local numbers were renumbered to the
contiguous R1-1..R1-40 below (A: 1–8, B: 9–16, C: 17–24, D: 25–32,
E: 33–40).

## Open Findings

### Category A — plot interaction, view state, zoom/pan (vs silx PlotInteraction/PlotWidget)

### R1-1: Mouse-drag pan and wheel zoom leave the right (y2) axis untouched

Severity: High

Rust: `src/widget/plot_widget.rs:1026-1067` — the drag-pan block (`interaction::pan(base, area, delta, ...)` → `commit`) and the wheel block (`interaction::zoom_about(base, factor, ...)` → `commit`) both feed `commit()` (`plot_widget.rs:1555-1573`), which writes only `plot.limits`; `plot.y2` is never read or written on these paths. Internally inconsistent: `arrow_pan` (`plot_widget.rs:1540-1546`) *does* pan y2, `actions::control::apply_zoom` (`actions/control.rs:142-146`) *does* scale y2, and `LimitsHistoryEntry` (`core/plot.rs:562`, `push_limits`/`zoom_back` at `:690-707`) snapshots/restores y2 — as if gestures moved it.

Reference: `silx/gui/plot/PlotInteraction.py:260-335` — `Pan.drag` computes its own y2 delta via `pixelToData(axis="right")` and shifts `y2Min/y2Max` in the same gesture; `_utils/panzoom.py:132-176` — `applyZoomToPlot` scales `y2Min/y2Max` about the wheel center mapped through the right axis.

Impact: on a dual-axis plot (curves bound to `YAxis::Right`), mouse pan and wheel zoom scroll/scale the left axis while right-axis curves stay pinned — the two families visually shear apart, where silx keeps them locked together. The `LimitsChanged` event then reports a stale y2 range for the gesture. (Box zoom being left-axis-only *is* recorded in roadmap row 1583 — so only pan and wheel are reported here.)

### R1-2: Wheel zoom ignores the per-axis zoom-enabled flags and the keep-aspect override

Severity: Medium

Rust: `src/widget/plot_widget.rs:1044-1067` — the wheel handler consults only `plot.scroll_zoom`; `plot.zoom_x_enabled()`/`zoom_y_enabled()` are applied solely at the box-zoom commit (`:1177-1182` via `constrain_zoom_axes`), and there unconditionally — no keep-aspect check.

Reference: `silx/gui/plot/PlotInteraction.py:1894-1913` — `_onWheel` builds `enabledAxes` (all-enabled when `isKeepDataAspectRatio()`, else `getZoomEnabledAxes()`), returns without zooming when `enabledAxes.isDisabled()`, and passes them into `applyZoomToPlot` so a disabled axis keeps its range on wheel zoom too. For box zoom, `_getAxesExtent` (`PlotInteraction.py:390-397`) applies the disabled-axes substitution only `if ... not self.plot.isKeepDataAspectRatio()`.

Impact: unchecking "Zoom axes: Y" stops box zoom from changing Y but wheel zoom still zooms both axes — the flag is honored by one zoom gesture and ignored by the other. Conversely, with keep-aspect on, siplot still constrains the box zoom to the enabled axes, which silx explicitly overrides to preserve the ratio.

### R1-3: Context-menu Reset Zoom adopts the raw `(v, v)` cached range with no `checkAxisLimits` repair — NaN view on single-point data

Severity: Medium

Rust: `src/core/plot.rs:1049-1107` — `reset_zoom_to_data_range` assigns the refit range directly (`self.limits = (x_min, x_max, y_min, y_max)`) with no degenerate-span repair or float32 clamp. The context-menu "Reset Zoom" (`plot_widget.rs:1407-1410`, the 8a0264e churn) calls `Plot::reset_zoom()`, which consumes the live cache populated by `raw_data_range_from_bounds` (`high_level.rs:1793-1799`) — deliberately unpadded, a single point reads `(v, v)` (test `high_level.rs:13672`). With default zero `DataMargins` the limits become degenerate; `Transform` requires `max > min` (`core/transform.rs:65`), so the ortho matrix and pointer mapping go NaN. The widget-level reset path repairs via `Bounds1D::as_non_degenerate` (`high_level.rs:1695-1702`), so the two reset verbs disagree on degenerate data — and even that repair's constants (`pad = max(0.05·|v|, 0.5)`) differ from silx's.

Reference: `silx/gui/plot/PlotWidget.py:3308-3345` — `_forceResetZoom` funnels through `setLimits`, whose first step is per-axis `_checkLimits` (`PlotWidget.py:2705-2712`) → `checkAxisLimits` (`_utils/panzoom.py:49-75`): `vmax == vmin` is expanded (`0 → (-0.1, 0.1)`, `v>0 → (0.9v, 1.1v)`, `v<0 → (1.1v, 0.9v)`) and both bounds are clamped into `±1e37`.

Impact: right-click → Reset Zoom on a plot whose data is a single point (or all-equal on one axis) collapses that axis to a zero span and blanks the render, where silx shows a ±10% window. Bounds beyond ±1e37 are likewise adopted unclamped on this path. Churn residue: `plot.home_limits` is now write-only (`plot_widget.rs:308` is the only writer, no reader since 8a0264e) and the comment above it (`:305-306`) is stale.

### R1-4: Axis-state toggles (log scale, keep-aspect) miss silx's immediate refit

Severity: Medium

Rust: `src/render/backend_wgpu.rs:714-732` — `set_x_log`/`set_y_log`/`set_keep_data_aspect_ratio` only flip `plot.x_scale`/`y_scale`/`keep_aspect`; the toolbar toggles (`high_level.rs:5978-5990` log, `:6043-6049` aspect) call them with no limit repair or refit. The log-force rule in `reset_zoom_to_data_range` (`core/plot.rs:1053-1059`) only helps once some later reset runs.

Reference: `silx/gui/plot/items/axis.py:398-421` (X) and `:463-484` (Y) — `_internalSetScale` on switching to LOGARITHMIC with `vmin <= 0` immediately calls `setLimits(dataRange[0], vmax)` / `setLimits(*dataRange)` / `setLimits(1.0, 100.0)`. `silx/gui/plot/PlotWidget.py:2958-2969` — `setKeepDataAspectRatio` calls `_forceResetZoom()` and emits `notify("setKeepDataAspectRatio", state=flag)`.

Impact: toggling X/Y log while the view includes non-positive values leaves a `Log10` axis with `min <= 0` — `Transform` (precondition `min > 0`, `transform.rs:27/65`) produces NaN mapping, so the plot renders broken until content changes or the user resets; silx snaps to the positive data range at toggle time. Toggling keep-aspect keeps the current view in siplot while silx refits to full data on every toggle, and no siplot event mirrors silx's notify.

### R1-5: `_forceResetZoom` cross-axis defaults missing — y2-only plots never refit, empty plots don't get (1, 100)

Severity: Medium

Rust: `src/widget/high_level.rs:7531-7535` — `apply_limits_from_data_bounds` early-returns when `data_bounds.x` **or** `data_bounds.y_left` is `None`, so a plot whose curves are all on `YAxis::Right` (x bounds present, left-y absent) never refits — not on add/clear autoscale, not on toolbar/high-level `reset_zoom`. The core path (`core/plot.rs:1067-1080`) leaves any axis with `None` data untouched (test `reset_zoom_autoscale_on_axis_with_no_data_is_preserved`, `plot.rs:1517`), so left-y is also never mirrored from the right range and an empty plot's reset is a no-op.

Reference: `silx/gui/plot/PlotWidget.py:3326-3335` — `_forceResetZoom`: `xmin, xmax = (1.0, 100.0) if ranges.x is None`; same for y; `ranges.yright is None → y2 := (ymin, ymax)`; and `ranges.y is None` with yright present → the **left** axis adopts `ranges.yright`.

Impact: for right-axis-only plots silx resets X from data and shows the yright range on both Y axes; siplot stays at the initial `(0, 1)` limits on every axis. An itemless plot's Reset Zoom is a no-op instead of silx's `(1, 100)`/`(1, 100)` home view.

### R1-6: Box-zoom acceptance threshold diverges — zero-height/width drags are accepted and the collapsed axis is repaired into a ±10% band

Severity: Low

Rust: `src/widget/plot_widget.rs:1164-1184` — the box-zoom commit gates on the drag diagonal `(start - end).length() > 4.0`, then `commit` (`:1555-1573`) runs `clamp_limits` **before** `is_valid`, and `clamp_axis_limits` (`interaction.rs:284-296`) repairs a degenerate axis (`v>0 → (0.9v, 1.1v)` etc.), so the candidate passes validation.

Reference: `silx/gui/plot/PlotInteraction.py:363, 490-498` — `Zoom.SURFACE_THRESHOLD = 5`; `endDrag` zooms only when `abs(x0-x1) * abs(y0-y1) >= 5` (pixel *area*), so any zero-height or zero-width drag is rejected outright.

Impact: a purely horizontal drag of e.g. 20 px in zoom mode does nothing in silx; in siplot it zooms X to the dragged span and collapses Y to a ±10% band around the drag row. The gesture-rejection contract is not honored.

### R1-7: Limits-history lifecycle inverted — never cleared on zoom-mode entry, but cleared by Reset Zoom; wheel pushes one entry per smooth-scroll frame

Severity: Low

Rust: `src/widget/high_level.rs:3522-3524` — `set_interaction_mode` only assigns the mode; no path clears `limits_history` on entering Zoom mode. The context-menu Reset Zoom (`plot_widget.rs:1407-1410`) calls `plot.clear_limits_history()` — a clear silx does not perform. The wheel handler (`plot_widget.rs:1063`) calls `plot.push_limits()` on every frame with non-zero `smooth_scroll_delta`, so one macOS trackpad flick pushes dozens of entries.

Reference: `silx/gui/plot/PlotInteraction.py:365-370` — `Zoom.__init__` runs `self.plot.getLimitsHistory().clear()` every time zoom mode is entered; `LimitsHistory.push` is called only from `Zoom._zoom` (`:475-478`, the box-zoom commit) — never from the wheel path; `actions/control.py` `ResetZoomAction` only calls `resetZoom()` without touching the history.

Impact: silx's Zoom Back steps back through discrete box-zooms of the current zoom session; siplot's Zoom Back after wheel activity pops one *frame* of a smooth-scroll gesture (effectively a no-op), carries stale entries across mode switches, and loses the whole stack when the user picks Reset Zoom from the context menu (silx keeps it). The roadmap row (line 1391) records pushing "before each zoom/box-zoom/pan" but not the per-frame granularity, the missing clear-on-mode-entry, or the extra clear-on-reset.

### R1-8: Wheel zoom factor is a pixel-proportional exponential, not silx's fixed 1.1 per wheel step

Severity: Low

Rust: `src/widget/interaction.rs:236-241` — `wheel_zoom_factor(scroll_y) = exp(-(scroll_y) * 0.0015)`; the zoom magnitude scales with the delivered pixel delta (a typical 50 px notch gives ≈1.078× per notch, and OS scroll acceleration changes it).

Reference: `silx/gui/plot/PlotInteraction.py:1912-1913` — `scale = 1.1 if angle > 0 else 1.0 / 1.1`; the magnitude of the wheel delta is ignored, every step is exactly 1.1×.

Impact: per-step zoom rate diverges from the silx contract and is platform/acceleration dependent; N notches give `exp(-0.0015·Σpx)` instead of `1.1^N`. Not recorded anywhere in the roadmap. If the smooth-trackpad behavior is a deliberate egui-ism, it needs a scope-decision entry.

### Category B — plot items, colormap, fit, stats (vs silx items / silx.math.fit)

### R1-9: Colormap autoscale ignores the normalization — log/sqrt/arcsinh autoscale uses linear-normalization semantics

Severity: High

Rust: `src/core/colormap.rs:872-950` — `AutoscaleMode::range(data, percentiles)` does not take the colormap's normalization; it always uses finite min/max, data-space mean±3σ, and `DEFAULT_RANGE = (0.0, 1.0)` (`:870`). Its own doc comment says it mirrors only "the *linear-normalization* autoscale". Every caller (`src/widget/high_level.rs:2015-2021` `autoscaled_colormap`, `src/widget/colormap_dialog.rs:341-343` `autoscale_range`, the six `autoscale_colormap` sites in `src/render/scene3d_items.rs`) feeds raw pixels regardless of `Colormap::normalization`.

Reference: `silx/gui/colors.py:682-692` — `_computeAutoscaleRange` dispatches to `self._getNormalizer().autoscale(...)`, i.e. autoscale is normalization-dependent: `silx/math/colormap.py:406-422` `LogarithmicNormalization` uses `min_positive` for minmax, `is_valid = value > 0` filtering for percentile (`:357-370`), and `DEFAULT_RANGE = (1, 10)`; `:313-340` computes stddev3 for log/sqrt/arcsinh in *normalized space* (`apply` → mean±3σ → `revert`), with the data-space variant reserved for linear/gamma (`:376-395`); sqrt filters `value >= 0` (`:434-436`).

Impact: for a log-normalized image whose data contains any value ≤ 0 (ubiquitous in counting data), silx autoscale yields `vmin = min_positive`; siplot yields `vmin ≤ 0`, so `norm_bounds()` (`colormap.rs:819-827`) sees `log10(vmin)` non-finite and returns `(0, 0)` — the whole image renders as the single low LUT color. Stddev3 and percentile bounds also differ numerically for every non-linear normalization, and the empty-data fallback is (0, 1) instead of (1, 10) under log. Highest-leverage fix: threading `Normalization` into `AutoscaleMode::range` closes the log-collapse, the stddev3-space error, the percentile validity filter, and the DEFAULT_RANGE fallback in one structural change.

### R1-10: `std` statistic missing from the "full DEFAULT_STATS" port

Severity: Medium

Rust: `src/core/stats.rs:55-81` — `Stats` carries min/max/delta/mean/sum/COM/coord-min/coord-max only; `src/widget/stats_widget.rs:228-237` — `STAT_COLUMNS` has 8 columns, none is `std`, while the comment claims it matches "silx `DEFAULT_STATS` order (StatsWidget.py:1266-1276)". The roadmap rows 1654/1656 likewise claim "the full silx `DEFAULT_STATS` set".

Reference: `silx/gui/plot/StatsWidget.py:1266-1276` — `DEFAULT_STATS = (StatMin, StatCoordMin, StatMax, StatCoordMax, StatCOM, ("mean", numpy.mean), ("std", numpy.std))`.

Impact: every `BasicStatsWidget` table in silx shows a standard-deviation column; siplot's stats table cannot show one at all (no accumulator for it), and the widget instead shows `sum`/`delta` columns silx's default table does not.

### R1-11: Histogram-item stats computed over the 2N step polyline, not N counts at bin anchors; no scatter stats context

Severity: Medium

Rust: `src/widget/high_level.rs:4037-4048` — `add_histogram` expands `(edges, counts)` via `histogram_step_values` and retains that 2N-point step curve as `RetainedItemData::Curve`; `feed_all_stats`/`feed_active_stats` (`:5513`, `:5547-5559`) then compute curve stats over the step points via `retained_data_to_stats_input` (`:1982-2003`, only `Curve`/`Image` arms exist).

Reference: `silx/gui/plot/stats/stats.py:376-414` — `_HistogramContext` computes over the raw `yData` (N counts) with `xData = item._revertComputeEdges(...)` (N bin anchors); `:425-498` — `_ScatterContext` computes stats over the scatter's *value* array with `(x, y)` axes; both kinds are in `BASIC_COMPATIBLE_KINDS` (`:741`).

Impact: for a histogram item, siplot's stats table reports `count = 2N`, `sum = 2·Σcounts`, and an edge-duplicated (shifted) mean/COM versus silx's N-point values — the sum is exactly doubled. Scatter value arrays never reach any stats path; silx computes the full stat set for scatter items.

### R1-12: Multi-Gaussian auto peak search uses sensitivity 3.5; silx FitManager estimation uses 2.5

Severity: Medium

Rust: `src/widget/fit_widget.rs:625-631` — the `FitModelChoice::MultiGaussian` dispatch calls `fit_multi_gaussian_full(&xs, &ys, guess_fwhm(&ys), DEFAULT_PEAK_SENSITIVITY, ...)` with `DEFAULT_PEAK_SENSITIVITY = 3.5` (`src/core/peaks.rs:15` — that constant is the *standalone* `peak_search` pyx default, which FitManager does not use).

Reference: `silx/math/fit/fittheories.py:107` — `DEFAULT_CONFIG["Sensitivity"] = 2.5`; `:338`/`:356` — `estimate_height_position_fwhm` passes `search_sens = float(self.config["Sensitivity"])` into the peak search that seeds the Gaussians theory.

Impact: peaks whose significance falls between 2.5σ and 3.5σ of the noise are found by silx's Gaussians-theory estimation but silently dropped by siplot's Multi-Gaussian fit — fewer seeded peaks, different fit result on noisy multi-peak data.

### R1-13: FitManager peak search's edge padding not ported — edge-adjacent peaks missed

Severity: Medium

Rust: `src/core/fitting.rs:2330` — `estimate_multi_gaussian` calls `crate::core::peaks::peak_search(y, ...)` directly on the raw array; no padding exists anywhere in `peaks.rs`/`fitting.rs`.

Reference: `silx/math/fit/fittheories.py:293-311` — `FitTheories.peak_search` pads `y` with `fwhm` copies of `y[0]` and `y[-1]` on each side, runs the C `seek` on the padded array, then re-maps indices (`peak_index - fwhm`) and keeps only in-range hits. This is the search the Gaussians/Lorentz/pvoigt estimators use.

Impact: the C `seek` state machine needs lead-in samples, so peaks within ~`fwhm` samples of either array edge are detected by silx but not by siplot's multi-Gaussian estimation — the fit seeds fewer peaks for spectra with edge peaks (a common case for truncated scans).

### R1-14: Step-up (and Atan) seed height — silx returns the rescaled derivative-peak height, Rust always uses max−min

Severity: Low

Rust: `src/core/fitting.rs:1777-1796` — `estimate_step` always seeds `height = max(y) − min(y)` (`data_amplitude`) and deliberately skips silx's derivative rescale, arguing it "leaves the argmax and half-maximum crossings … unchanged" (true for centre/fwhm only).

Reference: `silx/math/fit/fittheories.py:1130-1157` — `estimate_stepup` rescales the derivative so `max(y_deriv) = max(y)` (`:1133-1134`), then in the largest-peak loop replaces the height with `fittedpar[3*largest_index]` whenever it exceeds `data_amplitude` (`:1150-1157`). With the default no-strip config that fitted height ≈ `max(y)`, which exceeds `max(y) − min(y)` whenever `min(y) > 0`. (`estimate_stepdown:1019-1026` keeps `data_amplitude` — Rust matches stepdown but not stepup; the Atan theory also uses `estimate_stepup`, `:1466`.)

Impact: for any step-up/arctan data on a positive baseline, silx seeds `Height ≈ max(y)` while siplot seeds `max−min` — different LM starting point for the Step Up and Atan theories.

### R1-15: `estimate_slit` beamfwhm seed deliberately diverges from silx's formula (upstream index quirk corrected, unrecorded)

Severity: Low

Rust: `src/core/fitting.rs:1857-1862` — beamfwhm seeds as `0.5·(fwhm_up + fwhm_down)`, then clamps; the doc comment (`:1836-1838`) acknowledges silx "has an index typo that reads the down-step centre instead".

Reference: `silx/math/fit/fittheories.py:1076` — `beamfwhm = 0.5 * (largestup[2] + largestdown[1])`, i.e. up-step FWHM averaged with the down-step *centre position*, then the same min/max clamps (`:1077-1078`).

Impact: siplot's Slit-theory seed is numerically different from upstream for every dataset where `centre_down ≠ fwhm_down` (nearly always). The deviation is documented only in the code comment — not in `doc/parity-roadmap.md` — so it needs either a roadmap accepted-residual entry or a revert-to-upstream decision.

### R1-16: Default image colormap is viridis; silx's plot default is gray

Severity: Low

Rust: `src/widget/high_level.rs:3305` — `default_colormap: Colormap::viridis(0.0, 1.0)` (used by `add_image_default`/`try_add_image_default`, `:4118-4131`).

Reference: `silx/gui/plot/PlotWidget.py:3056-3062` — `setDefaultColormap(None)` builds `Colormap(name=silx.config.DEFAULT_COLORMAP_NAME, normalization="linear")` with `DEFAULT_COLORMAP_NAME = "gray"` (`silx/_config.py:58`).

Impact: every image added without an explicit colormap renders viridis in siplot vs gray in silx. Possibly a deliberate aesthetic choice, but no roadmap/decision entry records it.

### Category C — plot3d scene graph, picking, camera (vs silx.gui.plot3d)

### R1-17: Orbit/pan/zoom anchors ignore the geometry depth under the cursor

Severity: Medium

Rust: `src/widget/scene_widget.rs:188,202,217,235` — all three gestures anchor on the bounds centre: `OrbitDrag::begin(&self.camera, to_local(p), center)`, `PanDrag::begin(..., center)`, and wheel zoom uses `ndc_z = self.camera.matrix().transform_point(center, true).z`. `src/core/scene3d/interaction.rs:82-84` still justifies this with "with no picking yet" — stale since Phase 4 landed CPU picking.

Reference: `silx/gui/plot3d/scene/interaction.py:150-161` — `CameraSelectRotate.beginDrag` with `orbitAroundCenter=False` (the value `Plot3DWidget` passes for both 'rotate' and 'pan' modes, `Plot3DWidget.py:189-205`) uses the **picked object point** under the press as rotation centre, falling back to scene centre only on a miss; `interaction.py:226-235` — pan's plane depth is `_pickNdcZGL(x, y)` (depth under the cursor); `interaction.py:329-341` — wheel mode `"position"` un-projects the cursor at its own picked depth so the pixel under the mouse stays invariant.

Impact: rotation pivots around the scene centre even when the user grabs an off-centre object; pan tracks 1:1 only for content at centre depth; zoom-to-cursor keeps the wrong point invariant unless the target sits at centre depth. The CPU `SceneWidget::pick` (nearest-hit `ndc_depth`) is exactly the datum silx reads from the depth buffer, so the anchor can now be computed without GPU readback.

### R1-18: `DataItem3D` transform stack (translation / rotation+center / scale / matrix) has no Rust surface

Severity: Medium

Rust: `src/render/scene3d_items.rs` — no item exposes `set_translation`/`set_rotation`/`set_scale`/`set_matrix` (rg over `src/`: zero hits); every `append_to` (e.g. Scatter3D at :176, Mesh3D at :675) bakes raw data coordinates straight into the flat `Scene3dGeometry` channels, which carry no per-node matrices (`src/render/gpu_scene3d.rs:300-316`).

Reference: `silx/gui/plot3d/items/core.py:288-315` — every `DataItem3D` owns the composed stack `[translate, rotateFwd(center), rotate, rotateBwd, [matrix, scale]]`, with public `setScale`/`setTranslation`/`setRotationCenter` (incl. 'lower'/'center'/'upper' bbox-relative tags)/`setRotation(angle, axis)`/`setMatrix` (`core.py:335-485`). `ScalarFieldView.py:871-892` builds the flagship on the same model (`_dataScale`, `_dataTranslate`, `_dataTransform`, `_outerScale`) — this is how silx calibrates anisotropic voxel size and volume origin.

Impact: scenes that place/scale items (e.g. `ScalarFieldView.setScale` for non-cubic voxels, `setTranslation` for a real-world origin, per-item rotation) cannot be expressed; all items render in raw index/data space only. Picking likewise has no object-frame concept (silx converts the segment per item via `objectToSceneTransform`, `items/_pick.py:169-171`), which is consistent today only because no transform can ever be non-identity.

### R1-19: Scene chrome lacks `LabelledAxes` — no axis name labels, tick lines, or tick value labels (scene/text.py unported)

Severity: Medium

Rust: `src/render/gpu_scene3d.rs:526-576` — `add_bounding_box_with_axes` emits exactly 12 lines (3 RGB axes + 9 box edges); no billboard-text or tick machinery exists in `src/render/gpu_scene3d.rs`, `src/render/scene3d_items.rs`, `src/core/scene3d/`.

Reference: `silx/gui/plot3d/scene/axes.py:41-258` — the default root group of both widgets is a `LabelledAxes` (`SceneWidget.py:377` via `RootGroupWithAxesItem`; `ScalarFieldView.py:888` `self._bbox = axes.LabelledAxes()`): X/Y/Z `Text2D` name labels at the box face centres, dashed tick lines (`dash = 5, 10`) laid on the box planes from `ticklayout.ticks`, and a `Text2D` value label per tick, recoloured via `tickColor`/`SceneWidget.setTextColor`. `items/core.py:702-717` exposes `setAxesLabels`.

Impact: silx 3D views are self-annotating (numeric scale + axis names around the bounding box); siplot renders an unlabeled wireframe, and `setAxesLabels`/`setTextColor` have no analogue. This is the largest remaining visual gap in the default `SceneWidget`/`ScalarFieldView` frame, and it gates any future port of the 2D-text overlay family.

### R1-20: Orientation indicator (overview viewport) missing — silx shows it by default

Severity: Medium

Rust: `src/widget/scene_widget.rs:178-243` — `show()` paints exactly one scene (`paint_scene3d`) into the rect; there is no second viewport, and no `set_orientation_indicator_visible` API anywhere in `src/widget/`.

Reference: `silx/gui/plot3d/Plot3DWidget.py:51-93` — `_OverviewViewport`: a 100×100 px corner viewport drawing a half-transparent disc + RGB `Axes` scaled 2.5, whose camera listens to the main camera and re-poses at `-12 * direction` with the same orientation; `:159,165` constructed unconditionally and included in `_window.viewports` by default; `:325-336` `setOrientationIndicatorVisible`; `:387-388` docked top-right on resize.

Impact: the always-on orientation cue every silx 3D widget shows (which way is X/Y/Z while orbiting) is absent, and there is no toggle API. All the pieces exist in the port — it is a missing composition, not missing infrastructure.

### R1-21: Cut plane renders without its contour stroke

Severity: Medium

Rust: `src/render/scene3d_items.rs:2487-2498` — `ScalarField3D::append_to` emits the visible cut plane as a single `Scene3dTexturedMesh` (`build_cut_plane_mesh`, :2140-2220); nothing is added to the lines channel for the plane boundary.

Reference: `silx/gui/plot3d/scene/primitives.py:991-1056` — `PlaneInGroup` (base of `cutplane.CutPlane`, `scene/cutplane.py:235`) draws the plane/box intersection contour as a stroke: default colour `(1,1,1,1)`, width 2.0, `strokeVisible=True`; `ScalarFieldView.py:902-906` adds the `planeStroke` primitive to the outer bbox group and exposes `getStrokeColor`/`setStrokeColor` (`:555-570`).

Impact: whenever the cut plane is shown, silx frames the slice with a visible boundary line; siplot shows the naked textured polygon, so a slice through low-contrast data has no visual boundary, and there is no stroke colour/visibility API to port the `SFViewParamTree` "stroke" rows onto.

### R1-22: Pick negative space — Scatter2D LINES and image quads produce no hit at all

Severity: Low

Rust: `src/render/gpu_scene3d.rs:374-400` — `pick_triangles()` reads only `triangles` + `meshes`, `pick_points()` only `points`; the `lines` and `images` channels are unreachable by `SceneWidget::pick`. Scatter2D LINES emits solely into `lines` (`src/render/scene3d_items.rs:445-463`), image quads solely into `images`.

Reference: `silx/gui/plot3d/items/scatter.py:509-511` — Scatter2D in LINES mode is pickable (`_pickPoints` at its data points, 5 px threshold); `silx/gui/plot3d/items/image.py:55-84` — `ImageData/ImageRgba._pickFull` intersect the picking segment with the z=0 quad plane and return a position (plus row/column). Both types are in `PositionInfoWidget._SUPPORTED_ITEMS` (`tools/PositionInfoWidget.py:150-163`).

Impact: a Scatter2D switched to LINES visualization and any 3D image item become invisible to picking (no position, no depth — not merely missing index payloads). Boundary with the recorded residual: P1.3/PK4 record the *texel-index resolution* of an image quad hit as the remaining tail, which presupposes the quad hit itself exists; here the hit never occurs. The LINES gap is unrecorded entirely.

### R1-23: Viewport shading functions not carried — no fog, and the `ScalarFieldView` specular override (`shininess = 32`) is dropped

Severity: Low

Rust: `src/render/shaders/scene3d_mesh.wgsl:25-28,49-57` — lighting is baked as `AMBIENT 0.3 / DIFFUSE 0.7` constants with no specular path, and no shader anywhere has a fog term.

Reference: `silx/gui/plot3d/scene/viewport.py:227-233` — every viewport threads a `DirectionalLight` **and** a `Fog` program function into all fragment shaders; `Plot3DWidget.py:279-299` exposes `setFogMode(LINEAR)`; `function.py:263-275` — the light includes a specular term gated on `shininess > 0`; `ScalarFieldView.py:928` — the flagship sets `viewport.light.shininess = 32`, turning specular highlights **on** for exactly the widget `scalar_field_view.rs` ports.

Impact: linear depth-cue fog is unavailable (unrecorded — the roadmap's recorded lighting residual covers only "viewport defaults baked in / on-off API later"), and iso-surfaces in the ported `ScalarFieldView` render matte where silx renders them with a specular highlight — i.e. the flagship's upstream light state is not the "viewport defaults" the WGSL comment (lines 4-6) claims to reproduce.

### R1-24: Default style constants diverge from silx values

Severity: Low

Rust: `src/widget/scene_widget.rs:27-30` — `DEFAULT_BACKGROUND = Color32::from_gray(30)` ("as in silx's 3D views") and `DEFAULT_BOX_COLOR = Color32::from_gray(200)`.

Reference: `silx/gui/plot3d/Plot3DWidget.py:161` — default background is `(0.2, 0.2, 0.2, 1.0)` (grey 51, not 30); `SceneWidget.py:373-375` — foreground (bounding-box) and text colours default to white `(1.0, 1.0, 1.0, 1.0)`, matching `primitives.py:948` `BoxWithAxes(color=(1,1,1,1))`; `ScalarFieldView.py:875` — same white foreground.

Impact: siplot's scene clears noticeably darker than silx (30 vs 51 grey) and draws the bounding box light-grey (200) instead of silx's white — the doc comment's "as in silx" claim does not hold, and there is no `set_foreground_color`/`set_text_color` pair to restore the silx values per widget (only the background is settable).

### Category D — sidm channels, data plugins, widgets (vs PyDM)

### R1-25: `pva://` never publishes write access — every writable widget is permanently disabled over pvAccess

Severity: High

Rust: `sidm/src/data_plugins/epics_plugins/pva_plugin.rs:159-175` — the monitor callback sets only `s.connected = true` on `MonitorEvent::Connected`; no code path in the file ever writes `s.write_access` (the only backend write sites are `ca_plugin.rs:186`, `local_plugin.rs:85`, `fake_plugin.rs:180`). `ChannelState::default()` has `write_access: false` (`channel.rs:170`), and `widgets/base.rs:378-380` gates `enabled = state.connected && (!writable || state.write_access)`; all writable widgets pass `writable=true` (`line_edit.rs:131`, `slider.rs:113`, `push_button.rs:153`, `enum_combo_box.rs:92`).

Reference: `pydm/data_plugins/epics_plugins/p4p_plugin_component.py:233-237` — on first value after connect: `self.write_access_signal.emit(True)` ("no way to get the actual write access value from p4p, so defaulting to True"); repeated for late listeners at :448-449.

Impact: over `pva://`, SidmLineEdit/Slider/Spinbox/PushButton/EnumComboBox/EnumButton/DateTimeEdit/WaveformTable render permanently greyed-out; PyDM enables them. The live PUT tests pass only because `tests/pva_ioc.rs:100,145` write via engine-level `Channel::put`, bypassing the widget gate.

### R1-26: CA monitor mask drops `DBE_PROPERTY` — units/precision/limits/enum strings frozen at connect time

Severity: Medium

Rust: `sidm/src/data_plugins/epics_plugins/ca_plugin.rs:136` — `ch.subscribe()` resolves to `DBE_VALUE | DBE_LOG | DBE_ALARM` (`epics-ca-rs/src/client/mod.rs:2654-2662`); `apply_value` (`ca_plugin.rs:301-306`) applies only value/alarm/timestamp, and metadata is refetched solely in `on_connect` (first connect, reconnect, `NativeTypeChanged`, lines 164-195).

Reference: `pydm/data_plugins/epics_plugins/pyepics_plugin_component.py:59-64` — `auto_monitor = DBE_VALUE | DBE_ALARM | DBE_PROPERTY`; `update_ctrl_vars` (:120-177) re-emits precision/units/enum_strs/all six limits whenever a property event delivers a change.

Impact: a runtime `caput PV.PREC` / `.EGU` / `.HIGH` / mbbo-string change updates PyDM widgets live; sidm labels/spinboxes/scales keep the stale precision, units, limits and enum labels until a disconnect/reconnect cycle.

### R1-27: CA wire strings decoded UTF-8-lossy; PyDM decodes latin-1 — non-ASCII units/labels become U+FFFD

Severity: Medium

Rust: `sidm/src/data_plugins/epics_plugins/ca_plugin.rs:288` (units), `:312` (string values), `:361` + `lossy_strings` `:367-372` (string arrays, enum labels) — all through `PvString::as_str_lossy`, which is `String::from_utf8_lossy` (`epics-base-rs/src/types/pv_string.rs:56-57`), mapping any non-UTF-8 byte to U+FFFD.

Reference: `pydm/data_plugins/epics_plugins/pyepics_plugin_component.py:14-19` — `utils3.EPICS_STR_ENCODING = "latin-1"`: pyepics decodes every wire byte 1:1 into U+0080–U+00FF, so nothing is destroyed.

Impact: units/labels/string values written by IOCs in latin-1 — `µm` (0xB5), `Å` (0xC5), `°C` (0xB0), all common EGU strings at accelerators — render as `�m`/`�`/`�C` in sidm labels, spinbox suffixes and enum widgets where PyDM shows the intended glyphs. (The pva side is unaffected: pvAccess strings are UTF-8 by spec.)

### R1-28: `pva://` path component is appended to the PV name; PyDM treats it as a subfield selector

Severity: Medium

Rust: `sidm/src/data_plugins/epics_plugins/pva_plugin.rs:101` — `let pv = address.full_address();` with `full_address = netloc + path` (`address.rs:95-97`), so `pva://NAME/sub/field` searches for a channel literally named `NAME/sub/field`.

Reference: `pydm/data_plugins/plugin.py:262-266` — the monitor name is `get_address` = **netloc only** (passed at `plugin.py:291`, used at `p4p_plugin_component.py:78`); `get_subfield` (`plugin.py:269-280`) turns the `/path` into a list of keys drilled into the delivered structure (`p4p_plugin_component.py:262-284`). PyDM's pva grammar also has an RPC form (`pva://fn?arg=..&pydm_pollrate=..`, `p4p_plugin_component.py:200-209`) with no sidm counterpart.

Impact: any PyDM-style subfield address silently never connects (wrong channel name, permanent disconnected styling) instead of monitoring the base PV and selecting the subfield. Distinct from the recorded NTTable deferral: that covers the structured-table value model, not the address grammar; subfield selection also serves plain nested scalars. RPC addresses are likewise unsupported (unrecorded).

### R1-29: NTNDArray never yields a value — `PvField::Union` unhandled, so `pva://` images are dead

Severity: Medium

Rust: `sidm/src/data_plugins/epics_plugins/pva_plugin.rs:344-359` — `value_to_pv` matches Scalar/ScalarArray/ScalarArrayTyped/Structure(NTEnum) and falls to `_ => None` for `PvField::Union` — but an NTNDArray's `value` field is a union of typed arrays (`epics-pva-rs/src/pvdata/structure.rs:30-37`; the library even ships `nt/nd_array.rs`). `apply_ntscalar` (`:238-240`) then leaves `s.value` untouched on every event.

Reference: `pydm/data_plugins/epics_plugins/p4p_plugin_component.py:287-290` — ndarray values are emitted, with `NTNDArray` codec decompression via `pva_codec.decompress`.

Impact: SidmImageView pointed at an areaDetector `pva://` image (the standard PVA transport, `Pva1:Image`) never receives data — no value update at all, only connected styling — while PyDM displays it (including compressed codecs). The roadmap's P4/X3 image path is recorded only for `ca://ArrayData`; the pva union gap is unrecorded.

### R1-30: CA put path missing the write-access / read-only gate

Severity: Low

Rust: `sidm/src/data_plugins/epics_plugins/ca_plugin.rs:210-239` — the write branch checks only `connected_now` before `put_nowait`, despite `write_access` being tracked in state (`:185-187`).

Reference: `pydm/data_plugins/epics_plugins/pyepics_plugin_component.py:205-213` — `put_value` returns if `is_read_only()`, and only puts `if self.pv.write_access`; the p4p plugin gates on `is_read_only()` too (`p4p_plugin_component.py:409-411`).

Impact: with write access denied, sidm still emits `CA_PROTO_WRITE` and relies on the server's asynchronous rejection, where PyDM drops the write locally and never touches the wire; and sidm has no equivalent of PyDM's global read-only mode (`PYDM_READ_ONLY`) on any backend (also unenforced in `pva_plugin.rs:193-219`).

### R1-31: Value events posted for alarm-only / DBE_LOG / unchanged-value updates that PyDM suppresses

Severity: Low

Rust: `sidm/src/data_plugins/epics_plugins/ca_plugin.rs:197-205` — every monitor snapshot goes through `post_value` (the subscription additionally includes `DBE_LOG`, which PyDM does not request — see R1-26); `pva_plugin.rs:161-169` likewise posts on every `MonitorEvent::Data`. `channel.rs:390-396` documents "a repeated value still emits an event", citing PyDM's `receiveNewValue`-per-callback — but the reference dedups *before* that slot fires.

Reference: `pydm/data_plugins/epics_plugins/pyepics_plugin_component.py:102` — `if value is not None and not np.array_equal(value, self._value)` gates every `new_value_signal` emit, so an alarm-only (DBE_ALARM) callback re-emits severity but no value; `p4p_plugin_component.py:241-242` emits a value only when `"value"` is in the monitor's `changedSet()`.

Impact: SidmTimePlot (OnValueChange), SidmScatterPlot and SidmEventPlot append samples on alarm transitions, ADEL-gated DBE_LOG events, and pva metadata-only updates where PyDM curves append nothing — different point counts and visibly stepped duplicates on the same PV activity.

### R1-32: `loc://` missing `type=array`, the `unit`/`upper_limit`/`lower_limit`/`enum_string` extras, and float auto-precision

Severity: Low

Rust: `sidm/src/data_plugins/local_plugin.rs:70-103` — recognizes only `type` (float/int/bool/str), `init`, `precision|prec`; other keys silently ignored; no derived precision for floats. (The module doc defers arrays, but that deferral is not recorded in the roadmap, and the extras gap is documented nowhere.)

Reference: `pydm/data_plugins/local_plugin.py:28-32` — `type=array` (`ast.literal_eval` → `np.array`, plus numpy `dtype/order/...` kwargs) and extras `precision, unit, upper_limit, lower_limit, enum_string`; `:103-121` emits unit/ctrl-limits/enum strings; `:341-345` + `:384-388` — floats without explicit precision get `precision_for_value` (decimal-digit count, max 8) emitted on every value.

Impact: a `loc://` slider/scale gets no ctrl limits (SidmSlider disables itself without limits), enum widgets bound to `loc://...&enum_string=('A','B')` get no choices, waveform widgets cannot be driven from local arrays, and `loc://` float labels format with the default precision instead of PyDM's value-derived digits.

### Category E — adl2sidm parser/codegen/CALC (vs adl2pydm + MEDM C)

### R1-33: Visibility-gate `=`/`#` translate to evalexpr `==`/`!=`, which are type-strict — float channels never compare equal to integer literals

Severity: High

Rust: `adl2sidm/src/codegen.rs:437-453` — `medm_visibility_expr` emits `"A#0"` / `"A=0"`, `translate_calc_to_evalexpr` rewrites them to `A != 0` / `A == 0` with an integer literal `0`. `sidm/src/data_plugins/calc_plugin.rs:201-210` binds `PvValue::Float → Value::Float` (Enum→Int, Bool→Boolean). evalexpr-13.1.0 implements `Eq`/`Neq` as raw `arguments[0] == arguments[1]` over `#[derive(PartialEq)]` on the `Value` enum (`operator/mod.rs:302-311`, `value/mod.rs:21`), so `Value::Float(0.0) == Value::Int(0)` is **false** — cross-type operands are never equal. (Relational `>`/`<`/`>=`/`<=` DO coerce via `as_number()`; only Eq/Neq are strict.)

Reference: `medm/utils.c:4474-4477` — `IF_NOT_ZERO: return records[0]->value != 0.0`, `IF_ZERO: return records[0]->value == 0.0`; MEDM's whole CALC engine is double-typed (`calcPerform(valueArray…)`, `utils.c:4486-4508`). adl2pydm evaluates the rule in Python, where `0.0 == 0` is true.

Impact: for any DOUBLE-typed PV (every `ai`/analog channel), `vis="if zero"` widgets are **permanently hidden** (`A == 0` never true), and `vis="if not zero"` widgets **never hide at 0.0** (`A != 0` always true). Same wrongness for any user `calc` comparing a float channel to an int literal (`A=3`, `B#1`). Prior live verifications (Connected/Collecting pairs) used enum PVs (Int vs Int), which masked this. Cross-crate: the fix likely lands partly in `sidm`'s calc plugin (numeric-coercing compare), not only in `adl2sidm`.

### R1-34: MEDM CALC operator/function/operand surface beyond `#`/`=` is untranslated and fails at runtime — widget silently hidden forever, no warning

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:451-453` — `translate_calc_to_evalexpr` handles exactly two tokens (`#`→`!=`, standalone `=`→`==`) and passes everything else through verbatim; `visibility_gate_address` warns only for `&` (codegen.rs:406-416, left always-visible). At runtime `calc_plugin.rs:196` (`eval_with_context(expr, &ctx).ok()?`) swallows every parse/eval error → the gate never publishes → since `c750c23` the gate condition `is_some_and(|v| v != 0.0)` (codegen.rs:366-368) hides the widget.

Reference: `medm/medmCalc.c:178-260` — the MEDM CALC token table: functions `ABS SQRT SQR EXP LOGE LN LOG ACOS ASIN ATAN ATAN2 MAX MIN CEIL FLOOR NINT COSH COS SINH SIN TANH TAN NOT`, unary `~`, bitwise keywords `OR AND XOR`, constants `PI D2R R2D`, `RNDM`, ternary `? :` (:249-250), `**` exponent (:253), and operands `A`–`L` in **both cases** (:212-236). `medm/utils.c:4498-4505` binds E,F=0, G=elementCount, H=hopr, I=status, J=severity, K=precision, L=lopr of the main channel. evalexpr knows none of these spellings; the Rust gate binds only uppercase `A`–`D` variables (codegen.rs:334-339). Porting precedent: `adl2pydm/calc2rules.py:41-58` maps `A`–`L` (any case) to `ch[idx]` and lowercases NAME tokens.

Impact: any visibility `calc` using a function, keyword operator, constant, ternary, `**`, lowercase operand, or E–L operand evaluates to an error → widget permanently hidden, with **no converter warning** — asymmetric with the `&` case, which warns and fails visible.

### R1-35: Old-format channel keys `rdbk`/`ctrl` not read — pre-2.2 `.adl` monitors/controls all skipped

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:2673-2680` — `channel_address` reads only `attributes["control"]["chan"]` then `attributes["monitor"]["chan"]`; a widget with `monitor { rdbk="..." }` or `control { ctrl="..." }` resolves no channel and is dropped via `skip_no_channel` ("has no channel; skipped").

Reference: `adl2pydm/output_handler.py:177-185` — `get_channel` checks `("chan", "rdbk", "ctrl")`. Ground truth: `medm/medmMonitor.c:77` accepts token `"rdbk"` and `medm/medmControl.c:37` accepts `"ctrl"` (the pre-MEDM-2.2 spellings, still parsed by current MEDM).

Impact: legacy `.adl` screens (old-format monitor/control blocks) convert to screens with every channel widget missing — both references bind these channels.

### R1-36: Plot sub-blocks (`plotcom`, `x_axis`/`y1_axis`/`y2_axis`) dropped by the parser — plot titles, axis labels, plot colours, and user axis ranges all lost

Severity: Medium

Rust: `adl2sidm/src/adl_parser.rs:264-271, 364-409` — `ATTRIBUTE_BLOCKS` lifts only the 5 named attribute blocks and `apply_widget_specifics` handles only `textix`/`children`/`trace[`/`pen[`/`display[`/`command[`; `plotcom` and the three axis blocks are never parsed into the IR. Consequently `emit_strip_chart` (codegen.rs:1319-1370) and `emit_cartesian_plot` (codegen.rs:1392-1500) emit no title, no axis labels, no plot colours, and always leave sidm autoscale (sidm plot builders also expose no title/range API — `time_plot.rs`/`waveform_plot.rs` `with_*` lists).

Reference: `adl2pydm/adl_parser.py:302-320` — `parsePlotcomBlock` lifts `title`/`xlabel`/`ylabel` and folds plotcom `clr`/`bclr` into the widget colours; `adl_parser.py:462-466` stores `x_axis`/`y1_axis`/`y2_axis`. `output_handler.py:694,742-757` writes cartesian `title` + `xLabels`/`yLabels`, `:760-767` sets `autoRangeX/Y = (rangeStyle == "auto-scale")`, `:769-774` writes `axisColor`/`backgroundColor`; `:1064,1071-1076` the same for strip chart. Ground truth: `medm/medmCartesianPlot.c:2499-2547` applies `rangeStyle == USER_SPECIFIED_RANGE` with `minRange`/`maxRange` per axis.

Impact: every converted plot loses its caption and axis text; user-specified MEDM axis ranges render autoscaled.

### R1-37: MEDM `direction` unmapped for valuator (no orientation at all) and unmapped inversion for `down`/`left` bars

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:684-717` — `emit_valuator` never reads `direction` (and `SidmSlider` has no orientation builder — `sidm/src/widgets/slider.rs:49-69` exposes only limits/num_steps/precision/border_mode), so every valuator emits horizontal. `codegen.rs:2612-2640` — `direction_orientation` (used by byte + scale indicator) collapses `down`→Vertical and `left`→Horizontal with no inversion, and sidm's `SidmScaleIndicator` has no inverted-appearance API.

Reference: `adl2pydm/output_handler.py:1146` — `write_block_valuator` calls `write_direction`, which maps `up`/`down`→`Qt::Vertical` and additionally writes `invertedAppearance=True` for PyDMScaleIndicator when direction is `down`/`left` (`output_handler.py:436-450`). Ground truth: `medm/medmValuator.c` renders per `dlValuator->direction` (switches at :78/:338/:644; default `RIGHT` at :1446); `medm/medmBar.c` fills from the direction's origin.

Impact: a vertical MEDM valuator (`direction="up"`, common for slider columns) renders as a horizontal slider; `down`/`left` bars fill from the wrong end.

### R1-38: Byte with absent `sbit`/`ebit` collapses to 1 bit — MEDM's defaults are `sbit=15, ebit=0` and MEDM omits default values when writing the file

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:776-787` — `emit_byte` defaults both `sbit` and `ebit` to `0` (`unwrap_or(0)`), giving `num_bits = 1`, `shift = 0`, little-endian: a single-segment indicator.

Reference: `medm/medmByte.c:279-280` — `createDlByte` defaults `sbit=15, ebit=0` (a 16-bit, MSB-first byte), and `writeDlByte` **omits** the attributes at their defaults (`:366-369`). So a stock MEDM byte widget carries neither key in the `.adl`. (adl2pydm `output_handler.py:592-597` has the same 0/0 default — a bug against the C ground truth; MEDM C is authoritative here.)

Impact: every default-configured MEDM byte widget (16-bit status words) renders as a 1-bit indicator showing only bit 0.

### R1-39: Scale-indicator label modes and `fillmod` unhandled — indicator/meter always show the value label; center-fill bars fill from the edge

Severity: Low

Rust: `adl2sidm/src/codegen.rs:884-893` — `emit_scale_indicator` suppresses the value label (`with_value_label(false)`) only when `bar` and `label ∉ {limits, channel}`; for `indicator`/`meter` the sidm default `show_value_label: true` (`sidm/src/widgets/scale_indicator.rs:79`) always applies regardless of the MEDM `label` mode. No emitter reads `fillmod`, and there is no origin-at-zero/limit-labels capability in `SidmScaleIndicator`.

Reference: `adl2pydm/output_handler.py:1312-1329` — `write_limits` (called for indicator/meter via `write_block_indicator:844`) sets `showValueLabel=False` for label `None`/`no decorations`/`outline` and `True` only for `limits`/`channel` (plus `showLimitLabels` per mode). Ground truth: `medm/medmIndicator.c:123-167` draws the value text only under the `LIMITS`/`CHANNEL` label arms. For center fill: the real MEDM token is `fillmod="from center"` (`medm/medmBar.c:496-502`, default `FROM_EDGE` at :433, written only when non-default at :550; drawn from center at :194) — adl2pydm's own `origin`-key read (`output_handler.py:581-582`) is dead code against real MEDM files, so MEDM C is the contract here.

Impact: indicators/meters whose MEDM label mode hides the readout render an extra value label; `fillmod="from center"` bars fill from the low edge instead of the midpoint.

### R1-40: Vertical (stacking="row") choice buttons get their font from the full widget height instead of per-button height

Severity: Low

Rust: `adl2sidm/src/codegen.rs:677` — `emit_choice_button` passes `font_px: Some(font_px_from_height(geom.height))` unconditionally, so a 4-item vertical choice button 80 px tall gets a 20 px (clamp-capped) font inside 20 px rows; the round-8/9 exact-share division then truncates the captions.

Reference: `adl2pydm/output_handler.py:650-660` — for `stacking="row"` (vertical) the font is estimated from per-button height (`per_button_h = h / max(2, round(h/20))`); only `column` (horizontal) uses the full height. Ground truth: `medm/medmChoiceButtons.c:134-135` — `usedHeight = height/numberOfButtons` for row stacking, with the font chosen from that per-button height (`:69-88`).

Impact: multi-item vertical choice buttons render oversized, truncated captions where MEDM (and adl2pydm) shrink the font to the per-button row.

## Cleared During Review

Fix round 2026-07-03/04 (one commit per finding; branch merged fast-forward
into main):

**Category D batch (`fix/sidm`, merged at `e4ed898`):**

- R1-25 — `e19bf21` pva Connected publishes `write_access = true`
  (p4p parity: protocol carries no access-rights signal).
- R1-26 — `acea1d7` value mask now pyepics' exact
  `DBE_VALUE|DBE_ALARM|DBE_PROPERTY` (DBE_LOG dropped, pyepics parity) plus
  a DBE_PROPERTY-only subscription that refetches CTRL metadata
  (`update_ctrl_vars` parity; epics-ca-rs monitor snapshots are TIME-class).
- R1-27 — `c6d3d03` CA wire strings decode latin-1 at all four sites
  (units / string values / string arrays / enum labels); pva stays UTF-8.
- R1-28 — `3454028` pva monitor name = netloc only, `/path` drilled as
  subfield keys; RPC form implemented (NTURI, typed args, `pydm_pollrate`).
  *Residual:* pva subfield **writes** dropped with warning (part of the
  recorded NTTable value-model deferral).
- R1-29 — `cdb8d3d` `PvField::Union` unwraps the selected variant and
  recurses; NTNDArray ubyte lands as `Bytes`. *Residual:* compressed-codec
  arrays (blosc/lz4/bslz4/jpeg) skip the value with a one-time warning —
  decompression needs new deps.
- R1-30 — `985220a` ca+pva puts gate on published write access; CA seeds
  rights from `ChannelInfo` on every connect; `SIDM_READ_ONLY` env
  (PYDM_READ_ONLY parity) read at plugin construction. *Residual:* the
  revoked-rights path has unit-level coverage only (in-process CaServer
  always grants write).
- R1-31 — `ff8fcb8` value events only on actual value change: CA compares
  against `last_value` (cleared on disconnect), pva gates on the monitor's
  changed-field marks; first update always emits.
- R1-32 — `e4ed898` loc:// `type=array`, `unit`/`upper_limit`/
  `lower_limit`/`enum_string` extras, float auto-precision (digit count
  capped 8) on init and every float put.

## Review Log

- 2026-07-03: round opened; 5 read-only agents spawned (A/B/C/D/E).
- 2026-07-03: round consolidated — **40 findings** (High 4, Medium 22,
  Low 14), renumbered R1-1..R1-40 (A: 1–8, B: 9–16, C: 17–24, D: 25–32,
  E: 33–40).

  Thematic clusters:
  - **Recent-churn residue (0.4.x zoom work):** R1-3 (context-menu reset
    adopts degenerate `(v,v)` unrepai­red; `home_limits` now write-only),
    R1-7 (limits-history lifecycle inverted; per-frame wheel pushes).
    The two reset verbs and two zoom gestures now disagree with each
    other, not just with silx — an invariant-ownership smell.
  - **y2 axis as a second-class citizen (siplot):** R1-1, R1-5 — gesture
    paths skip y2 while keyboard/toolbar paths handle it; reset skips
    y2-only plots. One owner for "apply a view-limits change to all
    axes" would close the family.
  - **Normalization-blind autoscale (siplot):** R1-9 — one structural
    fix (thread `Normalization` into `AutoscaleMode::range`) closes four
    symptoms incl. total render collapse for log images with ≤0 values.
  - **Estimation-seed drift (siplot fit):** R1-12/13/14/15 — constants
    and pre-processing steps that differ from FitManager's actual
    call path (vs the standalone pyx defaults).
  - **plot3d: composition gaps, not math gaps** — R1-17..R1-24: the
    core math verified element-for-element; what's missing are upstream
    default *compositions* (labelled axes, orientation viewport, stroke,
    specular/fog) and the transform-stack API surface.
  - **sidm: silent-disable / silent-dead-channel class:** R1-25 (pva
    write access), R1-28 (subfield grammar), R1-29 (NTNDArray union) —
    all present as "connected but permanently inert", invisible to
    happy-path tests that bypass widgets.
  - **0.21-migration boundary:** R1-27 (UTF-8-lossy vs latin-1 decode
    policy) is the one real policy divergence introduced by the
    migration; the rest of the boundary verified clean (WallTime,
    unsigned variants, EnumWithChoices, pvRequest None, connect race).
  - **adl2sidm: evalexpr semantics vs MEDM CALC:** R1-33/34 — the
    translated gate expressions run on an engine with different typing
    and a tiny fraction of MEDM's operator surface, and errors fail
    *hidden* with no converter warning. Fix spans adl2sidm + sidm's
    calc plugin.
  - **adl2sidm: old-format/default-value blind spots:** R1-35 (rdbk/
    ctrl), R1-38 (sbit=15 default omitted from files), R1-36 (plotcom/
    axis blocks) — the parser was built against modern minimal files;
    MEDM's write-only-when-non-default convention makes absent keys
    semantically loaded.

  Classification (per port-translation-lessons):
  - Reference-independent defects (real regardless of upstream): R1-3,
    R1-4 (NaN renders), R1-9 (render collapse), R1-11 (sum doubled),
    R1-25, R1-29 (dead channels), R1-33/34 (widgets wrongly hidden).
  - Reference-faithful gaps (adopt upstream posture): R1-2, R1-6, R1-7,
    R1-8, R1-10, R1-12..16, R1-17, R1-21..24, R1-26, R1-27, R1-30,
    R1-31, R1-39, R1-40.
  - Interop-contract gaps (the file format / address grammar is the
    contract): R1-28, R1-35, R1-36, R1-37, R1-38.
  - Unimplemented surface (scope decisions to record or close): R1-18,
    R1-19, R1-20, R1-32, R1-5 (partially).
