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

### Round 2 (2026-07-04) — R2-1..R2-69

Same 5-agent split, scopes rotated to surfaces R1 left uncovered
(A: tools/widget layer, not gesture mechanics; B: items/colors/ticks/
fit-engine internals, not estimation seeds). Agent-local numbers were
renumbered to the contiguous R2-1..R2-69 (A: 1–26, B: 27–45, C: 46–52,
D: 53–60, E: 61–69). Per-category "below-bar residuals"/"examined and
excluded"/"verified clean" notes are retained inline — they are
inventory for future rounds, not findings.

### R2 Category A — plot tools & widget layer (vs silx tools/widgets/actions) [R2-1..R2-26]


### R2-1: ImageStack renders every frame through a fixed `viridis(0.0, 1.0)` colormap — silx autoscales the plot-default gray per frame

**FIXED (two sites):**

- *ImageStack core* — `9921117`: `rebuild_image` now derives each frame's range
  through a split-out `frame_colormap()` helper (base LUT/normalization
  preserved, range re-autoscaled minmax per frame); the default base colormap is
  gray. silx `addImage` with no colormap → the plot default gray with
  `vmin/vmax = None`, re-autoscaled to each frame (ImageStack.py:548-550,
  PlotWidget.py:1465-1467).
- *CompareImages sibling* — this session: `build_composite` seals the colormap
  over both placed images' concatenated finite values via a pure
  `seal_compare_colormap` helper, applied to every intensity mode
  (OnlyA/OnlyB/HalfHalf/SplitHorizontal/RedBlueGray[Neg]) — silx
  `CompareImages.__getSealedColormap` over `_CompareImageItem.getColormappedData`
  (tools/compare/core.py:166-183, 192-198), including the zero margin padding
  (`__createMarginImage`, CompareImages.py:842-844). The default colormap is now
  `Colormap::autoscale(Gray)` (silx `Colormap()`); a caller may still pass a
  pinned colormap to `set_images` to fix the range (`resolved` is a no-op on a
  pinned map, matching silx `setVRange`). Per-boundary tests: seal
  autoscales-over-both / keeps-a-pinned-range / includes-zero-padding. Both
  halves build on the R2-46 per-bound `Colormap::resolved` primitive.

Severity: High

Rust: `src/widget/image_stack.rs:521` — `colormap: Colormap::viridis(0.0, 1.0)` in `ImageStack::new`; `rebuild_image` (`:807-822`) passes `self.colormap.clone()` verbatim into `try_add_image`/`try_update_image`, and `set_colormap` (`:699`) only replaces the fixed map — no autoscale path exists.

Reference: `silx/gui/plot/ImageStack.py:548-550` — `self._plot.addImage(self._urlData[url.path()], resetzoom=...)` with **no** colormap argument; `silx/gui/plot/PlotWidget.py:1465-1467` — a new image gets `setColormap(self.getDefaultColormap())` = gray with `vmin=None, vmax=None`, i.e. re-autoscaled to each frame's own data range.

Impact: browsing any stack whose values are outside `[0, 1]` (counts, detector frames) shows a saturated single-color image out of the box; even after `set_colormap` the range stays frozen across frames while silx re-autoscales per frame. This is a residual site of the R1-16 family — its fix `1e8af27` changed only `high_level.rs:3477`. (A further out-of-category sibling exists at `high_level.rs:8577`, CompareImages.)

### R2-2: Free-line profile samples half a pixel off (silx's `-0.5` corner→centre shift dropped at every caller), and the axis-aligned free-line snap branch is unported

**FIXED (Round 2 profile-subsystem cluster):** ported silx `createProfile`'s
free-line dispatch as `free_line_profile` (`high_level.rs`): the aligned-endpoints
check (`int(startRow)==int(endRow) || int(startCol)==int(endCol)`) routes to a new
`aligned_partial_profile` (integer-rectangle plain mean/sum, out-of-image
**zero-padded**, a faithful port of silx `_alignedPartialProfile`); the general
case calls `line_profile_band` with silx's `-0.5` corner shift applied to both
endpoints. `line_profile_band` stays the pixel-centre primitive. Both Line-ROI
callers now route through it: `profiles_for_roi` Line arm (`profile_window.rs`) —
which backs the ImageView profile window and the StackView 1D line — and the free
`stack_line_profile` (StackView 2D line, per frame). Tests:
`free_line_profile_general_case_applies_the_minus_half_shift`,
`free_line_profile_row_aligned_uses_integer_rectangle`,
`free_line_profile_column_aligned_uses_integer_rectangle`,
`free_line_profile_aligned_zero_pads_out_of_image`. Residual: nearest-neighbour
`line_profile_values` callers still round raw corner-convention coords (R2-6 x-axis
concern, distinct); the profile-window plot-axis labels/coords are R2-6.

Severity: High

Rust: `src/widget/profile_window.rs:87-88` — `Roi::Line { start, end } => line_profile_band(..., *start, *end, ...)` where `start`/`end` come straight from `transform.pixel_to_data` (`src/widget/high_level.rs:10793,10800-10805` ImageView drag; `:12886-12891` StackView 1D). `line_profile_band`'s own doc (`high_level.rs:1546-1548`) declares its inputs are *pixel-centre* coordinates and that "silx's `-0.5` plot-corner shift is *not* applied here" — but no caller applies it. Same family: nearest-neighbour `line_profile_values` callers round raw corner-convention coords.

Reference: `silx/gui/plot/tools/profile/core.py:480-488` — `bilinear.profile_line((startPt[0] - 0.5, startPt[1] - 0.5), (endPt[0] - 0.5, endPt[1] - 0.5), roiWidth, method)`; and `core.py:413-448` — a free line whose endpoints share an integer row/column is snapped to `_alignedPartialProfile` (integer-rectangle `numpy.mean/sum`, out-of-image region **zero-padded**, `core.py:300-325`), never bilinear.

Impact: every free-line profile bilinearly samples 0.5 px up/right of silx — a drag along a pixel-row centre yields a 50/50 blend of two rows where silx returns exactly that row. A horizontal/vertical drawn free line additionally returns cross-row interpolated values (NaN out of bounds) instead of silx's exact integer-row reduction (zeros out of bounds).

### R2-3: H/V band profiles use plain mean/sum — silx uses `nanmean`/`nansum`, so masked (NaN) pixels poison the whole band

**FIXED (profile cluster, commit 2cef1d4):** `aligned_profile_values` reduces
the band NaN-aware — `Mean` divides by the finite count (NaN for an all-NaN
band = `numpy.nanmean`), `Sum` sums the finite pixels (0.0 for all-NaN =
`numpy.nansum`), silx `_alignedFullProfile` (tools/profile/core.py:241-247).
The sibling `rect_profile_values` (Roi::Rect strips) was swept for the same
NaN-poisons-reduction defect and made consistent. Tests: aligned + rect skip
a NaN in the band; all-NaN band → sum 0.0, mean NaN. (Doc marker backfilled
— code fix landed earlier this session.)

Severity: Medium

Rust: `src/widget/high_level.rs:1446-1459` — `aligned_profile_values` accumulates `(start..end).map(...).sum()` then divides by the full band size; no NaN filtering (the free-line `line_profile_band` *is* finite-filtered — internally inconsistent).

Reference: `silx/gui/plot/tools/profile/core.py:241-247` — `_alignedFullProfile`: `fct = numpy.nanmean` (mean) / `numpy.nansum` (sum).

Impact: siplot's mask pipeline stores masked pixels as `f32::NAN`, so an h/v profile with Width > 1 crossing a masked blob shows a NaN gap where silx shows the mean/sum of the remaining unmasked rows; one NaN pixel nukes the sample.

### R2-4: Profile never recomputes outside an active drag — Width/Method edits and image-data changes are dead until the next drag

**FIXED (profile cluster, commit d00ae1c):** `ProfileWindow` retains the
`(image, ROI)` it extracted from (`ProfileSource`) and owns one `recompute()`;
`set_line_width`/`set_method` recompute from it, `refresh_image` re-derives on a
new image — wired into `ImageView` (high_level.rs:10634) and `StackView`'s
per-frame dirty upload (high_level.rs:13717) — and the precomputed-curve path
(`set_profile_curve`) clears the source so a later width/method edit never
re-derives a stale image ROI over a scatter/stack profile. Width/Method edits
and image/frame changes now update the profile without a fresh drag (silx
`invalidateProfile` / recompute-on-DATA, manager.py:936-944, rois.py:238-257).
Tests: width/method recompute; frame-scrub recompute; precomputed-curve clear.
(Also recorded in the profile-subsystem cluster block below.)

Severity: Medium

Rust: the only `update_profile` call sites are inside `response.dragged()` blocks (`src/widget/high_level.rs:10796-10808`; StackView via `show_profile` from its drag handler); `ImageView::set_image` never touches `profile_window`; no profile ROI is retained after `drag_stopped`. The comment at `src/widget/profile_window.rs:341-343` — "the host re-drives from the active ROI each frame" — is false.

Reference: `silx/gui/plot/tools/profile/manager.py:936-944` — recompute on item DATA/MASK/POSITION/SCALE change; `silx/gui/plot/tools/profile/rois.py:238-257` — `setProfileMethod`/`setProfileLineWidth` call `invalidateProfile()` → immediate recompute; `:234-236` — recompute on ROI region edit.

Impact: with the profile window open, changing the Width DragValue or Mean/Sum combo visibly does nothing, and replacing the image leaves a stale profile; silx updates instantly in all three cases. Structural cause: no profile ROI is retained, so no recompute trigger has anything to act on.

### R2-5: StackView 2D stack profile hardcodes width = 1 / Mean and nearest-neighbour line sampling — the 1D mode of the same tool honors Width/Method

**FIXED (profile cluster, commit 09b8f40):** the `StackProfileDimension::TwoD`
arm reads the shared line width / reduction method from the profile window
(`profile_window.line_width()` / `.method()`, high_level.rs:13438-13439) — silx
`Profile3DToolBar` uses one setting for the 1D and 2D profiles — instead of the
hardcoded `1` / `Mean`, and its line profile is the bilinear band
(`stack_line_profile` → `line_profile_band`) matching the 1D line, not
per-frame nearest-neighbour. Ref silx rois.py:1096-1104 (stack ROIs pass
`lineWidth`/`method` into the same `core.createProfile`). Tests: stack line
profile varies with width/method; 2D profile equals a width-3 Sum extraction,
not width-1 Mean. (Also recorded in the profile-subsystem cluster block below.)

Severity: Medium

Rust: `src/widget/high_level.rs:12895-12903` — the `StackProfileDimension::TwoD` arm calls `stack_aligned_profile(..., 1, ..., ProfileMethod::Mean)` for H/V and `stack_line_profile` (`:12414-12428`, per-frame nearest-neighbour `line_profile_values`) for Line, ignoring the profile window's width/method.

Reference: `silx/gui/plot/tools/profile/rois.py:1096-1104` — the image-stack profile ROIs pass `lineWidth=self.getProfileLineWidth(), method=method` into the same `core.createProfile` (h/v → nan-aware band, line → bilinear `profile_line`).

Impact: switching the Profile3D toolbar to 2D silently reverts to a 1-px mean profile; the 2D line profile additionally uses nearest-neighbour instead of bilinear sampling. Roadmap row 553 records the extraction cores (which *do* take width/method) but not this hardcoded wiring.

### R2-6: Profile window plots value-vs-distance; silx plots against the projected plot axis with computed title/labels

**PARTIALLY FIXED (Round 2 profile-subsystem cluster) — x-coordinate half:**
`free_line_profile` (`high_level.rs`) now returns silx's projected plot-axis
coordinates (`core.py:529-563`) instead of arc distance: a row-aligned line runs
over its column coords (`arange + startCol`), a column-aligned line over its row
coords (`arange + startRow`), and a diagonal line over `linspace(x0, x1, len)` in
X data coords — with endpoints ordered left-to-right (silx `core.py:467-470`) so
the profile reads the same regardless of drag direction. siplot's identity image
geometry (origin `(0,0)`, scale `(1,1)`) collapses silx's `arange*scale+origin` to
these. This closes the cited numeric divergence (a `(0,0)→(3,4)` diagonal now
reads its X-span `0..3`, not distance `0..5`). Tests:
`free_line_profile_general_case_applies_the_minus_half_shift` (x = linspace),
`free_line_profile_general_case_orders_endpoints_left_to_right`,
`free_line_profile_aligned_coords_offset_by_the_start_pixel`.

**FIXED (Round 2 profile-subsystem cluster) — title/labels + scatter halves
(user sign-off 2026-07-04, "compute title, fall back Columns/Rows"):**

*Title/labels.* `profile_window.rs` now computes silx's self-describing profile
title + axis labels (`createProfile` `profileName`/`xLabel`, `core.py:369-565`;
`_lineProfileTitle`, `rois.py:68-89`), instead of the static `"Profile"`. A pure
`image_profile_desc(roi, w, h, line_width)` returns the `{xlabel}`/`{ylabel}`
templates per ROI kind: HRange → `{ylabel} = <row>` / `= [<lo>, <hi>]` widening
with the band (`_alignedFullProfile`); VRange → `{xlabel} = <col>`; Rect → the
row range reduced over columns; Line → the aligned/diagonal `_lineProfileTitle`
forms, replicating `free_line_profile`'s column-then-row endpoint ordering so the
title is drag-direction-independent; Cross → the crossing pixel (siplot merges
silx's two cross sub-windows into one). `relabel` fills the tokens from the
source plot's axis labels (silx `_relabelAxes`, empty → `"X"`/`"Y"`); the two
image call sites thread the ImageView `Plot2D`'s `"Columns"`/`"Rows"` and the
StackView's perspective labels. The title gets silx's `; width = %d` suffix and
the Y label is the method name (`"Mean"`/`"Sum"`). `%g` coordinate formatting is
ported in `format_g`/`format_g_signed`.

*Scatter.* `distance_value_curve` is replaced by
`ScatterLineProfile::dominant_axis_curve` (`scatter_viz.rs`): silx plots the
interpolated values against whichever source axis spans farther along the segment
(`rois.py:801-808`, `|x_last−x_first| > |y_last−y_first|`, tie → Y), not arc
distance, and `scatter_profile_labels` builds the window title via
`_lineProfileTitle` with `yLabel = "Profile"` (`rois.py:797-821`).

Tests: `format_g_matches_python_percent_g`, `relabel_*`, `hrange/vrange/rect/
cross_title_*`, `line_title_horizontal_vertical_and_diagonal`,
`line_title_is_independent_of_drag_direction`,
`scatter_labels_pick_the_dominant_axis_and_relabel`,
`dominant_axis_curve_*` (dominant span, tie-to-Y, NaN gaps, empty).

Severity: Medium

Rust: `src/widget/high_level.rs:1557` — `line_profile_band` returns `(distance_along_line, value)` pairs, plotted as-is; the scatter path plots `distance_value_curve` (`src/core/scatter_viz.rs:631-642`, whose doc claims it is "the form silx `ScatterProfileToolBar` shows"); `src/widget/profile_window.rs:196` — static title `"Profile"`, no axis labels.

Reference: `silx/gui/plot/tools/profile/core.py:540-563` — aligned profiles use `arange(len)*scale + origin` in the profiled axis' data coords; diagonal lines use `numpy.linspace(x0, x1, len)` (X data coords) with `xLabel = "{xlabel}"`; `silx/gui/plot/tools/profile/rois.py:801-808` — scatter profiles pick `points[:, 0]` or `points[:, 1]` by dominant span; `rois.py:313-323` — window title = computed profile description + `"; width = %d"`, axes relabeled from the source plot.

Impact: numerically different x values in the profile window — a (0,0)→(3,4) line reads 0..5 in siplot vs 0..3 in silx — and the window carries none of silx's self-describing title/labels. Distance is silx's convention only for `ProfileImageDirectedLineROI` (`rois.py:444-454`), which siplot does not port.

### R2-7: Median filter compounds on repeated Apply — silx always refilters the retained original image

**FIXED (stats/actions cluster):** `PlotWidget` now carries a single-slot
`median_filter_original: Option<(ItemHandle, Vec<f64>)>` — the analog of silx
`MedianFilterDialog._originalImage` (medfilt.py:83-102).
`apply_median_filter_kernel` captures on first Apply, always filters the
capture, and restores it after its own replace (silx's
sigActiveImageChanged disconnect/reconnect around `addImage`). Invalidation
is owned by the retained-data choke point: `set_retained_data` drops the
capture only when the item's PIXEL data changes bit-wise
(`image_pixels_bit_equal`, `to_bits` so NaN-bearing images compare equal to
themselves) — colormap/alpha/geometry-only re-uploads (autoscale, level
edits) keep it, matching silx where colormap edits never re-add the image.
`remove`/`clear` free the capture (handles are monotonic, never reused).
Tests (`tests/median_filter_original.rs`, headless GPU):
`repeated_apply_refilters_the_original_not_the_result` (3→5→3, with a
sanity assert that compounding would differ),
`colormap_only_reupload_keeps_the_capture` (autoscale between Applies),
`replacing_the_pixels_recaptures` (update_image_spec with new data).

Severity: Medium

Rust: `src/widget/high_level.rs:7075-7106` — `apply_median_filter_kernel` reads the **current** retained pixels, filters, then `update_image_spec(handle, spec)`; `update_image_spec` (`:4446-4449`) calls `set_retained_data(handle, data)` with the *filtered* pixels, so the next Apply filters the already-filtered image.

Reference: `silx/gui/plot/actions/medfilt.py:83-102` — `_updateActiveImage` captures `self._originalImage`; `_updateFilter` disconnects `sigActiveImageChanged`, filters `_originalImage`, `addImage(..., replace=True)`, reconnects — the disconnect exists precisely so the original survives every kernel change.

Impact: Apply at width 3 then width 5 displays `medfilt5(medfilt3(orig))` in siplot vs `medfilt5(orig)` in silx — progressive, irreversible degradation during normal kernel exploration, unrecoverable without re-adding the image.

### R2-8: FitAction plot flow unported — fit range not seeded from the visible X window, no "Fit <legend>" overlay curve on the source plot

**FIXED (fit-subsystem cluster):** both halves. (1) `set_fit_target` now seeds
`FitWidget`'s fit range from the plot's current X limits
(`fit.set_fit_range(Some(self.x_limits()))` — silx
`self._setXRange(*plot.getXAxis().getLimits())` at trigger, fit.py:249), and
`perform_fit` was brought in line with `perform_fit_choice` to honor the
configured range (silx's fitmanager always fits its xmin/xmax-restricted
data — with the seeded range, the old whole-curve path would have fitted the
full spectrum while `fit_range()` reported a window). (2) New
`PlotWidget::sync_fit_overlay(fit, source)` — the plot half of silx
`handle_signal` (fit.py:429-451): a successful fit adds/updates a
`Fit <legend>` curve on the SOURCE plot (no zoom reset; source's Y axis
re-applied per fit as silx `setYAxis`; plot axis labels carried as silx
curveParams), and while no result exists an existing overlay is hidden
(FitStarted/FitFailed → `setVisible(False)`), not removed. `FitWidget` gained
`fit_range()`/`fit_curve()` accessors (the ranged finite xs + fitted model —
what silx overlays). Tests (`tests/fit_action_flow.rs`, headless-GPU):
`fit_target_seeds_range_from_visible_x_window` (range == view window, fitted
xs ⊂ window, model reproduces the line),
`fit_overlay_appears_updates_in_place_and_hides_without_result` (no overlay
before first result; appears visible; re-fit updates same handle; new data
hides not removes; next success re-shows).

Severity: Medium

Rust: `src/widget/fit_widget.rs:726-735` — the fit result is a curve named `"Fit"` on the FitWidget's own internal Plot1D; `src/widget/high_level.rs:5872-5884` — `set_fit_target` passes the full `(x, y)`; `fit_widget.rs:445,452-457` — range defaults to whole-curve and, when enabled, seeds from the *data extent*, never from the plot's current X limits.

Reference: `silx/gui/plot/actions/fit.py:249` — `self._setXRange(*plot.getXAxis().getLimits())` (fit defaults to the visible zoom window); `:429-451` — `fit_legend = "Fit <%s>" % legend`, `x_fit` clipped to the range, `plot.addCurve(x_fit, y_fit, fit_legend, resetzoom=False, ...)` overlays the result on the **source** plot, hidden on `FitStarted`/`FitFailed`.

Impact: fitting a zoomed-in peak fits the whole spectrum — numerically different parameters for the canonical silx workflow — and the fit overlay never appears next to the data. Roadmap rows 549/551/560 cover only the FitWidget dialog internals.

### R2-9: PositionInfo snapping engage contract diverges — silx engages by item *pick* (filled-bar area / ±3 px polyline) with histogram priority-break and a DPR-scaled radius; siplot uses global-nearest apex within an unscaled 5 px

**FIXED (stats/actions cluster):** `snap_cursor` now walks candidates in item
order applying silx's per-item pick engagement. New pure kernels in
`position_info.rs`: `pick_polyline_indices` (the GLPlotCurve2D box pick,
GLPlotCurve.py:1396-1494 — data-space Cohen–Sutherland outcodes, inside
vertices plus lower endpoints of crossing segments tested against the bound
flagged in the *second* endpoint's outcode, NaN vertices masked, solid lines
take the segment path because silx maps `'-'` to dash pattern `()`) and
`pick_filled_histogram` (items/histogram.py:245-291 — strict bounds whose y
range always includes 0, `searchsorted(side="left") - 1` so an interior edge
belongs to the left bin, downward bars pick between value and baseline), plus
`PICK_OFFSET = 3` (BackendOpenGL `_PICK_OFFSET`, :1267). The pick box is
`±max(3, markerSize/2, lineWidth/2)` clipped into the plot area
(`_mouseInPlotArea`, :1269-1304) and converted per item axis; items with
neither line nor symbol are unpickable (GLPlotCurve.py:1409-1416). A picked
histogram returns bin centre + count immediately (silx `break`,
PositionInfo.py:246-258) — unconditional priority over nearer curve vertices;
curve/scatter distances run over picked indices only, first-minimum per item
(`nanargmin`), against the live radius `SNAP_THRESHOLD_DIST ×
pixels_per_point` (the `devicePixelRatio` scaling, :229-237; captured per
frame in `show`) shrinking to each accepted snap with ties to the later item
(:286-292). The non-filled ±3 px step-polyline histogram pick has no
reachable counterpart: siplot histograms are always filled by construction
(`add_histogram_with_align` sets `fill = true`; `update_curve_spec` replaces
retained histogram data with the curve form, so a non-filled histogram record
cannot exist). Tests: 8 kernel unit tests (crossing/corner-miss/NaN/no-line
boundaries; bar-interior/strict-bounds/edge-ownership/downward-bar) + 4
integration tests (`filled_bar_interior_snaps_far_from_the_apex`,
`picked_histogram_outranks_a_nearer_curve_vertex`,
`vertex_within_radius_but_outside_the_pick_box_does_not_snap`,
`snap_radius_scales_with_pixels_per_point` at pixels_per_point 2 vs 1).

Severity: Medium

Rust: `src/widget/high_level.rs:7213-7233` — `snap_cursor` feeds histogram `(centers, counts)` apex vertices (plus curve/scatter points) to `snap_to_nearest(..., SNAP_THRESHOLD_DIST)` (raw constant 5, `src/widget/position_info.rs:200`), picking the globally nearest vertex across all items; no `pixels_per_point`/DPR factor anywhere on the path.

Reference: `silx/gui/plot/tools/PositionInfo.py:229-237` — `sqDistInPixels = (SNAP_THRESHOLD_DIST * ratio) ** 2` with `ratio = devicePixelRatio()`, in Qt-logical space (`BackendOpenGL.dataToPixel` divides by DPR, BackendOpenGL.py:1617-1624); `:246-258` — a histogram is engaged via `item.pick(xPixel, yPixel)` — filled histograms area-pick anywhere between baseline and value (`items/histogram.py:283-291`), non-filled within ±3 px of the *step polyline* (`BackendOpenGL.py:1267`) — then snaps to bin centre/value and `break`s (unconditional priority over nearer curve points).

Impact: hovering the middle of a tall filled bar snaps in silx, never in siplot; on a DPR-2 display (macOS default) silx's effective snap radius is 10 logical px vs siplot's 5 — snapping is twice as hard to trigger; and a picked histogram loses priority to any nearer curve vertex.

### R2-10: Mask overlay color never adapts to the image colormap — `_setOverlayColorForImage`/`cursorColorForColormap` unported, overlay stays the constructor placeholder

**FIXED (mask-tools cluster; structural):** the structural gap was that
`Colormap` bakes its LUT and loses name provenance, so *no* consumer could
apply silx's name-keyed cursor-color rule. `Colormap` now carries
`cursor_color: [u8; 4]` with one meaning on every path:
`ColormapName::cursor_color()` (the silx `_AVAILABLE_LUTS` table,
math/colormap.py:52-66 — pink `#ff66ff` for gray/green/viridis/cividis/
temperature, green `#00ff00` for red/magma/inferno/plasma, yellow `#ffff00`
for blue, black for every matplotlib-loaded name per colors.py:244) for
catalog constructions and `set_name`; the registry's color for
`from_registered`; black for raw LUTs (`from_colors`, `set_lut`, `with_lut` —
silx `setColormapLUT` clears the name and nameless resolves to "black",
math/colormap.py:185-196); `reversed()` keeps it (silx's "reversed gray"
keeps gray's pink). `colormap_io` persists the field (absent → black, the
nameless rule). New `MaskToolsWidget::set_overlay_color_for_colormap`
(silx `_setOverlayColorForImage`, MaskToolsWidget.py:449-458) is called on
every `ImageView::set_image` sync; per-level overrides survive, and the
RGBA-image black branch has no counterpart (siplot's mask editor only
attaches to colormapped images — noted in the method doc). The
ScatterView mask is distinct: siplot displays masked points via the scatter
selection flag, not a `_defaultOverlayColor` overlay, so there is no color
state to adapt there. Tests: `cursor_color_matches_the_silx_builtin_table`,
`colormap_carries_cursor_color_and_a_raw_lut_resets_it`,
`overlay_color_adapts_to_the_image_colormap` (placeholder → pink on gray →
green on inferno; override survives), `absent_optional_fields…` amended to
the nameless-black boundary.

Severity: Medium

Rust: `src/widget/mask_tools.rs:355-363` — `color: Color32::from_rgb(160, 160, 164)` ("silx `_defaultOverlayColor = rgba(\"gray\")`") is never updated on image sync; the built-in colormaps carry no cursor colors and `registered_colormap_cursor_color` has no widget caller.

Reference: `silx/gui/plot/MaskToolsWidget.py:449-458` — on every image sync `_defaultOverlayColor = rgba(cursorColorForColormap(colormap["name"]))` for colormapped images, `rgba("black")` for RGBA images; `silx/math/colormap.py:54-67` — `"gray" → "#ff66ff"` (pink), magma/inferno/plasma → `#00ff00`, blue → `#ffff00`.

Impact: silx's `rgba("gray")` is only a pre-first-image placeholder; siplot keeps it forever, so with the (now silx-default, R1-16) gray colormap the mask overlay is gray-on-gray and nearly invisible, and the per-colormap contrast rule plus the RGBA black fallback are absent.

### R2-11: Stats mean/std/sum/COM filter NaN out; silx propagates NaN through them (only min/max are NaN-immune)

**FIXED (stats cluster, semantics pinned empirically against numpy):** the
engine now applies silx's numpy.ma rules with the on-limits/ROI clip as the
ONLY data filter (stats.py:343-346): mean/sum propagate NaN/±inf
(`numpy.mean`/`sum`); std is `None` whenever any included value is
non-finite (numpy.ma.std returns `masked` for NaN AND ±inf — verified by
running numpy); min/max skip NaN but let ±inf win (combo.pyx:150-200), and
an all-NaN clip surfaces `Some(NaN)` (combo keeps its `data[0]` init);
COM propagates NaN (`sum==0` stays the only None case); coord_min/coord_max
return the FIRST NaN sample's coordinates (numpy argmin/argmax return the
first NaN index — verified; the finding's "coord-min/max are NaN-immune"
premise was wrong). Clip comparisons rewritten to the positive
`x >= lo && x <= hi` form so a NaN coordinate is excluded exactly like
silx's mask comparisons; under `All` it stays and pollutes COM/coords.
`finite_count` renamed `included_count` (one meaning: clip-included count).
Same-defect sites fixed in the same pass: `roi_stats.rs` Accumulator (the
ROIStatsWidget path — same numpy.ma downstream) and the display layer
(`format_stat`/`format_coord` now print `nan`/`inf`/`-inf` for data-borne
non-finite values; `--` only for None/masked, statshandler.py:77-84).
Distinct, kept: `ValueStats` (siplot items-panel summary, deliberately
finite-filtered — documented + divergence-boundary test);
`curve_roi_counts` (already faithful); histogram bin count (R2-20);
ImageView profile aggregation (separate surface). Boundary tests: NaN
mean/sum/std, ±inf min/max/std, all-NaN, first-NaN coords, mask-excludes-
NaN-x (OnLimits + ROI), scatter All-scope keeps non-finite, image NaN
pixel, roi_stats image/curve NaN. parity-roadmap.md:1654 claim corrected.

Severity: Medium

Rust: `src/core/stats.rs:22-23` — "Non-finite values (`NaN`, `±inf`) are filtered out before any aggregation, matching silx's reliance on finite data for min/max/com"; every `for_curve`/`for_scatter`/`for_image` accumulator skips non-finite values.

Reference: `silx/gui/plot/stats/stats.py:343-346` — `values = numpy.ma.array(yData, mask=mask)` where the mask is only the onlimits/ROI clip (NaN stays unmasked); `:790-797` — `calculate` applies `numpy.mean`/`numpy.std` (`StatsWidget.py:1273-1274`) directly, so NaN propagates; only min/max go through NaN-ignoring `silx.math.combo.min_max`.

Impact: an item with a single NaN sample shows `nan` for mean/std/COM (and sum) in silx's stats table but finite filtered values in siplot. The code comment claims a silx parity that holds only for min/max/coord-min/coord-max; roadmap row 1654 repeats the claim inside a Done row without framing it as a deviation.

### R2-12: ScatterMask missing `updateEllipse`, `updateLine`, and the data-extent-scaled pencil — only disk and polygon exist

**FIXED (mask-tools cluster):** all three ported to `scatter_mask.rs` in the
existing point-array API style. `update_ellipse` — INCLUSIVE
`(px−ccol)²/rc² + (py−crow)²/rr² <= 1.0` per-axis test (unlike the disk's
strict `<`; ScatterMaskToolsWidget.py:150-168). `update_line` — rotated
width-band polygon with silx's own `theta = atan(slope)`, `theta = 0` for a
vertical line so the band degenerates to zero width (bug-for-bug: a vertical
pencil stroke masks only through its disks; :170-194). `scatter_pencil_width`
— `base × 0.01 × max(xMax−xMin, yMax−yMin)` over finite coordinates
(`_getPencilWidth` :532-540, extent from `_adjustColorAndBrushSize`
:318-327), unscaled when the data is empty/all-non-finite (silx
`_data_extent = None`). Tests: `ellipse_test_is_inclusive_and_per_axis`
(boundary points in, unmask clears), `line_masks_a_width_band_as_a_rotated_
rectangle`, `vertical_line_band_degenerates_like_silx`,
`pencil_width_scales_by_one_percent_of_data_extent` (NaN ignored,
empty/all-NaN unscaled). The roadmap-prose contradiction ("full drawing-tool
set", parity-roadmap.md:1537) is now true for the geometric operations; the
panel wiring still drives geometry programmatically (documented on
`show_mask_tools`).

Severity: Medium

Rust: `src/widget/scatter_mask.rs` — zero hits for ellipse/line/pencil; the ScatterView mask panel wiring (`src/widget/high_level.rs:12081-12131`) exposes level/clear/invert/undo/redo/threshold/not-finite plus disk/rect/polygon only.

Reference: `silx/gui/plot/ScatterMaskToolsWidget.py:150-168` — `updateEllipse` (`(px-ccol)²/rc² + (py-crow)²/rr² <= 1.0`, inclusive); `:170-194` — `updateLine` (rotated-rectangle polygon of width `width`); `:528-540` — `_getPencilWidth` scales the pencil width by `0.01 * self._data_extent` (pencil radius in data-extent units).

Impact: scatter masking cannot reproduce silx's ellipse, line, or pencil selections at all. Roadmap frozen rows only ever claimed disk+polygon, but the section prose (`parity-roadmap.md:1537`) claims "the full drawing-tool set" for both mask widgets — the inventory contradicts itself, and the gap is unrecorded as a decision.

### R2-13: Colorbar ticks outside `[vmin, vmax]` are clamped onto the bar ends — labels drawn at wrong value positions

**FIXED (colorbar cluster):** tick placement now goes through a new
`tick_frac` (colorbar.rs) — the silx `_TickBar._getRelativePosition` port
(ColorBar.py:808-820): UNCLAMPED fraction under the colormap normalization,
so nice-number `graphmin`/`graphmax` and log decades outside `[vmin, vmax]`
extrapolate past the bar and are clipped by the widget viewport
(`ui.painter_at(rect)`), never landing on the bar edge with a wrong label.
The silx non-finite fallback is ported: a log tick at `v <= 0` or a gamma
tick whose negative ratio powers to NaN positions at the `vmax` end
(relative position 0.0, :818-819). `clamp_label_center` (a siplot nicety for
in-range edge labels) now applies only when `frac ∈ [0, 1]` — out-of-range
labels extrapolate with their tick line instead of being pulled back onto
the edge. `Colormap::normalize` keeps its clamp: it is the shader mirror for
color lookup, a different meaning by design (dual-meaning removed by giving
the tick bar its own function, not by branching the shared one). Anchor
audit of `\.normalize\(` consumers: chrome.rs:1873 (in-plot colorbar) is
distinct — its generators (`nice_ticks`, `log_decade_ticks`) only emit
in-range values (to a ±step·1e-6 tolerance), so the clamp is unreachable
there; all remaining sites are color/LUT mapping where clamping is correct.
Tests: `tick_frac_is_unclamped_outside_the_range` (plus shader-mirror
still-clamps assertions), `tick_frac_log_decade_below_vmin_extrapolates`
(the vmin=3 "1"-at-3's-position impact case),
`tick_frac_non_finite_norm_lands_at_the_vmax_end` (log ≤ 0, gamma NaN).

Severity: Medium

Rust: `src/widget/colorbar.rs:260` — `paint_tick` places ticks via `self.colormap.normalize(v)`, and `Colormap::normalize` (`src/core/colormap.rs:866`) does `.clamp(0.0, 1.0)`; `paint_ticks_and_labels` applies no out-of-range filter.

Reference: `silx/gui/plot/ColorBar.py:808-843` — `_getRelativePosition` returns `1.0 - (normVal - normMin)/(normMax - normMin)` **unclamped**; out-of-range ticks extrapolate past the bar and are clipped out of view by the widget viewport.

Impact: nice-number layouts routinely emit `graphmin < vmin` (e.g. vmin = 0.13 → tick "0.0"), and the log path emits the decade below vmin plus sub-ticks over the enclosing decades; all of these land exactly on the bar edge labeled with a value that is not the edge value (a log bar with vmin = 3 shows "1" at 3's position while the end label says 3), with sub-tick lines piling on the edges. silx never draws a tick at a wrong position.

### R2-14: ColormapDialog cannot autoscale one bound only — silx has per-bound "Auto scale" (`Colormap` supports `vmin=None` with fixed `vmax`)

**FIXED (this session):** the single `autoscale: bool` gating both bounds is
replaced by per-bound `vmin_auto` / `vmax_auto` (silx `_BoundaryWidget`, one
toggle per bound). The dialog now renders a separate "Auto" checkbox + manual
`DragValue` for each bound (the value disabled while that bound autoscales), and
the autoscale mode/percentiles drive whichever bound(s) are auto. Range
resolution is unified into one per-bound rule (`resolve_range` → pure
`resolve_bounds`): a bound is data-driven when the user set it to autoscale **or**
when a pinned value is invalid under the normalization (silx `getColormapRange`
per-side repair), otherwise the pinned value is kept — so "pin vmax, let vmin
track" (and its inverse) resolves correctly, replacing the former
all-or-nothing autoscale/explicit split. The colormap model half was already
delivered by R2-46 (`Colormap::vmin_auto`/`vmax_auto`); `build_colormap` now
stamps those flags onto the applied colormap and `with_colormap` reads them, so
the per-bound auto state round-trips. Per-boundary tests: both-auto /
pin-vmax-track-vmin / pin-vmin-track-vmax / both-pinned / pinned-invalid-repair
under Log / flags-round-trip. Full siplot suite 1700 green.

Severity: Medium

Rust: `src/widget/colormap_dialog.rs:13,250-262` — a single `autoscale: bool` checkbox gates both bounds (auto → both DragValues replaced; off → both manual); siplot's `Colormap` carries plain `f64` bounds with no half-auto representation.

Reference: `silx/gui/dialog/ColormapDialog.py:111-160` — `_BoundaryWidget` (one per bound) each with its own "Auto scale" toggle; `:1664-1668` — `self._minValue.setValue(vmin or dataRange[0], isAuto=vmin is None)` and same for max, mirroring `Colormap(vmin=None, vmax=...)`.

Impact: the common silx workflow "pin vmax, let vmin track the data" (and its inverse) is unrepresentable in both the dialog and the colormap model.

### R2-15: Arc polar start/end handle drag drops silx's ±180° angle-coherency rule — crossing the branch cut flips the arc to a near-full annulus

**FIXED (roi cluster), angle-coherency half:** new `coherent_angle(previous,
target)` in roi.rs — the silx `_ArcGeometry.withStartAngle`/`withEndAngle`
"Never add more than 180 to maintain coherency" rule
(_arc_roi.py:139-146,162-170): the delta from the previous stored angle is
wrapped into ±π (single correction, as silx) before accumulating, so a drag
across the atan2 branch cut advances 3.0 → ≈3.283 instead of flipping to
−3.08, and stored angles may accumulate beyond ±π exactly like silx geometry
angles. Applied at both arc handles (Vertex 2/3). Anchor audit of raw-atan2
angle writes: ellipse `orientation` handles are distinct (silx `EllipseROI`
also assigns raw `atan2`, no coherency accumulation — orientation has no
sweep pairing); arc *creation* angles in interaction.rs are distinct (silx
creation also assigns fresh `numpy.angle` values; coherency governs edits of
an existing geometry only). Tests: branch-cut crossing both handles +
accumulated-past-π continuation.

**FIXED (roi cluster), radius/weight residual (option (a), user sign-off
2026-07-04):** the adjacent impact — silx stores `(radius, weight)` and clamps
only the *reported* inner radius (_arc_roi.py:856-865), so `weight > 2·radius`
survives follow-up drags — is now closed by restoring silx's `(radius, weight)`
storage. `Roi::Arc` field pair changed from `{ inner_radius, outer_radius }` to
`{ radius, weight }`; two free helpers `arc_inner_radius(radius, weight) =
(radius − weight/2).max(0.0)` (report-only clamp, silx getInnerRadius) and
`arc_outer_radius(radius, weight) = radius + weight/2` (silx getOuterRadius)
derive the reported band at every consumer. This removes the dual meaning that
made the clamp lossy: PolarMode `move_edge` now sets `*radius` from the inner
handle and `*weight = 2·|d − radius|` from the outer handle (silx
`_getWeightFromHandle`), and ThreePointMode `arc_three_point_drag` preserves
`weight` directly instead of recomputing `outer − inner`. Rippled through the
public `Roi` enum, `roi_io` serialization (on-disk geom now `[cx, cy, radius,
weight, start, end]`, lossless), chrome rendering, interaction creation,
`arc_contains`, `roi_manager` "+ Arc", `high_level`/`examples` readouts. New
boundary test `arc_weight_survives_a_reported_inner_radius_clamp`: drag weight
to 14 (reported inner clamps to 0), then drag radius to 8 → weight conserved
(14), inner un-clamps to 1, outer 15 — the old `(inner, outer)` model would
have lost the thickness and reported inner 3 / outer 13.

Severity: Medium

Rust: `src/core/roi.rs:750-751` — `RoiEdge::Vertex(2) => *start_angle = (dy - cy).atan2(dx - cx)` (raw atan2 in (−π, π]), same for the end handle.

Reference: `silx/gui/plot/items/_arc_roi.py:139-146` (`withStartAngle`) and `:162-170` (`withEndAngle`) — "Never add more than 180 to maintain coherency": the delta from the *previous* angle is wrapped into ±π and accumulated, so angles are continuous across the branch cut.

Impact: nudging a start handle from 3.2 rad flips the stored angle to ≈ −3.08, so `end − start` jumps by ~2π and the arc visually inverts (outline and `arc_contains` both use the raw sweep); silx never jumps more than 180° per drag. Adjacent (same handle family): storing only `(inner, outer)` loses silx's independent radius/weight when inner clamps to 0 (silx clamps only the *reported* value, `_arc_roi.py:856-865`), so a follow-up polar drag computes a different thickness.

### R2-16: `XAxisScaleToolButton`/`YAxisScaleToolButton` (linear/log/**asinh**) unported — and no arcsinh *axis* scale exists at all

**FIXED (tool-buttons cluster), buttons half + recorded decision on the asinh
half:** new `AxisScaleToolButton` in tool_buttons.rs (exported at the crate
root) — the silx `XAxisScaleToolButton`/`YAxisScaleToolButton` port
(PlotToolButtons.py:227-380) in the file's established split: pure state core
(`x_axis()`/`y_axis()` constructors, `scale()`/`set_scale` change tracking =
silx `sigScaleChanged → _xAxisScaleChanged`/`_yAxisScaleChanged` mirroring,
silx `STATE` action labels "Linear/Logarithmic X/Y-axis" and tooltips
"X/Y-axis scale is linear/logarithmic") plus an egui `ui` dropdown returning
the chosen scale for the host to apply. **Recorded scope decision — arcsinh:**
the OpenGL backend that siplot ports raises `NotImplementedError` for asinh
axis scales on both axes (BackendOpenGL.py:1555-1571); only the matplotlib
backend implements them. There is therefore no GL reference for an arcsinh
axis transform (tick layout, interaction, rendering), and offering the silx
menu's third entry would be a guaranteed error on the reference backend —
the button offers the two GL-supported states and documents the omission
with the citation; `Scale` stays `Linear`/`Log10`. Tests: default/track
boundaries + the silx STATE label table for both axes.

Severity: Medium

Rust: no counterpart anywhere; `rg asinh` over `src/` hits only colormap normalization; the axis scale enum is `Scale::{Linear, Log10}` only (`src/core/transform.rs:24-29`). Neither the roadmap nor the R1 doc mentions the scale tool buttons or an arcsinh axis scale.

Reference: `silx/gui/plot/PlotToolButtons.py:227-380` — two tool-button classes offering linear/log/asinh axis scales (`"asinh"` state → `axis.setScale(...)`); backed by `silx/gui/plot/items/axis.py:48,68` — `AxisScaleType = Literal["linear","log","asinh"]`, `ARCSINH = "asinh"`.

Impact: an entire axis-scale mode (and its two tool buttons) present in the current silx checkout has no port and no scope-decision record. Caveat, stated: this surface post-dates the frozen inventory (the roadmap's `PlotToolButtons.py` line citations correspond to an older checkout), so it may be new upstream surface — it still needs either a port or a recorded decision.

### R2-17: `SyncAxes` synchronizes limits only — silx's default contract also synchronizes scale and direction

**FIXED (sync cluster):** `SyncAxes` now carries the three silx constructor
aspects — `sync_limits`/`sync_scale`/`sync_direction`, ALL defaulting `true`
("By default everything is synchronized", axis.py:57-66) — alongside the
existing `sync_x`/`sync_y` axis-linking flags (silx builds one `SyncAxes`
per axis list; the aspect flags choose what propagates along the linked
axes, preserving the old `with_sync_x(false)` semantics). One generic
`sync_aspect` helper owns the per-frame contract for every aspect
(first-differing-plot source, propagate to all, remember): limits as before,
plus `x_scale`/`y_scale` (silx `sigScaleChanged → __axisScaleChanged`,
:158-171) and `x_inverted`/`y_inverted` (`sigInvertedChanged →
__axisInvertedChanged`); the first call pushes scale and inverted state from
the first plot too (silx `synchronize()`, :238-241). Tests: scale-syncs-by-
default, direction-syncs-by-default (untouched direction stays put),
first-call pushes scale+direction, aspect flags gate independently while
limits still sync, and an unlinked axis syncs no aspect.

Severity: Medium

Rust: `src/widget/sync.rs:81-139` — `sync` propagates only `plot.limits` (X and/or Y); `x_scale`/`y_scale`/`x_inverted`/`y_inverted` (`src/core/plot.rs:375-381`) are never read or written, though the module doc (`sync.rs:9-11`) claims it "Mirrors silx `SyncAxes`".

Reference: `silx/gui/plot/utils/axis.py:57-66` — `SyncAxes(..., syncLimits=True, syncScale=True, syncDirection=True)` ("By default everything is synchronized"); `:158-171` — `sigScaleChanged → __axisScaleChanged` and `sigInvertedChanged → __axisInvertedChanged` callbacks; `:238-241` — `synchronize()` pushes scale and inverted state too.

Impact: in linked-plot layouts (the ported `syncaxis.py` example scenario), toggling log scale or axis inversion on one plot leaves the others unsynced — silx keeps them locked. The (non-default) syncCenter/syncZoom modes are also absent.

### R2-18: Default grid is Major-on; silx plots start with no grid

Severity: Low

Rust: `src/core/plot.rs:605` — `grid: GraphGrid::Major` in `Plot::new` (and `#[default]` on `Major`); no construction site overrides it.

Reference: `silx/gui/plot/PlotWidget.py:435` — `self._grid = None`; `GridAction` initializes unchecked from it.

Impact: every siplot plot renders a major grid before any user action; silx renders none until toggled. Same shape as R1-16 (unrecorded default divergence) — needs either a fix or a roadmap decision entry.

**FIXED (this session):** moved `#[default]` from `GraphGrid::Major` to `GraphGrid::None` and set `Plot::new`'s `grid` to `None`. The user-facing `set_grid`/`set_grid_minor` toggles keep setting `Major` on turn-on (distinct — those mirror `GridAction`, not the default). Test `grid_defaults_to_none_like_silx`.

### R2-19: Ruler disarm destroys the measurement; silx hides it and reshows it on re-arm

**FIXED (tool-buttons cluster), structurally — ROI visibility is now the
primitive silx actually has:** `ManagedRoi` gains `visible: bool` (default
`true`) = silx `_RegionOfInterestBase.setVisible` (_roi_base.py:479-489),
with a `PlotWidget::set_roi_visible` setter in the established `set_roi_*`
family. A hidden ROI stays in the list but is neither drawn
(`chrome::draw_rois` gate) nor grabbable/translatable
(`interaction::roi_grab_at` gate — silx hides the backing items *and* their
handles, :824). Ruler disarm now does `set_roi_visible(index, active)` in
both directions (the silx `_callback` first line) and keeps `ruler_roi`;
re-arm reshows the previous measurement; the misattributed "(silx deselect)"
comment is gone. Because the hidden line now outlives disarm,
`PlotWidget::remove_roi`/`clear_rois` keep `ruler_roi` consistent across
list mutations (shift-down/clear — the index-based analog of silx's
by-reference `_lastRoiCreated`, mirroring the `current_roi` fix-up in
`Plot::remove_roi`). `visible` is transient in roi_io like `selected` (silx
`toDict` omits both; doc notes added). Distinct, kept: ROI stats rows and
the ROI tables list hidden ROIs (silx rows are registered by manager
membership, not gated on `isVisible`). Tests: disarm keeps the hidden
measurement + re-arm reshows it unchanged, tracking survives other-ROI
removals / self-removal / clear, `roi_grab_at` skips hidden ROIs (body,
handles, and all-hidden → None).

Severity: Low

Rust: `src/widget/high_level.rs:7313-7315` — disarm does `self.remove_roi(index)`; the doc comment (`:7300-7302`) attributes this to "(silx deselect)".

Reference: `silx/gui/plot/tools/RulerToolButton.py:118-122` — `_callback` starts with `self._lastRoiCreated.setVisible(self.isChecked())` — unchecking *hides* the ROI, re-checking reshows the previous measurement; removal happens only on `_disconnectPlot` (`:153-157`) or replacement by a new measurement.

Impact: toggling the ruler off/on restores the last measurement in silx; in siplot it is permanently lost, and the code comment claims a silx behavior silx does not have.

### R2-20: Pixel-histogram default bin count derived from finite-pixel count; silx uses total `array.size`

**FIXED (analysis cluster):** `pixel_intensity_histogram` now guesses
`min(1024, floor(sqrt(pixels.len())))` — the TOTAL element count, NaN/inf
included, exactly silx `guessed_nbins = min(1024, int(numpy.sqrt(
array.size)))` (histogram.py:250); only the range/stats stay
finite-filtered. Doc comment and the drifted roadmap Wave-7C text
(parity-roadmap.md:998, which stated the finite formula while labeling the
port faithful) both corrected. Anchor audit: the ColormapDialog histogram
path (`core/histogram.rs:57`) already used total `data.len()` = silx
ColormapDialog.py:1277 `data.size` — distinct, no change. Recorded for
completeness (unported UI chrome around the same widget, unchanged this
round): silx's integer-dtype `xmax−xmin` bin clamp is a documented N/A
(real-valued pixel model); the "Use weights" checkbox and the 2..9999
bin-spin range remain unported. Test: 100 elements with 36 NaN → 10 bins
(finite-count formula would give 8), 64 counted, range from finite pixels.

Severity: Low

Rust: `src/widget/actions/analysis.rs:279-280` — `guessed = sqrt(finite_count)`, `nbins = guessed.min(1024).max(2)`.

Reference: `silx/gui/plot/actions/histogram.py:250` — `guessed_nbins = min(1024, int(numpy.sqrt(array.size)))` — total element count, NaN/inf included (only the *range* is finite-filtered).

Impact: masked/NaN-bearing images get systematically fewer default bins than silx (50 % NaN → √2 fewer). The roadmap Wave-7C entry states the finite formula while labeling the port faithful — unnoticed drift, not a recorded deviation. (Adjacent unported bits, for the record: silx's integer-dtype `xmax−xmin` clamp is a documented N/A; the "Use weights" checkbox and the 2..9999 spin range are unported and unrecorded.)

### R2-21: Curve CSV export hardcodes an `x,y` header and drops error columns — silx writes the real axis labels plus `*_errors` columns

**FIXED (io cluster):** `curve_to_csv` now takes
`(x, y, xlabel, ylabel, x_error, y_error)` and reproduces the silx
`_saveCurve → _get1dData → save1D` contract: header
`xlabel + "," + ",".join(ylabels)` (io/utils.py:279); column order y →
x-error(s) → y-error(s) (actions/io.py:254-289); a scalar `ErrorBars`
broadcasts to a full `<label>_errors` column
(`numpy.zeros_like(y) + err`), a per-point error is one `<label>_errors`
column, an asymmetric `(2,N)` error splits into `<label>_errors_below`
(row 0) then `<label>_errors_above` (row 1). `save_to_path` resolves the
labels as silx `_getAxesLabels` (:248-252): the curve record's own
`x_label`/`y_label`, else the axis' currently-displayed label; an
unlabeled bare plot writes empty header labels exactly as a bare silx
`PlotWidget` (silx Plot1D's "X"/"Y" come from PlotWindow defaults
siplot never sets). Errors come from the record's retained `CurveData`.
Also ported from the same `_saveCurve` body: the no-active-curve
fallback to the first curve on the plot (`getAllCurves()[0]`, :342-351)
— previously `Ok(false)`. The reduced save surface (CSV-only; no
npy/NXdata/spec) remains the recorded decision. Tests: byte-exact
header+error-column matrix (scalar broadcast, per-point, asymmetric
below/above ordering), real-label + empty-label headers, existing
byte-for-byte `%.18e` rows unchanged.

Severity: Low

Rust: `src/widget/actions/io.rs:79-88` — `String::from("x,y\n")` then zips only `(x, y)`.

Reference: `silx/gui/plot/actions/io.py:248-289` — `_getAxesLabels` (curve label falling back to plot axis label) + `_get1dData` appending `<label>_errors` / `_errors_below`/`_errors_above` columns; `silx/io/utils.py:279` — CSV header = `xlabel + "," + ",".join(ylabels)`.

Impact: exported CSV loses the axis labels and any error-bar data. The reduced save surface (CSV-only) is a recorded decision; the header/error divergence *within* the ported CSV path is not.

### R2-22: Mask pencil anchors cells with `floor()`; silx (and siplot's own rect converter) truncate with `int()`

**FIXED (mask-tools cluster):** the pencil sample now converts through a
`pencil_cell(data_x, data_y)` seam using `as i64` truncation toward zero
(silx `int(col), int(row)`, MaskToolsWidget.py:858), consistent with
`rect_params_to_cells`. Anchor audit of `floor() as i64` cell conversions:
`profile_at_cursor` (high_level.rs) is distinct — silx ImageView gates
`x >= origin` before `int()` (ImageView.py:599-601), so floor+reject and
gate+int agree on every input; the bilinear resampler's `floor` is on
coordinates already clamped to `[0, dim−1]` (positive domain, matches silx
c_funct). Test: `pencil_cell_truncates_toward_zero_like_silx_int`
(interior, −0.5 → edge cell 0, −1.5 → −1, rect-converter consistency).

Severity: Low

Rust: `src/widget/mask_tools.rs:826` — `paint_pencil_point(data_y.floor() as i64, data_x.floor() as i64, ...)`; the same file's `rect_params_to_cells` (`:1992-1999`) deliberately uses `as i64` truncation with a "silx int(), not floor" test note.

Reference: `silx/gui/plot/MaskToolsWidget.py:858` — `col, row = int(col), int(row)` (truncation toward zero).

Impact: differs for negative fractional coordinates — pencil strokes within one pixel outside the top/left image edge anchor at −1 instead of 0, so edge strokes mask fewer border pixels than silx. Also internally inconsistent with the port's own rectangle/polygon converter.

### R2-23: ComplexImageView rebuilds a fresh autoscaled viridis per data/mode change — silx binds one persistent default-gray colormap shared across scalar modes, publicly settable per mode

**FIXED (structural fix + resolved-primitive refinement):**

- *Persistent shared colormap + public surface* — `3bde59a`: replaced the
  per-rebuild fresh viridis with a persistent `colormap` field (gray by default)
  reused across ABSOLUTE/REAL/IMAGINARY/SQUARE_AMPLITUDE, plus `colormap()` /
  `set_colormap()` (silx `getColormap`/`setColormap`, complex.py:125-143,216-233).
  Phase keeps the fixed `[-pi, pi]` hsv colormap.
- *Per-bound range refinement (this session)* — the `3bde59a` fix force-autoscaled
  both bounds every rebuild (`autoscaled`), so a user-pinned range set via
  `set_colormap` was silently overwritten. Now the default is
  `Colormap::autoscale(Gray)` and the scalar path uses per-bound
  `Colormap::resolved` (the R2-46 primitive) through a pure `scalar_mode_colormap`
  helper: an *auto* bound tracks each image (silx `getColormapRange` on
  `vmin/vmax = None`) while a *pinned* bound survives across data and mode changes
  (silx keeps a set `vmin`/`vmax`). Per-boundary tests: default-tracks-image /
  pinned-survives-data-change / phase-ignores-base.

Severity: Low

Rust: `src/widget/complex_image_view.rs:475-486` — `scalar_colormap`: `phase_colormap()` for Phase, else `Colormap::viridis(finite_range(scalar))` recomputed on every rebuild; no `set_colormap` surface exists.

Reference: `silx/gui/plot/items/complex.py:125-143` — one `colormap = super().getColormap()` (ColormapMixIn default = gray, autoscale) is the **same object** for ABSOLUTE/REAL/IMAGINARY/SQUARE_AMPLITUDE; `:216-233` — public `setColormap(colormap, mode)` persists user edits across mode switches.

Impact: default look diverges (R1-16 residual site), and a user cannot set or keep a colormap/range at all — every data or mode change silently re-autoscales.

### R2-24: ColormapDialog editor numerics — gamma clamped to [0.1, 10] vs silx [0.01, 100]; sqrt-normalization histogram range not clipped to min-positive

**FIXED (colormap-dialog cluster):** three sites, one normalization-validity
rule. (1) Gamma `DragValue` range → `0.01..=100.0` (silx
`_gammaSpinBox.setRange(0.01, 100.0)`, ColormapDialog.py:947). (2) The
auto-histogram now computes its range through new `normalized_data_range`
= silx `_computeNormalizedDataRange` (:445-476): `Log`/`Sqrt` →
`(min_positive, max)` (both indexed `[1],[2]` at :455-459),
`Linear`/`Gamma`/`Arcsinh` → finite `(min, max)`; `None` (no histogram)
when no positive value exists under `Log`/`Sqrt` — then
`compute_histogram(px, Some(range), log)` with log-binning only for `Log`,
exactly the `computeHistogram(data, scale=norm, dataRange)` shape. (3) Same
family, uncited: silx clips a *user-set* histogram to the bins whose lower
edge is valid under the normalization before displaying
(`_computeNormalizedHistogram`, :409-418; `is_valid`: Log `> 0`, Sqrt
`>= 0`, math/colormap.py:416-435) — siplot displayed it untouched. New
`first_valid_bin` applies that clip in `draw_histogram_panel` (all-invalid
→ nothing drawn, silx `(None, None)`); the stored histogram stays raw like
silx `_getHistogram`. Tests: range matrix per normalization (min-positive
clip, negatives-only → None for Log/Sqrt, NaN-only → None), lower-edge
validity clip per normalization incl. all-invalid. The gamma range is a
UI-widget bound (GPU/UI path, not unit-tested).

Severity: Low

Rust: `src/widget/colormap_dialog.rs:223-227` — gamma `DragValue ... .range(0.1..=10.0)`; `:155-160` — only `Log` is special-cased for the auto-histogram range, so sqrt uses the full finite min/max.

Reference: `silx/gui/dialog/ColormapDialog.py:947-948` — `_gammaSpinBox.setRange(0.01, 100.0)`; `:451-459` — `_computeNormalizedDataRange` returns `(min_positive, max)` for `SQRT` (as for LOG) when feeding the histogram.

Impact: silx-legal gamma values outside [0.1, 10] are unreachable; with negative data under sqrt normalization the dialog's distribution display and extent differ from silx.

### R2-25: `%.7g` stand-in picks fixed-vs-exponential from the pre-rounding exponent; C/Python `%g` decides after rounding

**FIXED (formatting cluster), structurally — one `%g` owner:** the anchor
`v.abs().log10().floor()` used for a `%g` notation decision had **two**
independent implementations with the same defect — `stats_widget::
format_significant` (cited; the `%g` owner behind `format_g7` → PositionInfo
`format_value` and the stats table) and a duplicate `colorbar::format_g`
(with its own `trim_fixed`/`trim_scientific`, and a latent bug: it kept
Rust's raw `e7` exponent instead of C's `e+07`). Fix: `format_significant`
now formats once via `%e` (which rounds to `digits` sig figs and normalizes
the mantissa) and reads the exponent **back** from that string, so a value
carrying up across a decade is classified on its rounded form —
`9999999.9 → "1e+07"` (was `"10000000"`), `9.9999999e-05 → "0.0001"` (was
`"1e-04"`), both matching `python3 "%.7g"`. `colorbar::format_g` and its two
trim helpers are deleted; `format_end_label`'s `%.7g` branch now routes
through `format_significant`, so its own `< 7`-gate boundary (`9999999.9`,
`log10 ≈ 6.9999999`) now matches silx `"%.7g"` → `"1e+07"`. The `%.2e`
out-of-gate branch keeps Rust's raw exponent (cosmetic, recorded in the
Examined/excluded list — unchanged). Tests: decade-crossing cases in both
directions plus non-crossing controls in `format_significant`; the
`format_end_label` gate + R2-25 boundary in colorbar. Full suite 1639 green.

Severity: Low

Rust: `src/widget/stats_widget.rs:327-331` — `exp = value.abs().log10().floor()`; `if exp < -4 || exp >= digits` — computed on the raw value (used by `format_g7` → PositionInfo `format_value` and the stats table).

Reference: `silx/gui/plot/tools/PositionInfo.py:315` — `"%.7g" % value`; C `%g` selects notation from the exponent *after* rounding to 7 significant digits.

Impact: decade-boundary values format differently — `9999999.9` → siplot `10000000` vs silx `1e+07`; `9.9999999e-05` → siplot `1e-04` vs silx `0.0001`. Affects the PositionInfo readout and every `format_significant` consumer.

### R2-26: `Roi::Line::contains` lacks silx's bounding-box gate — over-reports a strip up to 1 data-unit below/left of the segment

**FIXED (roi cluster):** `Roi::Line::contains` now filters the position
through the endpoints' inclusive axis-aligned bounding box before the
unit-square test, exactly silx `LineROI.contains` (items/roi.py:314-332:
`_BoundingBox.from_points(endpoints)` then `min_x <= x <= max_x &
min_y <= y <= max_y`, image/_boundingbox.py:95-105,123-139). Without the
gate, the unit square anchored at the query point's lower-left crossed the
segment for points up to one unit below/left, past the endpoints. Anchor
audit: `segment_intersects_unit_square` is called only from this one site
(silx applies the bbox filter only in `LineROI.contains`; `HLine`/`VLine`
use exact-coordinate equality, `Polygon` uses point-in-polygon, `Arc`/
`Band` have their own containment — distinct). The cited Rust test
`line_contains_unit_square_intersection` baked in the divergent `True` at
`(4.0, 4.5)` for a horizontal `y=5` segment (endpoint bbox degenerate at
`y∈[5,5]`): corrected to `False`. Added `line_contains_bbox_gate_trims_
beyond_endpoint_strip` for a diagonal segment — `(1.5,1.5)` below-left of
the start endpoint is now excluded (bbox `x,y < 2`) while the in-bbox
upper-right tolerance band `(5.0,4.5)` and the on-segment `(5.0,5.0)` stay
inside.

Severity: Low

Rust: `src/core/roi.rs:885` — `Roi::Line { .. } => segment_intersects_unit_square(*start, *end, pos)` with no pre-filter (the unit square is anchored at the query point's lower-left, so points just below/left of the segment still intersect).

Reference: `silx/gui/plot/items/roi.py:314-332` — `LineROI.contains` first filters positions through `_BoundingBox.from_points(endpoints).contains(...)`, and only then runs `_intersects_unit_square`.

Impact: per-pixel ROI masks (ROI stats over a Line ROI) include a one-unit-wide strip silx excludes; a Rust test bakes in the divergent `True`.

#### Examined and excluded (with reasons)

Ctrl re-evaluated mid-pencil-stroke (capture-once is recorded, roadmap rows 503/1556 — though note silx's *code* re-evaluates per event while its comment says otherwise); Cross/Directed-line profile toolbar arms (Cross display recorded as the row-552 deliverable; Directed-line is the one silx ROI that legitimately uses distance x-coords); `roi_io` dropping `interaction_mode` (internal round-trip loss — silx's ROI dict has no such field; worth a tech-debt note); CompareImages `viridis(0,1)` default at `high_level.rs:8577` (R1-16 sibling, outside category A — flagged for the consolidator); highlighted-ROI stroke `max(w,2)` vs absolute 2, arc/circle tessellation counts, exponent text `1.00e8` vs `1.00e+08`, normalization combo order (cosmetic); PositionInfo readout reset on cursor-leave (host-dependent immediate-mode idiom).

#### Verification note

Every finding independently re-verified at the cited lines on both trees; the roadmap and R1 doc were checked per finding for prior recording — none of the 26 is recorded.

### R2 Category B — plot items, colors, core math (vs silx items/colors/math.fit/ticklayout/sift) [R2-27..R2-45]


### R2-27: FitManager's fit path uses central differences (`left_derivative=True`); the Rust engine is forward-only

**FIXED (fit-stack cluster):** both engines take a `left_derivative: bool`
(silx's keyword, positional): `false` = forward `(f(p+δ) − f(p))/δ`, `true` =
central `(f(p+δ) − f(p−δ))/(2δ)` per leastsq.py:725-733. In the constrained
engine both perturbed vectors pass through the full constraint expansion and
the `derivfactor` scaling, and the final covariance pass inherits the mode
(leastsq.py:496). Callers routed per silx: `IterativeFit::fit_full`,
`fit_multi_gaussian`, and `fit_peak_from` (the FitManager.runfit equivalents)
pass `true` (fitmanager.py:897); the `estimate_multi_gaussian` 4-iteration
refine keeps the forward default (fittheories.py:411-419). Converged fits
barely discriminate the modes (goldens differ ~1.8e-8, below cross-impl
summation noise), so the tests assert the semantics directly by recording the
parameter vectors the model receives:
`central_jacobian_probes_both_sides_and_forward_does_not` (−δ probe present in
central, absent in forward, per parameter),
`central_jacobian_expands_constraints_on_the_minus_probe` (Factor-tied p1
arrives as `2·(p0−δ)` on the minus probe), and
`central_jacobian_tracks_silx_central_trajectory` (silx left_derivative=True
golden: niter 5 exact, params [0.15626324076563153, 1.6095323213968356] at
1e-6).

Severity: Medium

Rust: `src/core/fitting.rs:521-535` (unconstrained) and `:794-812` (constrained) — the Jacobian is always the forward difference `(f(p+δ) − f(p))/δ`; no central-difference mode exists in either engine or any caller.

Reference: `silx/math/fit/fitmanager.py:888-898` — every FitWidget fit calls `leastsq(..., left_derivative=True)`; `silx/math/fit/leastsq.py:725-733` — that flag computes `(f(p+δ) − f(p−δ))/(2δ)`. Only the estimation micro-fit (fittheories.py:411-419) uses the forward default.

Impact: the widget-path Jacobian is O(δ)-accurate where silx's is O(δ²) — different LM trajectory, iteration counts, and converged parameters at the tolerance margin for every FitWidget fit. (Roadmap row 555 records only the constraint-expanded *base evaluation* quirk, not the derivative mode.)

### R2-28: LM iteration budget decrements per lambda attempt in silx, per accepted outer iteration in Rust

**FIXED (fit-stack cluster):** both engines now decrement `iiter` once per inner
damping pass (top-of-pass placement — count-identical to silx's end-of-pass
`iiter -= 1` at leastsq.py:470 since the inner loop never tests `iiter` and the
outer test is `<= 0`), so rejected-λ retries consume the `max_iter` budget. The
Rust-only singular-matrix retry arm is charged the same way (it is modelled as a
rejected step). Golden-verified against silx leastsq.py run directly under
numpy: model `a·exp(b·x)`, `p0 = [1, 3]`, `max_iter = 8` → exactly 5 outer
iterations (three λ rejections billed) with parameters matching to
summation-order noise (~1e-7, numpy pairwise vs Rust sequential accumulation),
and `max_iter = 100` → converges with `niter = 15`, silx's exact trajectory
length. Test: `lm_budget_counts_lambda_attempts_like_silx`.

Severity: Medium

Rust: `src/core/fitting.rs:645` and `:1074` — `iiter -= 1` sits after the inner damping loop, so rejected-λ retries are free.

Reference: `silx/math/fit/leastsq.py:470` — `iiter = iiter - 1` is inside `while flag == 0:` (verified indentation), so every rejected-λ retry consumes the `max_iter` budget.

Impact: under λ rejections Rust runs strictly more outer iterations for the same `max_iter`. Sharpest in the 4-iteration estimation refine (fittheories.py:411-419 ↔ fitting.rs:2460-2471): silx's budget of 4 counts damping retries, Rust's counts 4 full accepted steps → different refined seeds → different final fits.

### R2-29: Peak estimation ignores silx's default strip background (+ Savitzky-Golay pre-smooth); three sites assert a false "off by default"

**FIXED (fit-stack cluster):** the blocking sub-gap is closed —
`core::background` gains `savitsky_golay` (ported from silx's C
`SavitskyGolay` + `smooth1d`, smoothnd.c:53-149: even width promoted to odd,
signed coefficient arithmetic, `npoints/3 + 1` rounds of end smoothing with
the tail window stopping one short of the last sample, `dhelp > 0` write
guard, invalid-width error path returns the input) and
`estimation_strip_bg(y)` = `strip(savitsky_golay(y, 5), w=2, n=5000,
factor=1.0)` (fittheories.py:236-251, DEFAULT_CONFIG
StripBackgroundFlag/SmoothingFlag True at :142-147). Goldens came from silx's
own smoothnd.c compiled directly and driven over the fixtures
(`savitsky_golay_matches_the_silx_c_filter_npoints_5`/`_7`, plus the
positive-sum guard and invalid-width boundaries). `estimate_multi_gaussian`
now computes `bg = estimation_strip_bg(y)` and uses it exactly where silx
does: seed heights `y[peak] − bg[peak]` (:374/:378), ForcePeakPresence argmax
of `y − bg` (:361-364), 4-iteration refine against `yw = y − bg` (:386-387);
the peak search and the caller's final fit keep raw `y`. All three false
"off by default" claims corrected (fitting.rs doc comment,
fit_widget.rs MultiGaussian comment, roadmap row 551). Discrimination
verified: `estimation_seeds_baseline_corrected_heights` and
`forced_peak_is_picked_from_the_stripped_signal` both FAIL when `bg` is
zeroed and pass with the fix.

Severity: Medium

Rust: `src/core/fitting.rs:2412` (`let height = y[pi];` raw), `:2392-2398` (ForcePeakPresence = argmax of raw `y`), `:2459-2471` (4-iter refine against raw `y`). The doc comment at `:2375`, `src/widget/fit_widget.rs:626-627`, and `doc/parity-roadmap.md` row 551 all claim "silx `StripBackgroundFlag` off by default" — factually wrong, so the recorded decision does not stand.

Reference: `silx/math/fit/fittheories.py:142-143` — `DEFAULT_CONFIG` has `"StripBackgroundFlag": True, "SmoothingFlag": True`; `estimate_height_position_fwhm` computes `bg = self.strip_bg(y)` (`:332`), seeds heights `y[peak] − bg[peak]` (`:374/:378`), picks the forced peak from `y − bg` (`:361-364`), and refines against `yw = y − bg` (`:386-387`). `strip_bg` = `strip(savitsky_golay(y, 5), w=2, n=5000, factor=1.0)` (`:236-251`).

Impact: on any data with a baseline, silx seeds baseline-corrected heights and refines against the stripped signal; siplot seeds inflated heights and refines against raw data — different LM starting point for Multi-Gaussian, and a different ForcePeakPresence pick on tilted baselines. Blocking sub-gap: `savitsky_golay`/`smooth1d` (filters.pyx + smoothnd.c) have no Rust counterpart anywhere in `src/`.

### R2-30: `erfc = 1 − erf` collapses to exactly 0 for arguments ≳ 5.9 — hypermet tail terms zeroed where silx keeps relative precision

**FIXED (fit-stack cluster):** replaced BOTH approximations (A&S 7.1.26 `erf`
and the derived `erfc = 1 − erf`) with the fdlibm implementation, vendored
verbatim from rust-lang/libm 0.2.16 `src/math/erf.rs` (the musl port of FreeBSD
msun `s_erf.c`, SunPro notice preserved; local registry copy, no new
dependency). This is the same code behind the libm `erf`/`erfc` silx links on
non-Windows: <1 ulp over the full range, `erfc` relative-accurate into the deep
tail (underflow only past x ≈ 27.2), `erf(0) = 0` / `erfc(0) = 1` exact — so the
step/slit centre-exactness and slit-symmetry tests pass unmodified, unlike the
NR `myerfc` form (silx's `_WIN32` fallback, 1.2e-7 wobble), which was evaluated
first and rejected for breaking those three exactness properties. Audit case
verified: σ=5, slope=0.7, dx=+5 (w=5.7578, formerly erfc→exact 0) now matches
the libm-computed hypermet reference to <1e-9 relative. Tests:
`erfc_keeps_relative_precision_into_the_far_tail`,
`hypermet_tail_survives_the_large_erfc_argument_regime`.

Severity: Medium

Rust: `src/core/fitting.rs:1446-1467` — `erf` is A&S 7.1.26 (absolute error ≤ 1.5e-7) and `erfc(x) = 1.0 - erf(x)`; consumed by the hypermet st/lt/step terms at `:1603/:1609/:1614` and the step/slit models at `:1488/:1506/:1530`.

Reference: `silx/math/fit/functions/src/funs.c:46-49` — `#define erfc myerfc` is `_WIN32`-only; on every other platform `sum_ahypermet` (`:1172/:1183/:1193`) calls libm `erfc` with full relative accuracy down to ~1e-300 (and even Windows' `myerfc`, funs.c:76-90, is the relative-accurate NR rational form).

Impact: hypermet tails are `erfc(w)·exp(z)` with `w = dx/(σ√2) + σ√2/(2·slope)` — the product depends on erfc's *relative* accuracy at large `w`. Measured: +0.67% error at w=5, −100% (exact 0) at w ≥ ~5.9; a short-tail term at σ=5, slope=0.7, dx=+5 reads 24.06 vs silx 20.92 (+15%), and for `σ/slope ≳ 8.5` (reachable under silx's own default bounds, `MinShortTailSlopeRatio=0.5`) the whole tail evaluates to 0 with a zero LM gradient, stalling the tail parameters. Step/slit models see only ≤1.5e-7 absolute — the code comment's "far below fit noise" is false specifically for hypermet.

### R2-31: `get_sigma_parameters` drops the CFACTOR multiplier

**FIXED (fit-stack cluster):** the FACTOR arm of `get_sigma_parameters` now
scales the reference sigma by the tie factor (`sigma_par[i] = factor *
sigma_par[reference]`, silx leastsq.py:875-876); DELTA/SUM keep the unscaled
copy (:877-880). Value expansion (fitting.rs `expand_parameters`) already applied
the factor — only the sigma path had collapsed the three ties. Test:
`factor_tied_sigma_scales_by_the_factor`.

Severity: Medium

Rust: `src/core/fitting.rs:334-341` — `Factor { reference, .. } | Delta {..} | Sum {..} => sigma_par[i] = sigma_par[reference]` — all three collapsed to an unscaled copy.

Reference: `silx/math/fit/leastsq.py:875-876` — `CFACTOR: sigma_par[i] = constraints[i][2] * sigma_par[ref]`; only CDELTA/CSUM copy unscaled (`:877-880`).

Impact: the reported uncertainty of any FACTOR-tied parameter is wrong by the factor — coincidentally exact for factor-1.0 ties, wrong for any user-entered factor via the widget's FACTOR editor.

### R2-32: FitWidget error column shows unconstrained `std_errors()` instead of silx's constraint-propagated `uncertainties`

**FIXED (fit-stack cluster):** the results table now reads the
constraint-propagated `LeastSqResult.uncertainties` via a new
`IterativeFitResult::uncertainties()` accessor (silx shows
`infodict["uncertainties"]`, fitmanager.py:904-909). Identical on the all-Free
path (unconstrained `leastsq` fills `uncertainties = std_errors`), silx-faithful
under QUOTED/FIXED/FACTOR/DELTA/SUM — including the default Multi-Gaussian whose
Positive constraints route through `leastsq_constrained`. Test:
`results_table_errors_use_constraint_propagated_uncertainties`.

Severity: Medium

Rust: `src/widget/fit_widget.rs:950-951` — `self.iterative_result.as_ref().map(|ir| ir.std_errors())` (sqrt of covariance diagonal).

Reference: `silx/math/fit/fitmanager.py:904-909` — `sigmas = infodict["uncertainties"]` → `_get_sigma_parameters` over `cov0` (leastsq.py:517-523): QUOTED gets `|B·cos(p)|·σ`, FIXED shows the parameter value, FACTOR/DELTA/SUM are tied.

Impact: identical on the all-Free path, divergent for every constrained fit — including the default Multi-Gaussian, whose Positive constraints route through `leastsq_constrained`. The silx-faithful value already exists as `LeastSqResult.uncertainties` (fitting.rs:1117-1118); the widget reads the other field.

### R2-33: Non-finite samples abort the widget fit; silx filters them and fits the rest

**FIXED (fit-stack cluster):** the widget's data selection is now one owner,
`fit_ready_data(x, y, range)` — drops any pair with a non-finite member (silx
`_finite_mask = isfinite(x) & isfinite(y)`, fitmanager.py:803-808) and applies
the normalized inclusive fit range in the same pass. Both widget entry points
route through it: `ranged_data` (all `perform_fit_choice` paths) delegates,
and `perform_fit` now fits AND draws over the filtered samples (previously it
passed raw `x_data`/`y_data`, which would also have misaligned the drawn curve
against a filtered `y_fit`). The engines' non-finite rejection stays — silx
`leastsq` itself raises via `asarray_chkfinite`; filtering is the manager
layer's job. Boundary tests: `fit_ready_data_drops_each_non_finite_member`
(either member non-finite ⇒ pair dropped), `fit_ready_data_all_non_finite_yields_empty`,
`fit_ready_data_range_is_inclusive_normalized_and_composes_with_mask`.

Severity: Medium

Rust: `src/widget/fit_widget.rs:575-595` — `ranged_data` filters by x-range only; `leastsq`/`leastsq_constrained` then hard-error on any non-finite sample (`fitting.rs:463-464`, `:897-898`) and the widget renders no fit.

Reference: `silx/math/fit/fitmanager.py:884-885` — `runfit` fits `ydata[self._finite_mask]`/`xdata[self._finite_mask]` (mask built at `:803-808`); estimation filters the same way (`:434-436`).

Impact: a curve containing a single NaN (routine in beamline data) fits normally in silx and silently produces no fit in siplot.

### R2-34: Curve data range excludes error bars

**FIXED (error-bar cluster, this session):** `curve_spec_bounds` now widens the
X and Y extents by the error bars via `error_adjusted_bounds`, a port of silx
`Curve.__minMaxDataWithError` (`items/core.py:1631-1657`): min = `min(vᵢ −
lowerᵢ)`, max = `max(vᵢ + upperᵢ)` over the finite results, a `NaN` error
magnitude contributing the bare `vᵢ` on that side (`dataMinusError[isNanError] =
data`). Magnitudes come from the new `ErrorBars::magnitudes` f64 accessor (the
clip/NaN owner shared with the GPU `bounds`, R2-44), so negative errors are
already clipped to 0 and never widen the range inward. No error ⇒ falls back to
`finite_bounds`. reset-zoom/autoscale now fits the whiskers like silx. Tests:
no-error fallback; symmetric + asymmetric widening; NaN-error bare value;
negative-error clip.

Severity: Medium

Rust: `src/widget/high_level.rs:1923-1929` — `curve_spec_bounds` uses `finite_bounds(spec.x)`/`finite_bounds(spec.y)` only; `x_error`/`y_error` never reach the bounds.

Reference: `silx/gui/plot/items/core.py:1661-1694` — `Curve._getBounds` → `__minMaxDataWithError` (`:1632`, applied at `:1685-1686`): bounds are `min(data − err)` / `max(data + err)`.

Impact: reset-zoom/autoscale clips error-bar whiskers extending past the data extremes; silx fits them in the view.

### R2-35: SIFT match-ratio gate 0.8 (L2) vs silx 0.73² = 0.5329 (L1); the in-code "equivalent" claim is false

**FIXED (SIFT cluster, this session):** the matcher no longer delegates to
`lowe_sift::match_features` (which gates the *L2* ratio at 0.8). A new
`match_features_l1` ports silx's `matching_cpu.cl` `matching` kernel verbatim:
for each query descriptor it tracks the nearest (`dist1`) and second-nearest
(`dist2`) **L1** distances and accepts the nearest when `dist2 != 0 && dist1 /
dist2 < ratio`, with the threshold now `MATCH_RATIO² = 0.73² = 0.5329`
(`MATCH_RATIO = 0.73`, silx `param.py:78`; the squared gate is `match.py:199`).
lowe-sift's f32 descriptors are a global rescaling of silx's uint8 gradient
histograms and that scale cancels in the `dist1/dist2` ratio, so the gate matches
silx up to the detector's own descriptor differences (already an accepted
detector-level divergence). MAXFLOAT-seeded `dist1`/`dist2` mirror the kernel so
a lone train descriptor is accepted. Tests: accept below 0.5329, reject at/above,
`dist2 == 0` guard, single-descriptor accept — plus the full-pipeline translation
recovery still passes under the tighter gate.

Severity: Medium

Rust: `src/core/sift_align.rs:30-33` — `MATCH_RATIO_THRESHOLD: f32 = 0.8` with the comment "silx `MatchPlan` applies an equivalent nearest-neighbour ratio gate"; `lowe-sift` gates the L2 ratio at that value.

Reference: `silx/opencl/sift/param.py:78` — `MatchRatio=0.73`; `match.py:199/:329` pass/apply `MatchRatio²` (0.5329) as the threshold on **L1** distances (kernel doc `matching_cpu.cl:113`: "0.73*0.73 for L1 distance").

Impact: siplot accepts substantially looser matches than silx, so the pair set feeding the affine fit differs and noisy images register differently. Roadmap rows 324/460/1630 record "Lowe ratio 0.8" descriptively without acknowledging silx's 0.73 — not a recorded divergence decision.

### R2-36: SIFT alignment's `< 18` matches shift-only fallback missing — affine fitted from as few as 3 pairs

**FIXED (SIFT cluster, this session):** `sift_auto_align` now ports silx's
fewer-than-3-points-per-DOF fallback. The empty-match guard is now `raw.is_empty()`
(silx returns None only for `len_match == 0`); with `raw.len() <
AFFINE_MIN_MATCHES` (`3 * 6 = 18`) it registers via the new `shift_only_transform`
— an identity linear part plus the median keypoint translation `(median(bx − ax),
median(by − ay))` — instead of least-squares-fitting a 6-parameter affine to a
handful of noisy pairs. The affine fit runs only at ≥ 18 matches. `median`
implements `numpy.median` (odd → middle, even → mean of the two middle). Tests:
`median` odd/even/single/empty boundaries and `shift_only_transform` returns
identity + `(median dx, median dy)` unaffected by an outlier delta.

Severity: Medium

Rust: `src/core/sift_align.rs:227-229` — `if raw.len() < 3 { return None; }`, else always least-squares-fits the full 6-parameter affine.

Reference: `silx/opencl/sift/alignment.py:309-320` — `if (len_match < 3 * 6) or shift_only:` → identity matrix + `offset = (median(dy), median(dx))`; the affine fit runs only with ≥ 18 matches ("3 points per DOF").

Impact: for 3–17 matches silx returns a robust median translation; siplot fits an affine to a handful of noisy pairs and can output scale/rotation silx would never produce on the auto-align path.

### R2-37: TimeSeries bracket ticks drawn outside the axis range — silx culls them

**FIXED (axis-tick cluster, this session):** the `axis_ticks_with_mode`
TimeSeries arm now culls ticks to the visible epoch window `[lo, hi]` before
formatting — `calc_ticks_tz` brackets one tick beyond each end
(`include_first_beyond` in `date_range`), and those are filtered out, so no tick
+ label (or, with grid on, grid line) paints in the frame gutter and the µs
zero-strip in `format_ticks_tz` spans only the visible ticks. Mirrors silx
`GLPlotFrame.py:460-462` (`visibleDatetimes = (dt … if dtMin <= dt <= dtMax)`
then `formatDatetimes` over the visible set); the numeric `nice_ticks` path
already culled (chrome.rs:320), so only TimeSeries leaked. Log/minor paths are
within-range by construction (distinct; log coarsening is R2-39). Tests:
non-aligned window culls the 01-04/01-11 brackets; day-aligned window keeps the
coincident endpoints.

Severity: Medium

Rust: `src/widget/chrome.rs:397-408` — the TimeSeries arm returns `calc_ticks_tz` output unfiltered (the port deliberately brackets via `include_first_beyond`, dtime_ticks.rs:566-584); the grid/tick/label loops (`chrome.rs:566-573`, `:584-597`) iterate all ticks with no `min ≤ v ≤ max` filter on an unclipped painter. The numeric path filters inside `nice_ticks` (`:320`), so only TimeSeries leaks.

Reference: `silx/gui/plot/backends/glutils/GLPlotFrame.py:460-462` — `visibleDatetimes = tuple(dt for dt in tickDateTimes if dtMin <= dt <= dtMax)`; labels (and the µs zero-strip) are computed over the visible set only; the mpl backend culls via the axes viewport.

Impact: on a time axis, one tick + label per end renders in the gutters beyond the plot frame, and with grid on, grid lines are painted outside the frame; the µs zero-strip is also computed over the out-of-range labels.

### R2-38: Linear nice-number tick layout diverges from silx (`/(nTicks)` vs `/(max_ticks−1)`, `<` vs `<=` thresholds, fixed 8/6 vs pixel-adaptive density)

**FIXED — fully closed across this audit's fix rounds; the three parts below
(algorithm divergences, structural consolidation, adaptive density) are all done.**

**Part 1 — algorithm divergences (axis-tick cluster):** the two are closed in
`chrome.rs`:
- Round thresholds now `<=` (silx `niceNumGeneric`'s `frac <= roundFrac` at
  `1.5 / 3.0 / 7.0`), so a frac exactly on a boundary rounds to the lower nice
  number instead of the coarser one.
- `nice_ticks` divides the span by `n_ticks` (silx `vrange / nTicks`), not
  `n_ticks − 1` (silx deviates from Heckbert here). The param is renamed
  `n_ticks` to match. `nice_ticks(0, 100, 5)` now yields step 20 like silx.
Tests: inclusive round boundaries (frac 1.5→1, 3.0→2, 7.0→5) and the `/nTicks`
divisor example.

**Part 2 — structural consolidation (FIXED):** the nice-number
layout that was duplicated four times (`chrome.rs`, `colorbar.rs`,
`core/scene3d/axes.rs`, `core/dtime_ticks.rs`) is now a single exact-silx port
in `src/core/ticklayout.rs` (`number_of_digits`, `nice_num_generic`,
`nice_num`, `nice_numbers`). All four modules delegate to it and keep only their
own tick *generation* (chrome's tolerance loop, axes' `_frange`, colorbar's
`nice_numbers_for_log10`, dtime's per-unit element logic). The `Option<&[f64]>`
`nice_fractions` argument reproduces silx's `niceFractions is None` branch
exactly: `None` uses the hardcoded default round table `(1.5, 3.0, 7.0, 10.0)`,
`Some(list)` averages adjacent fractions (the datetime per-unit path). This also
closes chrome/colorbar's latent `log10()`-vs-`ln/ln` divergence (silx
`niceNumGeneric` uses `math.log(value, highest)`), now uniform. Tests centralised
in the shared module; full siplot suite green (1702).

**Part 3 — adaptive density (FIXED):** ported silx `niceNumbersAdaptative`
(`ticklayout.py:174-192` + `GLPlotFrame.py:414-425`) as
`core::ticklayout::adaptive_n_ticks(physical_len_px)`. Key simplification found in
the reference: silx's `nbPixels = physical_len / devicePixelRatio` and
`tickDensity = 1.3·devicePixelRatio / dpi` multiply to `1.3·physical_len / dpi`,
so the **`devicePixelRatio` cancels** — the density is exactly 1.3 labels per inch
and the only free input is the DPI. That removes the dpr ambiguity; the DPI is
pinned to **92**, which is silx's *own* hardcoded fallback constant
(`GLPlotFrame.py:197`, used whenever no screen is attached), not a fabricated
value — egui exposes `pixels_per_point` (device-pixel ratio) but no DPI, so silx's
fallback is the faithful choice. `draw_axes_with_x_tick_mode` now feeds each axis
its physical pixel length (`Rect` logical extent × `pixels_per_point`) to
`adaptive_n_ticks` **when `x/y_max_ticks` is `None`**; an explicit `Some(n)` cap
(the `high_level` `set_x/y_tick_count` override) is still honoured — silx has no
such override, but keeping it loses no capability. This reverses the former fixed
8/6 defaults, which was the divergence this finding flagged (the audit's Impact:
"density does not adapt to plot size"). `round_ties_even` matches Python
`int(round(…))`. Tests: 1.3-per-92px density (92px→2, 920px→13, 600px→8), the
`max(2, …)` floor (0/10px→2), and round-to-nearest (355px→5, 390px→6). The
`x/y_max_ticks` doc comments (`chrome.rs`, `high_level.rs`, `plot.rs`) now describe
the adaptive `None`. Distinct-and-skipped under the anchor audit: the 3D scene
axes (`core/scene3d/axes.rs`, fixed `nice_numbers(…, 5)`) — silx plot3d uses
non-adaptive `ticklayout.ticks(nbTicks=5)`, zero `niceNumbersAdaptative` hits under
`plot3d/`, so the fixed count is faithful; and the inline colorbar
(`chrome.rs`, fixed 6) — silx's colorbar has its own layout, not GLPlotFrame's.

Severity: Medium

Rust: `src/widget/chrome.rs:306-325` — `step = nice_num(range / (max_ticks - 1), true)` with round thresholds `frac < 1.5 / < 3.0 / < 7.0` (`:284-291`), deployed with fixed defaults 8 (X) / 6 (Y) (`:540/:547`).

Reference: `silx/gui/plot/_utils/ticklayout.py:126-127` — `spacing = niceNumGeneric(vrange / nTicks, isRound=True)` (divisor `nTicks`); `niceNumGeneric` uses `frac <= roundFrac` (`:105`, defaults `(1.5, 3.0, 7.0, 10.0)`); the deployed nticks is pixel-adaptive `max(2, round(1.3·dpr/dpi · nbPixels))` (`GLPlotFrame.py:414-425`, `ticklayout.py:180-189`).

Impact: different tick sets for identical views (e.g. [0,100]: silx nticks=5 → step 20; siplot X → `nice_num(100/7)` → 10); exact-boundary fracs (1.5/3/7) flip to the coarser step; density does not adapt to plot size. Roadmap row 1369 records "nice-number tick layout" as plain done, no deviation noted.

### R2-39: Log axis never coarsens decade ticks (`niceNumbersForLog10` unported in chrome) and returns no ticks for `min ≤ 0`

**FIXED (axis-tick cluster, this session):** all four log divergences closed in
`chrome.rs`. A new `log10_tick_layout` ports silx `niceNumbersForLog10` +
the `dataMin ≤ 0` clamp (`GLPlotFrame.py:371-375`):
- **Coarsening:** for `> LOG_NUM_TICKS` (5) decades, spacing =
  `floor(rangelog / 5)` with the log bounds re-anchored to spacing multiples, so
  a 1e0..1e12 axis now shows 7 ticks (every 2 decades) instead of 13.
- **`min ≤ 0` clamp:** a non-positive lower bound is clamped to 1.0 (and the
  upper pulled up to match) so the axis draws over `[1, max]` instead of blank.
- **Label format:** the data axes now label the exponent via
  `format_axis_log_tick` = silx `"1e%+03d"` (e.g. `1e+02`, `1e-06`, `1e+12`).
  This is split from `format_log_tick` (value labels), which the inline colorbar
  keeps — matching silx, where the GL frame labels exponents and the colorbar
  labels values.
- **Sub-ticks gated on `step == 1`:** `log_minor_ticks` shares
  `log10_tick_layout` and emits the `2..9 × 10^k` sub-ticks (over the clamped
  `[lo, hi]`, dropping the top decade like silx's `frange[:-1]`) only when the
  decade spacing is 1, so a coarsened wide range no longer piles sub-ticks over
  sparse majors.
Tests: coarsening (1e0..1e12 → 7 ticks), `min ≤ 0` clamp to `[1, max]`,
`"1e%+03d"` exponent labels, and sub-tick unit-spacing gating.

(Note: the `niceNumbersForLog10` logic now exists in both chrome and colorbar —
see R2-38's structural follow-up on consolidating the four tick-layout copies.)

Severity: Medium

Rust: `src/widget/chrome.rs:335-343` — `log_decade_ticks` emits every decade in `ceil(log10 min)..floor(log10 max)` and returns empty when `min ≤ 0`; sub-ticks are always drawn (`:453-472`).

Reference: `silx/gui/plot/_utils/ticklayout.py:205-218` — for ranges > nTicks(5) decades, `spacing = floor(rangelog/5)` with bounds re-anchored to spacing multiples; `GLPlotFrame.py:371-375` clamps `dataMin ≤ 0` to 1.0 and still draws; sub-ticks are gated on `step == 1` (`:398`). (The colorbar port at colorbar.rs:567-587 implements this correctly — chrome does not.)

Impact: a 1e0..1e12 axis shows 13 labeled ticks vs silx's ~6 (61 overlapping labels for 1e-30..1e30, with sub-ticks on top); a log axis over non-positive limits renders tickless where silx recovers. Log labels also read "100"/"1e9" instead of silx's `"1e%+03d"` → "1e+02"/"1e+09" (`GLPlotFrame.py:395` vs `chrome.rs:347-353`).

### R2-40: ±inf maps to `nan_color`; both silx pipelines clip infinities into the LUT ends

**FIXED (colormap cluster, this session):** the value→color gate now routes only
`NaN` to `nan_color`; `±inf` survive the normalization clamp and land on the LUT
ends (`+inf` → top color, `-inf` → bottom), matching silx `GLPlotImage.py:202-206`
(`nancolor` for `isnan` only) and `_colormap.pyx:362-376`. Two sites — the family:
`Colormap::color_at` (CPU, feeds every CPU-colormapped item — 3D scatter/mesh/
image/heightmap/cut-plane) changed `!v.is_finite()` → `v.is_nan()`; and the 2D
GPU path `image.wgsl` changed the `[-MAX, MAX]` finite gate to `v != v`
(NaN-only). `normalize`/`normalize_value` already clamp the ratio to `[0, 1]`, so
`±inf` resolve to `1.0`/`0.0`. Degenerate range (`one_over_range == 0`) makes
`0*inf = NaN`: the CPU is safe via Rust's saturating `NaN as usize == 0`, and the
shader adds a `select(value, 0.0, value != value)` guard so the texcoord is never
NaN — both fall back to the low color like silx. The 3D shaders carry no finite
gate (they color on the CPU / sample pre-colored textures), so `color_at` covers
them. Tests: `±inf` → LUT ends, `NaN` → nan_color; degenerate range → low color.

Severity: Medium

Rust: `src/render/shaders/image.wgsl` fs_main — `finite = (v >= -3.4028235e38) && (v <= 3.4028235e38); if (!finite) { return nan_color; }` (the comment claims this mirrors silx); `src/core/colormap.rs:880-886` — `color_at` returns `nan_color` for every non-finite value, feeding all CPU-colored items (`src/render/scene3d_items.rs:239/475/937/1623/2447/...`).

Reference: `silx/gui/plot/backends/glutils/GLPlotImage.py:202-206` — `nancolor` only when `isnan(raw_data)`; ±inf pass through the normalization clamp → +inf hits the top LUT color, −inf the bottom. Same in the CPU path: `silx/math/_colormap.pyx:362-376` — only `isnan(value)` gets `nan_color`; `value <= normalized_vmin → lut[0]`, `>= normalized_vmax → lut[last]` (+inf survives `apply_double` as +inf, `:228-229`).

Impact: saturated/overflow pixels (`+inf`, routine in detector float data) render transparent white (default `nan_color`) instead of the top colormap color, on the 2D image shader and every CPU-colormapped item.

### R2-41: Explicit vmin/vmax invalid under the normalization is not repaired — silx falls back to per-side autoscale, siplot collapses the render

**FIXED (colormap cluster, this session):** the dialog's autoscale-off path now
ports silx `getColormapRange`'s per-side invalid-bound repair. `apply` resolves
the effective range through `resolve_explicit_range`: a bound failing the
normalizer's `is_valid_autoscale_value` (Log `≤ 0`, Sqrt `< 0`) is recomputed
from the active image's data (via the newly shared `autoscale_from_plot`, used
by both the autoscale and explicit paths), keeping the valid side and applying
silx's ordering repairs — `vmin2 = min(fmin, vmax)`, `vmax2 = max(fmax, vmin2)`
(the "handle max ≤ 0 for log" clamp). Both-valid short-circuits untouched. The
per-side selection math is factored into the pure `repair_range` so it is tested
without a GPU-backed `Plot2D`: valid-range-untouched, invalid-lower recovers +
clamps to vmax, invalid-upper `max(fmax, vmin2)` clamp, both-invalid full
autoscale, and Linear-never-repairs (5 boundary cases). Direct construction
`Colormap::new(name, 0.0, max).with_normalization(Log)` without a data source is
out of this fix's reach (no data to autoscale from at construction) and remains
part of the gated autoscale-representability cluster (R2-1/R2-23).

Severity: Medium

Rust: `src/widget/colormap_dialog.rs:348-378` — with autoscale off, `apply` passes `self.vmin`/`self.vmax` straight into `build_colormap`; nothing checks the explicit range against the normalization domain. `Colormap::norm_bounds` (`src/core/colormap.rs:844-852`) then sees `log10(vmin ≤ 0)` non-finite and returns `(0, 0)`, mapping the whole image to the low color.

Reference: `silx/gui/colors.py:711-724` — `getColormapRange` treats an explicit bound failing `normalizer.is_valid` (e.g. `vmin ≤ 0` under log) as `None` and recomputes that side from data (`:726-750`, with `vmax2 = max(fmax, vmin2)` ordering repair). The GL backend therefore always receives a strictly positive log range (`GLPlotImage.py:363`).

Impact: switching the dialog to Log with an explicit `Min: 0` (the default lower bound for counting data), or constructing `Colormap::new(name, 0.0, max).with_normalization(Log)`, renders the entire image as the single low LUT color; silx recovers to `(min_positive, vmax)`. Distinct from R1-9, which fixed only the autoscale computation.

### R2-42: LUT lookup quantization — GPU samples the LUT with linear filtering (silx: GL_NEAREST) and the CPU bins by ×255 (silx: ×256)

**FIXED (colormap cluster, this session):** both halves closed.
- GPU: the 256×1 LUT now samples with a NEAREST sampler (silx GL_NEAREST,
  `GLPlotImage.py:338-347`), so displayed colors snap to LUT entries instead of
  interpolating (registered discrete LUTs stay discrete). The old `lut_sampler`
  was Linear and *also* served the direct-RGBA path's bilinear interpolation
  (gpu_image.rs:881), so it was split: `lut_sampler` (NEAREST, LUT lookup) and a
  new `linear_sampler` (LINEAR, RGBA interpolation) — flipping one no longer
  regresses the other. With ClampToEdge the shader texel is
  `min(floor(coord·256), 255)`.
- CPU: new single owner `Colormap::lut_index` = `min(int(ratio·256), 255)` (silx
  `_colormap.pyx:345-376`, `nb_colors=256`), replacing the four `int(ratio·255)`
  sites — `color_at`, `point_colors`, `colormap_to_rgba`, and the CompareImages
  `red_blue_gray_composite` intensity (silx composes from
  `silx.math.colormap.normalize`, itself `int(ratio·256)`). GPU and CPU now agree
  with each other and silx (ratio 0.5 → 128, was 127). The `Subtract` A−B diff
  channel (`diff·255`, high_level.rs:9678/9680) is a signed-difference→channel
  scaling, not a `normalize`→LUT binning → distinct, skipped. Tests: `lut_index`
  256-binning capped at 255; color_at midpoint 128; composite/point-colors mirror
  the shared owner.

Severity: Low

Rust: `src/render/gpu_image.rs:544-547` — the LUT sampler is `FilterMode::Linear` (min and mag) and `image.wgsl` uses `textureSample(lut_tex, lut_samp, vec2(value, 0.5))`, so displayed colors are interpolated *between* LUT entries; `src/core/colormap.rs:884` (and `src/widget/high_level.rs:9477/:13880/:15120`) — CPU index is `trunc(ratio·255)`.

Reference: `GLPlotImage.py:338-347` — the cmap texture is `minFilter=GL_NEAREST, magFilter=GL_NEAREST`, i.e. texel `trunc(value·256)` clamped; `silx/math/_colormap.pyx:345-376` — CPU `lut_index = int((value − vmin')·(nb_colors/range))` with overflow clamp, the same 256-binning.

Impact: siplot displays colors not present in the 256-entry table (registered discrete LUTs become gradients) and the first/last half-texels differ; on the CPU path roughly half of all values land one LUT entry away from silx's (e.g. ratio 0.5 → index 127 vs 128).

### R2-43: Snip background snips the full array; silx's default anchor split leaves the last two samples raw

**FIXED (background cluster, this session):** added `snip_background_theory`
porting silx `bgtheories.estimate_snip`'s default-anchor path
(`bgtheories.py:229-243`) and pointed `Background::Snip` at it (the raw
`snip_background` filter is kept for `filters.snip1d` callers). With the default
config it uses implicit anchors `[0, n-1]` — snipping the body segment
`y[0:n-1]` and leaving the length-1 tail `y[n-1:]` identity — so index `n-2`
(last sample of the body sub-array, which `snip1d`'s descending-`p` passes never
touch) and `n-1` (the identity tail) both stay at the raw value, exactly like
silx. Verified silx `SmoothingFlag`/`AnchorsFlag` both default to `False`
(`bgtheories.py:78-80`), so no Savitzky-Golay pre-smoothing and no explicit
anchor list apply — the `[0, n-1]` split is the sole case; there is no
additional smoothing gap. Tests: a right-edge peak at `n-2` stays raw in the
background (whole-array snip strips it), the interior is still snipped, and ≤1
sample arrays are identity.

Severity: Low

Rust: `src/core/background.rs:78-95` — `snip_background` runs over the whole array (modifies `1..=n−2`), used by `Background::Snip` (`:234`).

Reference: `silx/math/fit/bgtheories.py:229-243` — with default `AnchorsFlag=False`, `anchors_indices = [0, len−1]`, so `background[0:n−1] = snip1d(y[0:n−1], w)` and `background[n−1:] = snip1d(y[n−1:], w)` (identity); the C `snip1d` on the n−1 sub-array leaves its own last element raw too, so silx keeps **both** `n−2` and `n−1` at raw values and the difference propagates ~`2·width` samples (default SnipWidth 16 → last ~32 samples) through the descending-p passes.

Impact: the Snip background curve diverges from silx over the right-edge region; a peak abutting the right edge is absorbed into the background by silx but stripped by siplot.

### R2-44: Negative error values are not clipped to 0 before drawing

**FIXED (error-bar cluster, this session):** `ErrorBars::bounds(i)` — the single
accessor the draw path (`build_errorbar_segments`, gpu_curve.rs:921/929) *and*
the R2-34 data-bounds path both consume — now clips negative error magnitudes to
`0` via `clip_negative_error` (silx `_filterNegativeValues`,
`items/core.py:1585-1596`, `numpy.clip(v, 0, None)` applied unconditionally to
both error arrays). `NaN` and `+inf` pass through (numpy `clip` leaves them),
matching silx. Structural placement at the shared accessor closes the family:
every current and future error consumer gets non-negative magnitudes, so a
negative entry now draws a suppressed whisker, not an inverted one. Tests:
`error_bounds_clip_negative_magnitudes_to_zero`,
`error_bounds_preserve_nan_and_inf`.

Severity: Low

Rust: `src/render/gpu_curve.rs:906-937` — `build_errorbar_segments` uses raw `(lo, hi)` from `ErrorBars::bounds`; no negative-clip exists.

Reference: `silx/gui/plot/items/core.py:1586-1611` — `_filterData` runs `_filterNegativeValues` on both error arrays unconditionally (`numpy.clip(data, 0, None)`), linear and log alike.

Impact: a negative error entry draws an inverted whisker instead of a suppressed one.

### R2-45: Histogram step outline is 2N+2 points (two hard-coded y=0 end anchors); silx builds exactly 2N and leaves closure to the fill baseline

**FIXED (this session):** `histogram_step_values` now emits exactly `2N` stair
points (two per bin, `(left, count)` / `(right, count)`), matching silx
`_getHistogramCurve` (`items/histogram.py:88-106`). The two `(edge, 0.0)` end
anchors are gone; closure to the baseline is the fill's job (the consumer sets
`baseline = Baseline::Scalar(0.0)` and the fill pipeline draws per-segment quads
down to the baseline, so the removed anchors were zero-width vertical segments
contributing no fill area — fill output is byte-identical, only the two vertical
end segments silx never strokes are dropped from the outline). The outline is now
correct under any baseline, not just 0. The log-range-repair consumer
(high_level.rs:2107) is unaffected (its y=0 points were dropped under log anyway,
and its comment already assumed 2N). Tests updated to the 2N shape.

Severity: Low

Rust: `src/widget/high_level.rs:1161-1173` — `histogram_step_values` pushes `(edges[0], 0.0)` first and `(edges[N], 0.0)` last around the 2N stair points.

Reference: `silx/gui/plot/items/histogram.py:88-105` — `_getHistogramCurve` is exactly 2N stair points; closure to the baseline is the backend fill's job (`baseline` param, `:194`).

Impact: the drawn outline includes two vertical end segments silx never strokes (visible with fill off), and the anchors are pinned to 0 regardless of baseline — coincident today only because `add_histogram_with_align` (`:4247`) hard-codes `Baseline::Scalar(0.0)`; any non-zero baseline desynchronizes outline and fill.

#### Additional minor residuals (below bar, verified — consolidator's discretion)

- equal-bounds QUOTED rejects the whole fit vs silx holding the parameter (`fitting.rs:910-914` ↔ `leastsq.py:673-693`)
- `seek` on regions of ≤ 6 samples returns nothing vs C continuing with `sqrt(data)` significance (`peaks.rs:80-82` ↔ `peaks.c:106-116`, unreachable via the deployed padded path)
- µs tick zero-strip keeps the full label where silx's slice yields empty labels — silx-side bug needing a bug-for-bug decision record (`dtime_ticks.rs:770-784` ↔ `dtime_ticklayout.py:303`)
- linear tick labels cap at 6 decimals with no mpl-style offset text (`chrome.rs:328-331`)
- colorbar scientific threshold 8 chars vs 35 px and `1e3` vs `1e+03` exponent text (`colorbar.rs:502-517/:432/:638` ↔ `ColorBar.py:888-896/:436/:448`)
- Python banker's rounding vs Rust half-away in tick-count derivation (`colorbar.rs:411-413`, `dtime_ticks.rs:633`)
- linear minor ticks are a siplot extension (silx GL draws none)
- REGULAR_GRID 1-row/col scatter collapses the axis scale to 0 (`high_level.rs:11065-11074` ↔ `scatter.py:450-453`)
- binned-statistic drops NaN-valued points instead of NaN-poisoning the bin (`scatter_viz.rs:1155-1158` ↔ `scatter.py:501-513`)
- histogram log-axis bounds cross-filter (`high_level.rs:1876-1882` ↔ `histogram.py:209-243`)
- zero-width model-parameter guards documented in fitting.rs docs vs C NaN/abort
- `decimate.rs:10-11` misattributes its algorithm to silx (silx has no decimation feature)

#### Verified clean (agent's sweep)

LM core (flambda 0.001/×10/÷10/cap 1000, deltachi/epsfcn stops, derivative step, weights, damping, all seven constraint transforms incl. `_get_sigma_parameters` QUOTED strict-bounds quirk, two-pass covariance); all 19 theory models + estimator seed conversions (modulo findings above); sum_gauss..sum_ahypermet formulas; strip.c/snip1d.c cores; peaks.c seek state machine + guess_fwhm + padded_peak_search; bgtheories config defaults; dtime tick tables/formats/DST (modulo R2-37); colorbar tick machinery incl. never-draw-graphmax quirk; colormap LUT contents (gray/temperature/jet/hsv), mask-overlay LUT, autoscale (R1-9 fix verified), dialog histogram binning; scatter grid-detection/quadrilateral/binned math; complex modes incl. `_complex2rgbalog`; histogram edges/revert/pick; median filter default mode; bilinear profile sampling; SIFT pipeline structure and affine decomposition.

### R2 Category C — plot3d scene graph, items, camera (vs silx.gui.plot3d) [R2-46..R2-52]


### R2-46: Every colormapped 3D item defaults to a fixed viridis [0, 1] range — silx defaults to gray with autoscale that tracks the data

**FIXED (colormap-autoscale cluster, two halves):**

- *Palette half* — `61b2872` (earlier this session) already changed all six
  plot3d item constructors from `ColormapName::Viridis` to `ColormapName::Gray`
  (silx `_config.py:58` default colormap name), closing the R1-16 defect at the
  six 3D sites.
- *Autoscale-follows-data half (this session)* — `Colormap` now models per-bound
  autoscale (`vmin_auto`/`vmax_auto`, silx `vmin/vmax is None`). A new
  `Colormap::autoscale(name)` constructs gray + both-bounds-auto, and
  `Colormap::resolved(mode, data)` refreshes *only* the auto bounds from the data
  while preserving pinned bounds and the flags (silx `getColormapRange`, the
  per-bound counterpart of the existing force-both `autoscaled`). All six items
  (`Scatter3D`, `Scatter2D`, `ColormapMesh3D`, `ImageData3D`, `HeightMapData`,
  `CutPlane`) now default to `Colormap::autoscale(Gray)` and re-resolve against
  their value array on every `set_data`/`set_colormap` (silx
  `mixins.py:128-137` `_syncSceneColormap`). The cut-plane colormap is resolved
  through its owner `ScalarField3D` (which holds the volume): `set_data` and the
  new `set_cut_plane_colormap` both drive `resolve_cut_plane_colormap`
  (silx `ScalarFieldView.py:403-405` `_sfViewDataChanged`). The pre-existing
  one-shot `autoscale_colormap`/`autoscale_cut_plane_colormap` (the manual
  Autoscale button) are unchanged. Per-boundary tests: `Colormap::resolved`
  fills-both / pins-one-fills-other / noop-when-pinned, and item-level
  default-is-gray-autoscale / set_data-resolves / pinned-colormap-untouched /
  cut-plane-autoscales-to-volume. Full siplot suite 1687 green (no golden
  changed — no existing test pinned colors on out-of-[0,1] data via the default
  colormap).

Severity: Medium

Rust: `src/render/scene3d_items.rs:113, 314, 831, 1503, 1838, 2227` — `Scatter3D`, `Scatter2D`, `ColormapMesh3D`, `ImageData3D`, `HeightMapData`, and `CutPlane` all construct `Colormap::new(ColormapName::Viridis, 0.0, 1.0)` (each doc-commented "silx defaults"). `Colormap` carries plain `vmin`/`vmax` f64s — there is no autoscale state — and the only range-follows-data paths are the manual one-shot `autoscale_colormap()` / `autoscale_cut_plane_colormap()` (`scene3d_items.rs:167-172, 2632-2641`), which nothing calls on `set_data`.

Reference: `silx/gui/plot/items/core.py:608-609` — every plot3d `ColormapMixIn` item defaults to `Colormap()` = name `gray` (`silx/_config.py:58`), linear, `vmin=vmax=None` (autoscale); `silx/gui/plot3d/items/mixins.py:128-137` — `_syncSceneColormap` pushes `colormap.getColormapRange(self)` whenever data or colormap changes. `ScalarFieldView.py:358-360` — cut-plane colormap `Colormap(name="gray", ..., vmin=None, vmax=None)`; `ScalarFieldView.py:403-405` — `_sfViewDataChanged` re-autoscales on every data change.

Impact: any colormapped 3D item shown with default settings and data outside [0, 1] renders saturated flat color (e.g. a volume in [100, 4000]: the visible cut plane is one solid top-LUT color until the user presses Autoscale; silx shows the full gradient immediately and keeps tracking data updates). The LUT-name half (viridis vs gray) is the exact R1-16 defect at six 3D sites the R1-16 fix (2D `default_colormap` only) did not sweep. The roadmap's recorded "CPU `color_at` at build time" simplification covers the mapping *mechanism*, not the default name/range or the autoscale-follows-data contract; the structural gap is that autoscale is unrepresentable in the 3D colormap binding.

### R2-47: Line, triangle, and mesh pipelines are opaque — silx renders the whole viewport with GL_BLEND, so iso-surface/mesh alpha is dropped (and the iso depth-sort is dead code)

**FIXED (3D cluster, this session):** enabled `PREMULTIPLIED_ALPHA_BLENDING` on
the shared line/triangle pipeline (`make_scene_pipeline`) and the mesh pipeline
in `gpu_scene3d.rs`. silx enables `GL_BLEND` with
`glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)` for the whole scene
(`viewport.py:356-357`), depth-test on and **depth-write on** (no `glDepthMask`
disable), relying on the primitive-level sort for translucent ordering. All
siplot scene geometry carries linear-**premultiplied** RGBA (`egui::Rgba`, the
same convention as the points/image pipelines, which already blend), and both
the `scene3d.wgsl` and `scene3d_mesh.wgsl` fragment shaders output premultiplied
color (mesh: `color.rgb·factor + specular·α`, `apply_fog` scales its target by
`α`) — so premultiplied blending reproduces silx's straight `SRC_ALPHA` over
premultiplied input. Depth-write is left on, matching silx; the `-level`
iso-surface sort (`ScalarField3D::append_raw`, silx `volume.py:659-663`) is now
live rather than dead. Opaque geometry (α=1) writes fully, so no existing golden
changed; the 0.6-α axis tick lines (`axes.py:114`) now composite over non-black
content (they matched over the dark scene background because `rgb·0.6` opaque and
`rgb·0.6 + dark·0.4` coincide there).

Verified with pixel-readback regression tests (`tests/scene3d_blend_render.rs`):
a 50 %-α blue triangle and a 50 %-α blue lit mesh over a bright-green backdrop
now let the green show through (composite), and the triangle test was confirmed
to **fail** (`g=0`, pure blue) when the blend was reverted to opaque. The full
3D render suite (157 tests, incl. the lit-cube / mesh-shading / fog goldens)
stays green.

Note: `ComplexIsosurface`-style *colormapped* translucent surfaces
(`ColormapMesh3D` with per-vertex α) are a separate composition gap tracked under
R2-49; this finding wires the blend contract the existing solid-color meshes and
`Mesh3D`/`ColormapMesh3D` vertex alpha already needed.

Severity: Medium

Rust: `src/render/gpu_scene3d.rs:791-793` — shared line/triangle pipeline `targets: &[Some(target_format.into())]` "blend: None … → opaque write"; `:929-930` — mesh pipeline (iso-surfaces, `Mesh3D`, `ColormapMesh3D`) likewise "Opaque (blend None)". Only points, image quads, and textured meshes blend (`:867, :999`). Meanwhile `ScalarField3D::append_raw` sorts iso-surfaces by decreasing level (`src/render/scene3d_items.rs:2752-2758`) — an order that only matters under alpha blending — and the widget's tick lines are emitted at 60% alpha (`src/widget/scene_widget.rs:360-365`) into the opaque line pipeline.

Reference: `silx/gui/plot3d/scene/viewport.py:356-357` — `Viewport.render` enables `GL_BLEND` with `glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)` for **all** scene content; `silx/gui/plot3d/items/volume.py:659-663` — `_updateIsosurfaces` sorts by `-level` so nested translucent surfaces composite inner-first; `:319-329`/`:728-739` — `Isosurface.setColor` RGBA and `ComplexIsosurface._updateColor` drives `mesh.alpha`; `silx/gui/plot3d/scene/axes.py:114` — tick lines use `color[3] * 0.6`.

Impact: a semi-transparent iso-surface renders fully opaque in siplot — the outer shell hides everything inside — where silx composites; `Mesh3D`/`ColormapMesh3D` vertex alpha is ignored; LabelledAxes tick dashes render at full strength instead of 60%. The Rust code carries both silx-side conventions (the `-level` sort, the 0.6 alpha) whose visible effect the pipelines then discard — internal evidence the blending contract was intended but not wired.

### R2-48: 3D wheel zoom applies silx's fixed 0.2 step once per *frame* of smoothed scroll, not once per wheel *event*

**FIXED (R1-family recurrence batch):** the 3D zoom trigger now reads the raw
`MouseWheel` events (`wheel_zoom_steps`, pure seam in `scene_widget.rs`)
instead of the frame's `smooth_scroll_delta` — one silx-fixed ±0.2 step per
wheel EVENT in delivery order, magnitude-independent (Plot3DWidget.py:407-416
→ interaction.py:340-341), with a re-pick of the anchor depth per step (silx
re-reads the depth buffer on every event). Frames over which egui's
sum-conserving smoothing dribbles a notch deliver no events → no extra steps;
the frame-rate-dependent `0.8^N`-per-notch collapse is gone. Momentum-phase
events are deliberately not filtered (Qt delivers momentum as more
wheelEvents and silx steps on each). Boundary tests:
`one_wheel_event_is_one_step_regardless_of_magnitude` (1-line vs 3-line notch
both = one step), `smoothing_frames_without_events_fire_nothing`,
`multiple_events_step_once_each_in_delivery_order`,
`horizontal_only_wheel_events_do_not_zoom`.

Severity: Medium

Rust: `src/widget/scene_widget.rs:487-494` — `let scroll = ui.input(|i| i.smooth_scroll_delta.y); if scroll != 0.0 … self.camera.zoom_at(ndc, ndc_z, scroll > 0.0)`; `src/core/scene3d/camera.rs:484-486` — every `zoom_at` call moves the camera by the fixed `step = ±0.2` of the distance to the anchor, ignoring delta magnitude.

Reference: `silx/gui/plot3d/Plot3DWidget.py:407-416` — one Qt `wheelEvent` dispatches one `handleEvent("wheel", …)`; `silx/gui/plot3d/scene/interaction.py:340-341` — `_zoomToPosition` applies `step = 0.2 * (1 if angle < 0 else -1)` exactly once per event, magnitude-independent.

Impact: egui's sum-conserving smoothing spreads one discrete notch over N frames, each frame firing a full 0.2 step — one notch multiplies camera-to-anchor distance by 0.8^N (≈0.26 at N=6) instead of 0.8; a macOS trackpad flick with momentum collapses the view onto the anchor in a single gesture. Zoom rate is frame-rate- and platform-dependent. Same per-frame-vs-per-notch family as R1-8 (2D), but the 3D fix needs a per-event (accumulate-and-quantize or raw-event) trigger since silx's 3D step is fixed-per-event, not per-angle.

### R2-49: `ComplexField3D` per-child complex modes missing — no own-mode cut plane, no colormapped `ComplexIsosurface`

**FIXED (this session):** ported both silx per-child complex-mode branches with a
hybrid ownership split that keeps existing behaviour intact.

- **Own-mode cut plane** (silx `ComplexCutPlane`, a `ComplexMixIn`):
  `ComplexField3D` gains `cut_plane_mode: Option<ComplexMode>` with
  `cut_plane_mode()` / `set_cut_plane_mode()`. When set to a mode *different* from
  the field mode, `append_to` slices that mode's projection AND re-ranges an
  autoscale slice colormap to it — silx `_syncDataWithParent` uses both
  `getData(mode)` and `getDataRange(mode)`. `None`, or the field's own mode,
  falls back to the inner field's slice unchanged.
- **Colormapped iso-surface** (silx `ComplexIsosurface` mode ≠ NONE): a new
  `ComplexIsosurface { iso, color_mode, colormap }` type, owned in
  `ComplexField3D::colormapped_isosurfaces`. Its geometry is extracted from the
  **field mode** at the iso's level; each vertex is coloured by trilinearly
  sampling the iso's **own** colour mode (`sample_field_value` = silx `interp3d`)
  and mapping through the iso's `Colormap`, with alpha forced to the iso's alpha
  (silx `mesh.alpha = color[3]`), emitted through `add_mesh_triangle_rgba`.
  `add_colormapped_isosurface` / `colormapped_isosurfaces[_mut]` /
  `remove_colormapped_isosurface` / `clear_colormapped_isosurfaces` manage them;
  `set_complex_mode` clears them alongside the solid iso-surfaces (silx clears all
  iso-surfaces on a mode change); `reproject` re-resolves each iso's auto level
  (field-mode geometry) and autoscale colormap (own colour-mode range) on every
  data/mode change.

Solid iso-surfaces (silx colour mode NONE) stay plain `Isosurface`s on the inner
field — the example's `field_mut().add_auto_isosurface(...)` path is unchanged, so
no `Isosurface` colour-variant churn was needed. `ScalarField3D::append_raw` was
refactored into shared free helpers (`append_solid_isosurfaces`,
`append_colormapped_isosurface`, `append_cut_plane`, plus the zyx→xyz triangle
transforms) that both field types reuse, so no rendering logic is duplicated
(the full existing `ScalarField3D` suite still passes). `ComplexIsosurface` is
re-exported from `lib.rs`. Tests (per invariant boundary): colormap tracks its
own colour mode's range vs a pinned range surviving; re-resolve on a data change;
`set_complex_mode` clears colormapped isos; `cut_plane_mode` default-`None`
round-trip; a colormapped surface emits lit triangles with more than one colour;
the own-mode cut plane reslices+reranges vs same-mode falling back to the field
slice.

Severity: Medium

Rust: `src/render/scene3d_items.rs:2877-2884, 3041-3051` — `ComplexField3D` stores a single `mode: ComplexMode` and projects **one** real field into the inner `ScalarField3D`; the cut plane and every iso-surface can only display that projection, and `Isosurface` (`:2104-2109`) has only a solid `Color32` — no colormapped-surface variant. Module doc (`:2869-2875`) and roadmap P2.3b record only the two amplitude-phase *hue composites* as unported.

Reference: `silx/gui/plot3d/items/volume.py:690-699` — `ComplexCutPlane` is itself a `ComplexMixIn`: `_syncDataWithParent` fetches `parent.getData(mode=self.getComplexMode())`, so the slice can show e.g. PHASE while iso-surfaces sit on ABSOLUTE; `:741-756, 776-801` — `ComplexIsosurface` with mode ≠ NONE extracts the surface from the parent's mode but colours it by *another* mode's values (`interp3d` → `primitives.ColormapMesh3D` with `mesh.alpha = color[3]`) — "iso-surface of amplitude coloured by phase".

Impact: two whole silx display branches of the complex flagship cannot be expressed. Neither is in the roadmap's recorded scope decisions (which cover only the hue composites), so the gap is silent; `ColormapMesh3D` and the trilinear sampler already exist in the port — missing composition, not missing infrastructure.

### R2-50: Gesture depth anchors cannot see the cut plane — `SceneWidget::pick` skips the textured-mesh channel, so orbit/pan/zoom over the flagship's slice anchor at the far plane

**FIXED (3D cluster, this session):** added a textured-mesh channel to
`SceneWidget::pick` — it now intersects the picking segment with every
`textured_meshes` triangle (the cut plane's colormapped slice) and reports a new
`ScenePickKind::TexturedSurface { mesh }`, competing in the same nearest-by-NDC-
depth selection as the data surfaces. This makes the gesture depth read geometry-
complete like silx's item-agnostic depth-buffer read
(`interaction.py:153-156, 228-229, 331` → `viewport._pickNdcZGL`), so the orbit
pivot / pan plane / wheel anchor land on the slice under the cursor instead of
falling back to the far plane. `ScalarFieldView::pick` still combines this with
its own `pick_cut_plane` (for the sampled value); both now find the cut plane at
the same plane∩box intersection, so the nearest-wins reduction is unchanged (its
`pick_hits_the_visible_cut_plane_and_samples_its_value` test still passes). No
production code exhaustively matches `ScenePickKind`, so the new variant is
additive. Tests: a centre ray hits the cut-plane quad (`TexturedSurface`), and
the channel is ordered by depth against a data triangle in both directions
(cut-plane-nearer vs surface-nearer).

Severity: Low

Rust: `src/render/gpu_scene3d.rs:397-400` — `pick_triangles()` documents "Image quads and textured meshes (the cut plane) are excluded"; `SceneWidget::pick` (`src/widget/scene_widget.rs:549-647`) covers triangles, meshes, points, line anchors, and image layers, but never `textured_meshes`. Orbit pivot (`:445-449`), pan plane (`:464-471`), wheel anchor (`:487-493`) all fall back to scene centre / NDC z = 1 on a miss; `ScalarFieldView::show` (`src/widget/scalar_field_view.rs:285-287`) delegates straight to `SceneWidget::show`, never consulting `ScalarField3D::pick_cut_plane` for gestures (only `ScalarFieldView::pick` does, `:256-281`).

Reference: `silx/gui/plot3d/scene/interaction.py:153-156, 228-229, 331` — all three gesture anchors read `viewport._pickNdcZGL(x, y)`, the depth *buffer* under the cursor (`viewport.py:536-…`), which contains every rendered fragment — the cut plane included.

Impact: in the `ScalarFieldView` default interactive state (cut plane visible, no/hidden iso-surfaces under cursor), silx pans 1:1 with the slice pixel grabbed and zooms keeping the slice point invariant; siplot anchors at the far plane — pan translates far too fast and wheel zoom drifts. Distinct from cleared R1-17 (anchor wiring) — this is the pick traversal's negative space, same shape as cleared R1-22 (which added image and LINES channels but not textured meshes).

### R2-51: `CutPlane` has no `displayValuesBelowMin` — values ≤ colormap min cannot be discarded

**FIXED (3D cluster, this session):** added `CutPlane::display_values_below_min`
(default `true`, silx `volume.py:134-150`) with `display_values_below_min()` /
`set_display_values_below_min()`, and wired the discard into
`build_cut_plane_mesh`. When the flag is `false`, a texel whose *normalized*
value is `0.0` (raw ≤ vmin — `Colormap::normalize` clamps below-min to 0, the CPU
mirror of silx's GLSL `clamp(...)`) is emitted fully transparent, matching silx's
`function.py:462-520` `if (value == 0.) { discard; }`. The discard is applied
after normalization but gated on `!value.is_nan()`, so NaN still takes
`nan_color` rather than being punched out — silx places `$discard` before the
isnan branch and NaN never satisfies `value == 0.`. Tests: below-min texels
become transparent only when the flag is off (default draws them opaque), the
above-vmin block core stays opaque, and the flag defaults `true`.

Severity: Low

Rust: `src/render/scene3d_items.rs:2200-2212` — `CutPlane` carries `plane / colormap / interpolation / resolution / visible / stroke_*` only; the slice raster (`build_cut_plane_mesh`, `:2439-2447`) always maps every sample through `color_at`, which clamps below-min values to the low LUT colour. No API in the 3D surface toggles below-min transparency.

Reference: `silx/gui/plot3d/items/volume.py:134-150` — `CutPlane.get/setDisplayValuesBelowMin` (same API on `ScalarFieldView.py:618-634`, the SFViewParamTree "Values<min" row); `silx/gui/plot3d/scene/function.py:498, 462-466, 516-520` — default `True`, and when `False` the colormap GLSL substitutes `if (value == 0.) { discard; }`, punching below-min texels out of the slice.

Impact: default rendering matches, but silx's thresholded-mask mode (hide everything at/below vmin) has no siplot counterpart, and the parameter row it backs cannot be ported. API/param-semantics gap, unrecorded in the roadmap.

### R2-52: Default viewpoint is the "Side" three-quarter face — silx's initial camera is the front view

Severity: Low

Rust: `src/widget/scene_widget.rs:114-117` — `SceneWidget::new` runs `camera.extrinsic.reset(CameraFace::Side)` before framing, comment "Default to the silx 'side' three-quarter view".

Reference: `silx/gui/plot3d/scene/viewport.py:221-223` — viewport camera created at `position=(0, 0, 12)` with `CameraExtrinsic` default `direction=(0, 0, -1)` (`camera.py:50`), i.e. the **front** face; only startup adjustment is `centerScene()` on first render (`Plot3DWidget.py:377-379`); `resetCamera` does not touch direction/up (`camera.py:283-291`). `'side'` exists only as the `resetZoom`/viewpoint-action parameter (`Plot3DWidget.py:342-349`).

Impact: a fresh `SceneWidget`/`ScalarFieldView` opens on the (-1,-1,-1) diagonal where silx opens face-on down -Z; the code comment's "as silx" attribution is wrong (same mis-attribution class as cleared R1-24, which covered colour constants only). Needs either a revert to Front or a recorded deliberate-deviation entry.

**FIXED (this session):** `SceneWidget::new` now `reset(CameraFace::Front)` (was `Side`), matching silx's face-on -Z default camera; the misleading "silx side viewpoint" comments were corrected. 'Side' remains a viewpoint-action preset. Test `fresh_scene_opens_on_the_front_view_like_silx`.

#### Verified clean (agent's sweep, no finding)

camera fit math (`resetCamera` sin/min-fov/depth-extent, orthographic branch, `adjustCameraDepthExtent` 0.95/1.05/zextent-1000) vs camera.py:283-324/viewport.py:385-410; OrbitDrag/PanDrag vs arcball CameraSelectRotate.drag/CameraSelectPan (interaction.py:149-261) incl. π/minsize angle + y-inversion; iso auto-level re-resolve on data change; set_level clears auto; decreasing-level ordering; (min, min_positive, max) finite range; scatter defaults (symbol 'o', size 6.0); NaN → transparent-white; image interpolation default nearest; SFV centerScene-once, setScale/setTranslation re-centering, shininess 32. Not reported because recorded: hue-composite complex modes, ClipPlane, _model/ParamTreeView, Spheres, per-fragment 3D-texture slice, height-map resample quirk, cut-plane 1px stroke, snapshot-less labels.

### R2 Category D — sidm widgets & engine (vs PyDM) [R2-53..R2-60]


### R2-53: SidmSpinbox writes on every step/drag tick; PyDM sends only on Enter (writeOnPress is opt-in and defaults off)

Severity: Medium

Rust: `sidm/src/widgets/spinbox.rs:105-118` — `ui.add(drag).changed()` → `changed.then(|| self.set_value(value))`: every `DragValue` mutation (each arrow step, every frame of a drag) issues a channel `put`.

Reference: `~/codes/pydm/pydm/widgets/spinbox.py:31,55-66,90-91` — `_write_on_press = False` by default; `stepBy` calls `send_value()` **only** `if self._write_on_press`; the value is otherwise sent solely from `keyPressEvent` on `Qt.Key_Return`/`Qt.Key_Enter`.

Impact: stepping a sidm spinbox from 0 to 10 emits ten (or more, when dragged) puts to the control PV where PyDM emits exactly one on Enter — intermediate setpoints are written to hardware that PyDM never sends, and there is no way to get PyDM's compose-then-commit behaviour.

**FIXED (this session):** `show` no longer writes on every `changed()`. Added a pure `should_write(enter_pressed, changed, write_on_press)` gate: PyDM sends on Enter unconditionally (keyPressEvent, spinbox.py:90-91) and on any step only under `write_on_press` (stepBy, spinbox.py:55-66, default false, added as `pub write_on_press` + `with_write_on_press`). The DragValue buffers text edits until commit, so `enter_pressed = response.lost_focus() && key_pressed(Enter)` reproduces compose-then-commit. Test `write_gated_on_enter_unless_write_on_press` (per-boundary matrix), `write_on_press_defaults_off_like_pydm`.

### R2-54: SidmSpinbox default step is `10^-precision`; PyDM's default single step is 1 (`step_exponent = 0`)

Severity: Low

Rust: `sidm/src/widgets/spinbox.rs:97` — `let step = self.step.unwrap_or_else(|| 10f64.powi(-decimals));` (module doc `spinbox.rs:7-8` presents this as the port of `step_exponent`).

Reference: `~/codes/pydm/pydm/widgets/spinbox.py:35,122-127` — `self.step_exponent = 0` at init and `update_step_size` sets `setSingleStep(10**self.step_exponent)` = 1.0, independent of precision; the exponent changes only via Ctrl+Left/Right (`:84-88`, floored at `-self.decimals()`).

Impact: a stock spinbox on a PREC=3 PV steps by 0.001 in sidm vs 1.0 in PyDM — arrow/drag interactions produce different write payloads; PyDM's Ctrl+arrow exponent adjustment and the "Step: 1E{n}" suffix/tooltip (`spinbox.py:143-148`) have no counterpart.

**FIXED (this session):** replaced the raw `step: Option<f64>` (default `10^-precision`) with PyDM's `step_exponent: i32` model — single step is `10^step_exponent`, defaulting to `0` (step = 1.0) independent of precision (spinbox.py:35). Added the pure `stepped_exponent` clamp (floors at `-decimals`, spinbox.py:88), wired Ctrl+Left/Right in `show` while the entry is focused (spinbox.py:84-88), and the `Step: 1E{n}` suffix via `show_step_exponent` (default true, spinbox.py:143-145). Builder `with_step` → `with_step_exponent`. Tests `step_defaults_to_one_independent_of_precision`, `with_step_exponent_sets_the_power_of_ten`, `stepped_exponent_floors_at_negative_decimals`.

### R2-55: Alarm-border default inverted for PushButton/Spinbox/Slider — PyDM ships these three with `alarmSensitiveBorder = False` (and the slider with `alarmSensitiveContent = True`)

Severity: Medium

Rust: `sidm/src/widgets/base.rs:323-331` — `ChannelBase::new` applies `BorderMode::default()` = `Alarm` and `alarm_sensitive_content: false` uniformly; `push_button.rs:86`, `spinbox.rs:38`, `slider.rs:44` all take these defaults unchanged (only `with_border_mode` builders exist; no widget-specific default override).

Reference: `~/codes/pydm/pydm/widgets/pushbutton.py:74` and `~/codes/pydm/pydm/widgets/spinbox.py:29` — `self._alarm_sensitive_border = False`; `~/codes/pydm/pydm/widgets/slider.py:264-265` — `alarmSensitiveContent = True`, `alarmSensitiveBorder = False`. (Frame and Drawing also default border-off, which sidm did port — `frame.rs`/`drawing.rs` per roadmap T1/T4 — so the rule exists but was not applied to these three.)

Impact: on any MINOR/MAJOR/INVALID alarm, sidm draws a 2 px severity ring around every push button, spinbox and slider that PyDM leaves unstyled; conversely PyDM's slider recolours its value label by severity while sidm's slider has no severity-coloured content at all.

**FIXED (this session):** added `ChannelBase::with_border_mode`/`with_alarm_sensitive_content` fluent overrides and applied the per-widget PyDM defaults verified against source — PushButton (pushbutton.py:74) and Spinbox (spinbox.py:29) construct `.with_border_mode(BorderMode::Off)`; Slider (slider.py:264-265) adds `.with_alarm_sensitive_content(true)`. Anchor-audit of `alarm_sensitive_border/content = False/True` across all PyDM widgets confirms the full population is Frame/Drawing (already border-off) + these three. Tests `{push_button,spinbox,slider}_defaults_alarm_border_*`.

### R2-56: Fortran reading order reshapes to the wrong geometry — PyDM makes `width` the first (row) axis, sidm keeps `width` columns and transposes with the wrong stride

**FIXED (R1-family recurrence batch):** `reshape_image`'s Fortran arm now
implements PyDM's actual contract (image.py:108-109): `reshape((width, -1),
order="F")` — `width` becomes the ROW axis, the displayed image is `width`
rows × `len/width` columns with `M[r][c] = data[c·width + r]`, returned as
`(cols, rows, row-major pixels)`. A non-divisible tail drops the partial
COLUMN (documented deviation from numpy's raise). The locking test that
encoded the divergent transpose is replaced by the PyDM golden
(`reshape_fortran_makes_width_the_row_axis_like_pydm`, verified against
numpy directly: len=6/width=3 → `[[d0,d3],[d1,d4],[d2,d5]]`) plus the
partial-column boundary (`reshape_fortran_drops_a_trailing_partial_column`).

Severity: Medium

Rust: `sidm/src/widgets/image_view.rs:63-72` — Fortran branch produces a `height × width` image (same dims as C-like) with `p[r*width + c] = data[c*height + r]`, `height = len/width`.

Reference: `~/codes/pydm/pydm/widgets/image.py:106-109` — `Clike: img.reshape((-1, width), order="C")`; `Fortranlike: img.reshape((width, -1), order="F")`, i.e. a **`width`-row × `(len/width)`-column** image with `M[i][j] = data[j*width + i]`, displayed row-major (`image.py:210` `axisOrder="row-major"`).

Impact: for any non-square image the two disagree in both shape and pixel mapping — e.g. `len=6, width=3`: PyDM Fortran renders 3 rows × 2 cols `[[d0,d3],[d1,d4],[d2,d5]]`, sidm renders 2 rows × 3 cols `[[d0,d2,d4],[d1,d3,d5]]` (the sidm unit test `reshape_fortran_transposes_into_row_major`, `image_view.rs:300-308`, locks in the divergent mapping). Only square images coincide. A PyDM camera screen using Fortranlike shows a different picture in sidm.

### R2-57: SidmImageView defaults diverge — reading order defaults CLike (PyDM: Fortranlike) and colormap defaults Viridis (PyDM: Inferno)

Severity: Low

Rust: `sidm/src/widgets/image_view.rs:26-28` — `#[default] CLike` (justified in the doc comment as "the EPICS areaDetector default", which is not the PyDM contract); `:148` — `colormap: ColormapName::Viridis`.

Reference: `~/codes/pydm/pydm/widgets/image.py:196` — `self._reading_order = ReadingOrder.Fortranlike` is the constructor default; `:185` — `self._colormap = PyDMColorMap.Inferno`.

Impact: an image widget instantiated with defaults renders with a different orientation family (C vs Fortran interpretation of the same flat array — compounding R2-56) and a different palette than the same PyDM widget. Neither default flip is recorded in `doc/pydm-parity-roadmap.md` P4 or the module docs as a deviation (same class as the R1-16 gray-vs-viridis finding).

**FIXED (this session):** moved `#[default]` on `ReadingOrder` from `CLike` to `Fortran` (image.py:196) and changed the `colormap` field default from `ColormapName::Viridis` to `ColormapName::Inferno` (image.py:185), both verified against the PyDM source. Test `reading_order_defaults_to_fortran_like_pydm`.

### R2-58: Scatter and event plots default to the 18000-sample time-plot buffer; PyDM's default for both is 1200

Severity: Low

Rust: `sidm/src/widgets/scatter_plot.rs:164` and `sidm/src/widgets/event_plot.rs:104` — both use `ring_buffer::DEFAULT_BUFFER_SIZE` (= 18000, `sidm/src/widgets/ring_buffer.rs:20`, which is `timeplot.py`'s constant).

Reference: `~/codes/pydm/pydm/widgets/scatterplot.py:12` — `DEFAULT_BUFFER_SIZE = 1200`; `~/codes/pydm/pydm/widgets/eventplot.py:11` — `DEFAULT_BUFFER_SIZE = 1200`.

Impact: a default sidm scatter/event curve retains 15× more points than PyDM before dropping the oldest — after the 1200th sample the two widgets show different data windows (PyDM starts rolling; sidm keeps accumulating to 18000), and memory/draw cost per curve differs accordingly.

**FIXED (7e8ab44):** Added `DEFAULT_SCATTER_EVENT_BUFFER_SIZE = 1200` (`ring_buffer.rs`) and pointed `scatter_plot.rs`/`event_plot.rs` at it; the time plot keeps `DEFAULT_BUFFER_SIZE = 18000`. Test `scatter_event_default_buffer_matches_pydm_not_timeplot` pins both PyDM contracts distinct.

### R2-59: `calc://` plain dialect cannot evaluate PyDM's expression vocabulary — bare `math` names, `np`, `epics_string`, `epics_unsigned` all fail and the failure is silent

**FIXED (R1-family recurrence batch):** the plain dialect now evaluates in the
PyDM calc vocabulary (`pydm_calc_context()` in `calc_plugin.rs`): the bare
`math.__dict__` names PyDM injects (28 unary fns incl. `erf`/`erfc` via
siplot's SunPro port — now `pub use`d from `siplot::core::fitting` — plus
`atan2`/`copysign`/`fmod`/`hypot`/`pow`, two-arity `log`, `ldexp`,
`isnan`/`isinf`/`isfinite`, `isclose`, and constants `pi`/`e`/`tau`/`inf`/
`nan`), plus `epics_string` and `epics_unsigned` (default bits=32, explicit
bits, ≥63-bit float fallback). A `Bytes` char-waveform child now binds as its
NUL-terminated UTF-8 string (the `epics_string` transform applied at binding,
since evalexpr has no byte-array value; `epics_string(A)` is then
identity-on-string so PyDM screens work unchanged). The silent half is closed:
an eval/bind failure logs a warn **once per connection** (PyDM
`logger.exception`s every failure; sidm's 50 ms poll would repeat it
indefinitely) and the warn-once flag is asserted by test. Documented remaining
gaps (all now *visible* as logged eval errors, enumerated on
`pydm_calc_context`): `np`/`numpy` and dotted `math.` spellings, Python's
implicit builtins beyond evalexpr's own, `frexp`/`modf`, iterable
`fsum`/`prod`/`dist`, combinatorics, `gamma`/`lgamma`/`nextafter`/`ulp`/
`remainder`. Tests: `bare_math_names_evaluate_like_pydm`,
`epics_unsigned_reinterprets_negative_ints`,
`bytes_child_binds_as_nul_terminated_string_for_epics_string`,
`eval_failure_warns_once_and_publishes_nothing`.

Severity: Medium

Rust: `sidm/src/data_plugins/calc_plugin.rs:341-357` — the default (non-medm) dialect feeds the expression to evalexpr, whose math builtins are namespaced (`math::sin`, no bare `sin`, no `pi`, no numpy, no EPICS helpers); `eval_with_context(expr, &ctx).ok()?` maps every parse/eval error to `None` = "publish nothing", with no log.

Reference: `~/codes/pydm/pydm/data_plugins/calc_plugin.py:51-53` — `eval_env = {"math": math, "np": np, "numpy": np, "epics_string": epics_string, "epics_unsigned": epics_unsigned}` plus **all** of `math.__dict__` injected bare (`sin`, `cos`, `pi`, `e`, `floor`, …); `:174-179` — `eval(self._expression, env)`, and errors are at least logged via `logger.exception`.

Impact: any PyDM-grammar calc address — `calc://x?expr=sin(A)*2`, `expr=A*pi`, `expr=epics_unsigned(A)`, `expr=epics_string(A)` — evaluates in PyDM but publishes no value ever in sidm: the channel sits connected-but-valueless (the same silent-dead-channel class as R1-25/29) and, unlike the medm dialect's fail-visible warn (`calc_plugin.rs:321-331`), nothing is reported. char-waveform (`Bytes`) children additionally have no binding in the plain dialect (`pv_to_evalexpr` covers scalars only), where PyDM hands the ndarray to `epics_string`.

### R2-60: SidmLineEdit enum-substitutes its display text; PyDMLineEdit shows (and round-trips) the numeric value

**FIXED (sidm cluster, this session):** the enum-label substitution in
`format_default` was shared by `SidmLabel` (which should substitute, PyDM
`label.py:137-141`) and `SidmLineEdit` (which should not, PyDM
`line_edit.py:294-311`). Split the decision by entry point, mirroring PyDM's
per-method structure: `format_value` (enum → label, unchanged callers) and a new
`format_value_for_edit` (enum → numeric index), both delegating to a shared
`format_value_impl` threaded with an `EnumSubstitution` enum; `format_default`
skips the enum branch under `EnumSubstitution::Numeric`. `SidmLineEdit::current_text`
now calls `format_value_for_edit`, so an enum PV shows `"1"` (precision- and
unit-formatted, since the label branch's early-return no longer fires) instead of
`"On"`. The write path is unaffected — `parse_enum` already accepts a numeric
index (`line_edit.rs:269-270`) as well as a label, so the display/parse pair
round-trips. Tests: the label path still shows `"On"`, the edit path shows `"1"`,
and an Int enum channel with units/precision shows `"1.00 V"` (the label branch
would have returned `"On"` before the unit code). Full sidm suite (389 tests)
green.

The sub-bar observations (a)–(e) are consolidator's-discretion adjacent gaps, not
part of this finding; they remain open and unaddressed.

Severity: Low

Rust: `sidm/src/widgets/line_edit.rs:116-118` — `current_text` delegates to `format_value`, whose Default path substitutes the enum label for integer-like values (`display_format.rs:117-120`), so a line edit on an mbbo/bo shows `"On"`.

Reference: `~/codes/pydm/pydm/widgets/line_edit.py:294-311` — `set_display` runs only `parse_value_for_display` (Default returns the value unchanged) then `format_string.format(new_value)` for int/float; unlike `label.py:137-141`, there is **no** `enum_strings` branch, so an enum channel's int index displays as `"1"` (precision-formatted), and `send_value` (`line_edit.py:126`) parses the field back with `int(...)`.

Impact: for the same enum PV the two toolkits show different field text (label vs index); sidm's display/parse pair is self-consistent (labels accepted on write, `line_edit.rs:268-278`) but a PyDM operator procedure phrased in terms of the numeric field content and any pixel-level screen comparison diverge. Undocumented as a deviation in roadmap W3.

#### Sub-bar observations (consolidator's discretion)

(a) `SidmEnumComboBox` lacks PyDM's `_has_enums` enable gate and "Enums not available." tooltip (`enum_combo_box.py:128-151` vs `enum_combo_box.rs:92` — the sidm combo is clickable-but-empty before enum strings arrive)
(b) PyDM's push-button release write reuses the `relative` addition (`pushbutton.py:525-530` via `__execute_send`), while sidm's release is always absolute (`push_button.rs:136-139` — deviation noted in the code comment only)
(c) `PYDM_DEFAULT_PROTOCOL` is an env var in PyDM (`config.py:6-9`) but only a programmatic setter in sidm (`engine.rs:156`)
(d) PyDM's line-edit right-click unit-conversion menu (`line_edit.py:191-242`, `utilities.find_unit_options`) has no sidm counterpart
(e) `str(ndarray)` in a label is space-separated in PyDM (`label.py:150`) vs comma-separated in `display_format.rs:225-235`

#### Verified clean (agent's sweep, no finding)

alarm palette hexes + dashed-disconnected border; display_format.rs numeric/hex/binary/exponential/enum formats (incl. floor-toward-−∞ and Python exponent shape); macro substitution/macParseDefns port; byte-indicator defaults (1 bit / vertical / squares / colors); checkbox >0 / write 1-0; timeplot buffer 18000 + OnValueChange default + 1000 ms fixed-rate default; scale-indicator defaults (10 divisions / pointer / value label); line-edit parse paths (radix, strtobool, unit strip, array round-trip); disconnected-label = channel address; remove_protocol middle-click copy.

### R2 Category E — adl2sidm parse/codegen (vs adl2pydm + MEDM C) [R2-61..R2-69]


### R2-61: Absent `vis` in a dynamic attribute is treated as "if not zero" — MEDM's default is V_STATIC and MEDM never writes the default

**FIXED (MEDM absent-key-default cluster):** `visibility_gate_address`
(`codegen.rs:387`) now resolves an absent `vis` to `"static"` (MEDM's V_STATIC
default), which the existing `if vis == "static" { return None }` maps to no gate —
so a dynamic attribute with a channel but no `vis` (the common `clr="alarm"` +
`chan=…SEVR` alarm-recolour pattern) is always visible, matching MEDM
(`dynamicAttributeInit` → V_STATIC, `writeDlDynamicAttribute` omits `vis` at
V_STATIC, `calcVisibility case V_STATIC: return True`). Corrected the misleading
"(the MEDM default)" comment on the `A#0` expr fallback. Anchor audit of the
`.get(<key>).unwrap_or("<literal>")` family confirmed the other four sites
(`stacking→"row"`, `fill`/`style→"solid"`, `direction→"right"`) fabricate MEDM's
*actual* defaults (ROW / F_SOLID / SOLID / RIGHT) and are correct. No test locked
the wrong default (the gate fixtures — `sample.adl:162`, the CALC fixture — all use
explicit `vis`). Test: `absent_vis_defaults_to_static_not_if_not_zero`.

Severity: High

Rust: `adl2sidm/src/codegen.rs:387` — `let vis = da.get("vis").map(String::as_str).unwrap_or("if not zero");` inside `visibility_gate_address` (`:385-414`); only a literal `vis="static"` returns `None`, so any `"dynamic attribute"` block that carries a channel but no `vis` key gets a `calc://…expr=A#0` gate, and the gate condition (`:365-367`) hides the widget when the channel reads 0.0 — and also while it is disconnected.

C reference: `medm/medmCommon.c:805` — `dynamicAttributeInit` sets `dynAttr->vis = V_STATIC`; `medm/medmCommon.c:1518` — `writeDlDynamicAttribute` writes `vis` **only** `if(dynAttr->vis != V_STATIC)`, so every stock MEDM file with a static-visibility dynamic attribute omits the key; `medm/utils.c:4472-4473` — `calcVisibility` `case V_STATIC: return True` (always drawn). (adl2pydm has the same bug: `output_handler.py:83` `vis = attr.get("vis", "if not zero")` — MEDM C is the contract.)

Impact: the extremely common alarm-coloured shape/text pattern — `"dynamic attribute" { clr="alarm" chan="$(P)$(M).SEVR" }`, no `vis` — converts to a widget that MEDM always shows (recoloured by severity) but sidm **hides whenever the channel value is 0** (NO_ALARM severity ⇒ invisible) and while disconnected. Same wrong gating for `clr="discrete"`+chan and chan-only blocks. This re-opens the R1-33/34 visibility family from the *defaults* side: the expression engine is now right, but the rule is fabricated where MEDM has none.

### R2-62: Strip chart time span — `milli-second` units unscaled (1000× too long), and the omitted-default `period` falls to sidm's 5 s instead of MEDM's 60 s

**FIXED (MEDM absent-key-default cluster):** `strip_chart_span` (`codegen.rs`) now
scales `milli-second`/`milli second` by ×0.001 (`medmStripChart.c:586`), drops the
fabricated `"hour"` unit (not a MEDM unit), and defaults an absent `period`/`units`
to MEDM's stock 60-second window (`SC_DEFAULT_PERIOD 60.0`, `SC_DEFAULT_UNITS
SECONDS`) instead of returning `None` (which left sidm's 5 s). It always emits
`.with_time_span`. Pre-2.1 `delay` (consulted only when `period` is absent, as in
MEDM) is now converted via MEDM's units factor (`0.060`/`60`/`3600` × delay,
`:2140-2160`) with a converter warning that the `linear_scale` nice-rounding is
approximated — closing the silent drop. Test:
`strip_chart_span_scales_units_defaults_and_legacy_delay`.

Residual (documented): the exact `linear_scale` nice-number rounding for `delay`
is approximated, not ported (rare pre-2.1 format; warned).

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:1618-1626` — `strip_chart_span` scales `period` by `Some("minute") => 60.0, Some("hour") => 3600.0, _ => 1.0` and returns `None` when `period` is absent (no `.with_time_span`, so the sidm default applies: `sidm/src/widgets/time_plot.rs:60` `DEFAULT_TIME_SPAN: f64 = 5.0`). The legacy `delay` key is never read (no hit in `codegen.rs`).

C reference: `medm/medmStripChart.c:2105-2108` — parse accepts `"milli second"`/`"milli-second"` → MILLISECONDS; `:586-588` — `timeInterval = period * 0.001 / dataWidth` (milliseconds ×0.001); `"hour"` is not a MEDM unit (units are milli-second/second/minute). `:39-40` — `SC_DEFAULT_PERIOD 60.0`, `SC_DEFAULT_UNITS SECONDS`; `:2211` — `writeDlStripChart` omits `period` when it equals 60.0, so a stock strip chart carries no `period` key. `:2091, 2172-2199` — pre-2.2 files carry `delay`, converted via `linear_scale`. (adl2pydm passes `period` raw as `updateInterval`, `output_handler.py:1065-1068` — also unscaled; MEDM C is the contract.)

Impact: a `period=500, units="milli-second"` chart (0.5 s window in MEDM) converts to a 500 s window; a default-configured MEDM strip chart (no `period` key — the common case) converts to a 5-second window instead of MEDM's 60 seconds; legacy `delay`-format charts silently lose their span too.

### R2-63: Pre-2.2 top-level `basic attribute`/`dynamic attribute` inheritance and the old nested `attr{}` form are dropped — legacy static graphics lose colour, fill, width, and visibility rules silently

**FIXED (full port, user-approved):** the parser now implements the pre-2.2
rolling-attribute format (`parseAndAppendDisplayList`, display.c:475-546).
Gated on `adl_version` < 20200, with a `file` block lacking a `version` key read
as version 0 (MEDM `parseFile` initialises `versionNumber = 0`, medmCommon.c:107).
`parse_children` intercepts top-level `basic attribute`/`dynamic attribute`
blocks (including the ancient `<<basic atribute>>` misspellings, display.c:539-545)
into rolling state: each block RESETS to defaults then parses
(`parseOldBasicAttribute`/`parseOldDynamicAttribute` call the `*Init` first),
keys collected at any depth through the `attr{}`/`mod{}`/`param{}` wrappers. Every
later `rectangle`/`oval`/`arc`/`text`/`polyline`/`polygon` takes the rolling basic
attribute unconditionally (`clr` resolved into the widget colour; init default
clr=0), and the rolling dynamic attribute lands only while its `chan` is set, then
that `chan` is cleared — the consumed-once MEDM 2.2.9 behaviour (display.c:526-529).
The state threads through composite `children{}` in document order (MEDM parses
them via the same function with `static` rolling state). Additionally the
widget-nested attribute lifting for the two attribute carriers now collects keys
at ANY brace depth — MEDM's token parsers never gate key matching on nesting
(`parseBasicAttribute`/`parseDynamicAttribute`), so the nested pre-2.2 wrapper
parses in every MEDM version. The interim warn-only patch (`MedmScreen.warnings`)
was superseded and removed. Tests:
`pre_2_2_rolling_attributes_apply_to_graphics`,
`pre_2_2_rolling_state_threads_composites_and_resets_per_block`,
`widget_nested_old_attr_wrapper_parses_at_any_version`.

Severity: Medium

Rust: `adl2sidm/src/adl_parser.rs:562-568` — `parse()` keeps only top-level blocks whose symbol is in `ADL_WIDGET_SYMBOLS` (`:108-133`), so a top-level `"basic attribute"`/`"dynamic attribute"` block (the old-format carrier) is discarded without a warning; additionally `parse_widget`'s attribute lifting (`:317-329`) reads assignments at nesting level 0 only (`locate_assignments`, `:197-215`), so the old nested `"basic attribute" { attr { clr=… } }` / `dynamic attribute { attr { mod {…} param {…} } }` shape yields an empty attribute map even where the block is widget-nested.

C reference: `medm/display.c:487` — for `versionNumber < 20200` the parser initialises rolling attributes; `:536-546` — top-level `"basic attribute"` (and the misspelled `<<basic atribute>>`) / `"dynamic attribute"` tokens are parsed via `parseOldBasicAttribute`/`parseOldDynamicAttribute` into rolling state; `:515-529` — each subsequent Rectangle/Oval/Arc/Text/Polyline/Polygon **inherits** the last-seen attributes (dynAttr consumed once, basic attr persists). Old write shape: `medm/medmCommon.c:630` and `:1536` (nested `attr {`). adl2pydm drops these blocks the same way; MEDM C is the contract.

Impact: every pre-MEDM-2.2 `.adl` converts with all static graphics in default black-solid (colour/fill/line-width lost) and all old-format visibility/alarm-colour rules gone — silently, no converter warning. Same interop-contract family R1-35 opened (`rdbk`/`ctrl` were fixed; the attribute half of the old format was not).

### R2-64: Related display `visual` never read — "invisible" hotspots render as opaque buttons, row/column-of-buttons render as a menu; entry `policy` misread as `mode`

**PARTIALLY FIXED (silent-drop cluster) — `policy` misread closed:** the per-entry
replace flag now reads the `policy` key with value `"replace display"`
(medmRelatedDisplay.c:666-671, stringValueTable[REPLACE_DISPLAY]="replace display",
verified in the C source). The `.adl` format has no `mode` key — that is MEDM's
internal field name — so `spec.get("mode")` never matched and the replace-mode
deviation warning (`rd_click`) could never fire. Test:
`related_display_replace_flag_reads_the_policy_key_not_mode`.

**FIXED (`visual` rendering port) — no sign-off needed, the C source pins every
choice:** `emit_related_display` now parses `visual` into `RdVisual`
(`related_display_visual`, medmRelatedDisplay.c:728-739; unrecognized/absent →
`RD_MENU`, plus an adl2sidm warning) and branches the emitted egui:

- **`RD_MENU` (default) / single target:** unchanged menu path — one button for a
  single target (MEDM "case 1 of 4", :243, applies to *any* non-hidden visual),
  a `menu_button` dropdown for many. Existing tests/goldens are byte-identical.
- **`RD_ROW_OF_BTN` / `RD_COL_OF_BTN` (`emit_related_display_buttons`):** N
  equal-cell push buttons (`XmNrecomputeSize FALSE`, :461-561) — a row splits the
  width (`ui.put` at cell `x + i·w/N`, full height), a column splits the height
  (`y + i·h/N`, full width); column font sized to the split cell height. Each
  button opens its own target, captioned by that display's `label`. Only ≥2
  targets take this path; a single target stays one button (case 1).
- **`RD_HIDDEN_BTN` (`emit_hidden_related_display`):** no widget and **no fill** —
  a transparent `allocate_rect(.., Sense::click())` hotspot over the geometry so
  the underlying graphic shows through; a click opens the **first** target
  (eventHandlers.c:228-251 opens `display[0]`, never a menu). The prior
  `style_prelude` bclr fill is skipped entirely on this path.

The finding's feared "rendering decisions" are all pinned by the C source (no
menu for a hidden N-target button; equal-cell, not content-sized, layout), so no
sign-off was required. Generated egui is compile-gated by a new self-contained
fixture (`tests/fixtures/rd_visuals.adl` → `rd_visuals_screen.rs`, `include!`d in
`compiles.rs`) that type-checks the `ui.put`/`allocate_rect` cell layout against
real egui 0.34. Tests: `related_display_row_of_buttons_emits_n_filled_cells_not_a_menu`,
`related_display_column_of_buttons_splits_the_height`,
`related_display_invisible_is_a_transparent_hotspot_opening_the_first`,
`related_display_single_target_row_is_still_one_button`,
`related_display_unrecognized_visual_warns_and_uses_menu`,
`rd_visuals_matches_the_committed_module`.

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:2076-2157` — `emit_related_display` chooses plain-button (1 entry) vs `menu_button` (N entries) purely from entry count; no code reads the `visual` key (zero hits in `codegen.rs`), and `style_prelude` (`:2149-2153`) paints the widget's `bclr` as a filled rect over the full geometry. `:2265` — `related_display_entries` reads `spec.get("mode")` for the replace flag.

C reference: `medm/medmRelatedDisplay.c:728-739` — parse of `visual` ("a row of buttons"/"a column of buttons"/"invisible", `displayList.h:451-453`); `:819-821` — written whenever ≠ RD_MENU, so the key is present in exactly the files that need it; `:562-593` — `RD_HIDDEN_BTN` creates **no widget**, drawing only a sparse 4×4 stipple over the underlying graphic (click handled directly); `:461-556` — ROW/COL create N side-by-side buttons. Entry key: `:666-671` parses `policy` ("replace display"), `:778-780` writes `policy=` — there is no `mode` key in the file format. (adl2pydm reads `visual=="invisible"` — `output_handler.py:268-283`, `:410-417` — and `policy` — `:1025`.)

Impact: screens that overlay invisible related displays on graphics (a standard MEDM hotspot idiom) convert to opaque, bclr-filled buttons that cover the graphic; row/column button groups collapse into a single menu button. The `policy` misread means `replace` is never detected, so the replace-mode deviation warning (`codegen.rs:2195-2199`) can never fire.

### R2-65: Text-update/text-entry `format` types beyond `string` silently dropped — `exponential` and `hexadecimal` have exact sidm surfaces

**FIXED (silent-drop cluster):** `string_format_builder` now maps `exponential`
(+ the backward-compat `decimal- exponential notation`) → `DisplayFormat::Exponential`
and `hexadecimal` (+ misspelling `hexidecimal`) → `DisplayFormat::Hex`, keeps
`string`/`$`-suffix → `DisplayFormat::String`, and treats `decimal`/absent as the
fixed-point default (no builder). The formats with no sidm surface — `engr.
notation`, `compact`, `truncated`, `octal`, `sexagesimal`/`-hms`/`-dms` — now emit
a converter warning instead of a silent drop. Tokens verified against MEDM
`displayList.h` stringValueTable[22..32] and the `medmTextUpdate.c:581-600`
backward-compat aliases. Test:
`exponential_and_hex_formats_map_to_sidm_and_the_rest_warn`.

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:2561-2568` — `string_format_builder` maps only `format="string"` (or a `$`-suffixed PV) to `DisplayFormat::String`; every other MEDM format (`exponential`, `engr. notation`, `compact`, `truncated`, `hexadecimal`, `octal`, `sexagesimal*`) falls through to `None` with **no warning**, leaving sidm's fixed-point default. Call sites `:499` (text update), `:526` (text entry).

C reference: `medm/medmTextUpdate.c:300-345` — the runtime format switch renders `EXPONENTIAL` via `cvtDoubleToExpString`, `HEXADECIMAL`/`OCTAL` in that radix, `COMPACT`, `TRUNCATED`, three `SEXAGESIMAL` modes (format strings at `displayList.h:409-418`); parse at `medmTextUpdate.c:567`, `medmTextEntry.c:773`. adl2pydm maps only `string` too (`output_handler.py:1211-1227`). sidm already ships the missing targets: `sidm/src/widgets/display_format.rs:33-46` has `Exponential` and `Hex`, and both `SidmLabel::with_format` (`label.rs:68`) and `SidmLineEdit::with_format` (`line_edit.rs:68`) accept them.

Impact: a `format="hexadecimal"` status word renders as decimal, `format="exponential"` renders fixed-point — numerically misleading displays — even though the two most common non-default formats need only a two-line mapping; the remaining formats at minimum need the converter warning every other unsupported feature gets.

### R2-66: `limits` block source/default resolution misread — `precDefault` applied without its `precSrc` gate, absent `hoprDefault` read as 0.0 instead of MEDM's 1.0, and a single-sided `*Src="default"` overrides both ends

**FIXED (MEDM absent-key-default cluster):** each `limits` bound now resolves from
its own `*Src` (MEDM `medmTextUpdate.c:495-497`, `medmCommon.c:653-666`).
`precision_default_builder` pins precision only when `precSrc="default"` (a bare
`precDefault` is a leftover MEDM ignores → channel PREC), defaulting `precDefault`
to `PREC_DEFAULT` 0 when absent. `user_defined_limits` emits a fixed range only
when BOTH `loprSrc` and `hoprSrc` are `"default"`, with `loprDefault`→LOPR_DEFAULT
0.0 and `hoprDefault`→**HOPR_DEFAULT 1.0** (was 0.0); a single-sided default can't
be split into sidm's all-or-nothing `with_limits` (`user_limits.or(ctrl_limits)`),
so it stays channel-driven and warns instead of forcing both ends. Fixtures with a
bare `precDefault` that were intended to pin precision (`sample.adl`,
`local_panel.adl`, `embed_child.adl`) gained `precSrc="default"` to stay valid MEDM
pinning screens; committed modules regenerated. Test:
`limits_precision_resolves_each_bound_per_its_own_source`.

**FIXED (single-ended residual, structural) — per-bound limit model.** The
former all-or-nothing residual is closed. sidm's numeric widgets
(`SidmSlider`/`SidmSpinbox`/`SidmScaleIndicator`) now hold a per-bound
`UserLimits { lower: Option<f64>, upper: Option<f64> }` instead of
`Option<(f64, f64)>`; `control_range` resolves EACH end independently (user
override when pinned, else the channel `DRVL`/`DRVH`), returning `None` only when
a bound is unavailable. New `with_lower_limit`/`with_upper_limit` builders pin one
end and leave the other channel-driven — a deliberate step beyond PyDM parity
(PyDM's `userDefinedLimits` is all-or-nothing, `reset_limits` bails when either
`userMinimum`/`userMaximum` is `None`) so `adl2sidm` converts single-sided MEDM
`limits` blocks faithfully. `adl2sidm`'s `user_defined_limits` now maps each MEDM
case directly (`both default → .with_limits`, `lower only → .with_lower_limit`,
`upper only → .with_upper_limit`, `neither → nothing`), replacing the warn-and-
drop. Tests: sidm `single_sided_limit_keeps_the_other_end_channel_driven`
(slider + spinbox), adl2sidm `single_sided_valuator_limit_emits_a_per_bound_builder`
plus the extended `user_defined_limits` unit cases (lower-only / upper-only /
absent-default fallbacks).

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:2550-2553` — `precision_default_builder` emits `.with_precision(precDefault)` whenever the key parses, never checking `precSrc` (call sites `:498`, `:525`, `:748`, `:906`). `:2704-2721` — `user_defined_limits` triggers when **either** `loprSrc` or `hoprSrc` is `"default"` and then emits `.with_limits(loprDefault.unwrap_or(0.0), hoprDefault.unwrap_or(0.0))` — both ends forced, missing `hoprDefault` read as 0.0.

C reference: `medm/medmCommon.c:665-666` — `writeDlLimits` writes `precDefault` whenever it differs from `PREC_DEFAULT` (0) **even when `precSrc` stays channel** (`precSrc` itself written only when `== PV_LIMITS_DEFAULT`, `:663`); `medm/medmTextUpdate.c:495-497` — at runtime `prec` comes from the channel's precision unless `precSrc == PV_LIMITS_DEFAULT`. `medm/medmWidget.h:55-57` — `LOPR_DEFAULT 0.0`, **`HOPR_DEFAULT 1.0`**, `PREC_DEFAULT 0`; `medmCommon.c:660-661` omits `hoprDefault` when it equals 1.0, so `limits { hoprSrc="default" }` alone means HOPR = 1.0, and each of lopr/hopr/prec resolves per its own source. (adl2pydm shares the 0.0-default and both-ends bugs, `output_handler.py:1349-1365`, and skips precision per its TODO at `:1345-1348`.)

Impact: a `.adl` carrying a leftover `precDefault=3` with channel-sourced precision converts pinned to 3 decimals where MEDM tracks the PV's PREC; `limits { hoprSrc="default" }` converts to `with_limits(0.0, 0.0)` — a degenerate scale/slider range where MEDM shows 0.0..1.0; a widget that defaults only HOPR loses its channel-driven LOPR.

### R2-67: `clrmod="alarm"` silently ignored on every controller — MEDM alarm-colours text entry, message button, menu, choice button, valuator, and wheel switch

**FIXED (alarm wiring, structural) — controllers now colour by severity like the
monitors.** The cross-crate builder the finding flagged is now present on all six
sidm controllers, and the converter wires it:

- **sidm (single owner).** `ChannelBase::framed_alarm_content` is the one place
  that installs MEDM's `XmNforeground = alarmColor(severity)` rule: it sets
  `override_text_color` from `content_color(state)` (a no-op unless
  `alarm_sensitive_content` is set and the channel is in a coloured alarm). Every
  egui text widget the controllers use — `Button`, `RadioButton`, `TextEdit`,
  `Slider`/`DragValue` — honours `override_text_color`, so `SidmLineEdit`,
  `SidmPushButton`, `SidmEnumButton`, `SidmSlider`, `SidmSpinbox` all route their
  content through it. `SidmEnumComboBox` paints its face caption by hand, so it
  reads `content_color` directly into that galley instead. All six gain
  `with_alarm_sensitive_content` + `with_alarm_palette` builders (previously only
  the four monitor widgets had them). Tests: `base.rs`
  `framed_alarm_content_overrides_foreground_only_when_alarm_sensitive` (headless
  render proves the override is installed only when sensitive) plus a per-widget
  builder test on each controller.
- **adl2sidm.** The six emitters now `extend(alarm_content_builder(widget))`
  (`clrmod="alarm"` → `.with_alarm_sensitive_content(true).with_alarm_palette(AlarmPalette::Medm)`,
  the same string the monitors already emit — one source of truth), replacing the
  interim `warn_controller_alarm_clrmod` (removed). The valuator is special:
  `SidmSlider` ships `alarm_sensitive_content` ON (PyDM parity), so a MEDM valuator
  *without* `clrmod="alarm"` gets `valuator_alarm_builder` → an explicit
  `.with_alarm_sensitive_content(false)` so it does not alarm-colour with no MEDM
  basis. Tests: `clrmod_alarm_on_a_controller_wires_severity_content`,
  `valuator_without_alarm_clrmod_turns_off_default_content_sensitivity`; the
  regenerated `sample_screen.rs`/`local_panel_screen.rs` goldens compile the
  slider builder against real sidm through the `compiles.rs` gate.

MEDM's palette rides along (`AlarmPalette::Medm` → `NoAlarm` = Green3, total
replacement), so the controllers match the monitor widgets exactly — the symmetry
the finding asked for.

Severity: Medium

Rust: `adl2sidm/src/codegen.rs` — `alarm_content_builder` (`:2582-2588`) is applied only to text update (`:500`), byte (`:836`), and scale indicators (`:879`); the controller emitters — text entry `:520-546`, message button `:550-583`, menu `:586-607`, choice button `:612-665`, valuator `:671-721`, wheel switch `:725-770` — never read `clrmod` and emit **no warning** when it is `"alarm"`. (Root cause partly cross-crate: sidm exposes `with_alarm_sensitive_content` only on `SidmByteIndicator`/`SidmDrawing`/`SidmLabel`/`SidmScaleIndicator`.)

C reference: MEDM colours the control's foreground by severity under `clrmod=ALARM` at runtime: `medm/medmTextEntry.c:418-424` (`XmNforeground = alarmColor(pr->severity)`), `medmMessageButton.c:348`, `medmMenu.c:540`, `medmChoiceButtons.c:375`, `medmValuator.c:892-895`, `medmWheelSwitch.c:390`. (adl2pydm drops controller clrmod too.)

Impact: operator screens that rely on a text entry / menu / message button turning red on MAJOR alarm lose that indication entirely on conversion, with no converter warning — asymmetric with the monitor widgets, where the same MEDM key is faithfully wired (R1-33-family fix). Closing fully needs the sidm builder on the controller widgets; at minimum the silent drop should warn.

### R2-68: Cartesian plot runtime surface silently dropped — `trigger`, `erase`, `eraseMode`, `countPvName`, `style`, `erase_oldest`

**FIXED (silent-drop cluster):** `warn_unsupported_cartesian_keys` now warns for
each runtime key with no sidm surface (verified against MEDM's parse,
medmCartesianPlot.c:2957-3070): `trigger` (redraw gating), `erase`+`eraseMode`
(plot-clear PV), a non-numeric `count`/`countPvName` (PV-driven buffer size),
`style` when `point plot`/`step`/`fill under` (rendered as a connected line), and
`erase_oldest` circular/stop-at-n buffering. sidm's plot is a live, full-array,
auto-scaling line plot, so `line plot`/`line` and a numeric `count` (the scatter
buffer, already handled) are faithful and stay silent. Behaviour is unchanged —
these keys were and remain unimplemented; the fix is that the drop is no longer
silent, matching every other unsupported-feature path in the emitter. Test:
`cartesian_plot_warns_on_unsupported_runtime_keys`.

Severity: Low

Rust: `adl2sidm/src/codegen.rs:1425-1535` — `emit_cartesian_plot` reads only `count` (numeric, scatter-buffer only), the traces, plotcom, and x/y1/y2 axis blocks; none of the six keys above is read anywhere in `codegen.rs`, and no warning is emitted for any of them (a non-numeric `count` — the PV-name form — also silently disappears through `parse::<usize>().ok()`).

C reference: `medm/medmCartesianPlot.c:3043-3068` — parse of `trigger` (plot updates only when the trigger PV posts), `erase` + `eraseMode` (`if not zero`/`if zero`), and `countPvName` (`count` may name a PV, `:2957-2963` stores it as a string); `:2964-2994` — `style` (`point plot`/`line plot`/`step`/`fill under`) and `erase_oldest` (`plot last n pts` circular vs `plot n pts & stop`); `:439-466` — erase/trigger wired as live records at execute time. (adl2pydm's `write_block_cartesian_plot`, `output_handler.py:687-775`, ignores all six as well.)

Impact: a triggered cartesian plot converts to one that redraws on every waveform update; erase-PV screens lose their clear function; `style="point plot"` renders as connected lines; a PV-driven count degrades to the default buffer — all without any converter warning, unlike every other unsupported-feature path in the emitter.

### R2-69: Wheel-switch `format` only parsed in adl2pydm's `w.d` form — MEDM's documented printf form (`"% 6.2f"`) falls back to channel precision

**FIXED (silent-drop cluster):** `wheel_decimals` now parses MEDM's real printf
spec the way the Xc `WheelSwitch` widget does (`WheelSwitch.c:1347-1391`): find
`%`, require an `f` conversion after it, skip flags (`+`/` `/`#`/`0`/`-`), read
`w.p`, and clamp `p` to `[0, w-1]`; a width-only printf (`% 6f`) yields 0 decimals.
The bare `w.d` (`format="6.2"`) and `"integer"` conveniences still resolve, and a
truly unparseable value (`"% 6d"`, no `f`) still returns `None` so the caller warns
rather than silently dropping. Test: `wheel_decimals_reads_medm_printf_and_bare_forms`.

Severity: Low

Rust: `adl2sidm/src/codegen.rs:2762-2767` — `wheel_decimals` handles `"integer"` and `w.d` (`fmt.split_once('.')?.1.parse::<i32>()`); for a printf-style value the fraction part is `"2f"`, the parse fails, and the emitter warns "precision left to channel" (`:740-747`).

C reference: `medm/medmWheelSwitch.c:664-667` — the token is stored raw and handed to the Xc widget as `XmNformat`; `medm/xc/WheelSwitch.c:44` — `DEFAULT_FORMAT "% 6.2f"` (the documented printf shape), `:1348-1355` — the widget parses the format by locating `%` and the trailing `f`, so `"% 6.2f"` means exactly 2 decimals regardless of PREC. (adl2pydm makes the same `w.d` assumption, `output_handler.py:1178-1197`.)

Impact: any wheel switch whose `.adl` carries the real MEDM printf format displays with the channel's PREC instead of the format's decimals whenever the two differ (warned, not silent — but the decimals are recoverable by stripping the `%`-prefix/`f`-suffix before the `w.d` split).

#### Verification notes

Choice-button `stacking` row/column orientation checked against `medmChoiceButtons.c:131-140` (XmVERTICAL for ROW) and matches; `"row column"` grid stacking degrades to a warned vertical stack (warned, not silent — not reported). Valuator `dPrecision`→display-precision is a recorded roadmap decision, skipped. No source files modified.

## Round 3 Open Findings (R3-1..R3-24)

Convergence check, 2026-07-05. Baseline: post-R2-fix-round + post-flake-fix
HEAD `0a51e4e` (all 109 prior findings R1-1..R1-40 / R2-1..R2-69 closed). Same
5-agent split, scopes rotated to surfaces R2 left thin, with a mandatory
**fix-verification lens**: every R2 fix in git range `227cdec..HEAD` was
re-read line-by-line against the exact upstream lines it claims to port (test
skepticism applied to fixes, not just to the port). All 69 R2 fixes verified
faithful *at the cited site*; the residuals that verification surfaced — a fix
that closed the cited line but not the sibling in the same reference function,
or whose doc note mis-classified the remaining half — are the R3 findings tagged
"(RN residual)" below.

### R3-1: Toolbar grid toggle turns on the major grid only — silx PlotWindow's GridAction sets `"both"` (major + minor) — (R2-18 residual)

**FIXED (R3, commit after `bd37b11`):** both user-facing binary grid toggles
(toolbar grid button `high_level.rs`, LimitsWidget "Show Grid" checkbox
`limits_widget.rs`) now go `None <-> MajorAndMinor` to match silx's deployed
`GridAction(gridMode="both")`. The toolbar's minor sub-button still drops to
major-only, so siplot stays a strict superset. Internal `set_graph_grid(bool)`
setup calls (ImageView / side histograms) keep major-on/off — a distinct site,
not the deployed action.

Severity: Low

Rust: `src/widget/high_level.rs:6606-6611` — grid button calls `self.set_graph_grid(grid)`; `:7135-7141` — `set_graph_grid(true)` sets `GraphGrid::Major`. Minor grid is a separate opt-in button (`:6613-6623`) silx's toolbar does not have.

Reference: `silx/gui/plot/PlotWindow.py:198-199` — `GridAction(self, gridMode="both", parent=self)`; `silx/gui/plot/actions/control.py:293,314` — `_actionTriggered` → `setGraphGrid(self._gridMode if checked else None)`: one click shows major AND minor.

Impact: silx's grid button yields major+minor; siplot's yields major only. The R2-18 FIXED note classified the toggles as distinct because they "mirror `GridAction`" — wrong for the deployed `gridMode="both"`, so this escaped the R2-18 sweep.

### R3-2: `pixel_intensity_histogram` mean/std/sum drop ±inf — silx `nanmean`/`nanstd`/`nansum` skip only NaN and propagate infinities — (R2-20 residual)

**FIXED (R3):** `analysis.rs` now computes mean/std/sum over the non-NaN pixels
(a second pass gated on `!v.is_nan()`) instead of the finite-only pass, so ±inf
propagates (sum/mean → ±inf, std → nan) exactly as `numpy.nanmean/nanstd/nansum`
do; the range/reset stays finite-filtered (`min_max(finite=True)`). The stale
test `histogram_excludes_non_finite` — which asserted the divergent finite-only
`sum==6.0`/`mean==1.5` — was replaced by `histogram_stats_propagate_inf_while_
range_excludes_it` (+inf → inf/inf/nan) and `histogram_stats_skip_nan_and_stay_
finite`.

Severity: Low

Rust: `src/widget/actions/analysis.rs:245-276` — stats accumulate over `v.is_finite()` pixels only, while docs at `:209-214`/`:238-240` claim these are silx `numpy.nanmean`/`nanstd`/`nansum`.

Reference: `silx/gui/plot/actions/histogram.py:296-298` — `mean=numpy.nanmean(array)`, `std=numpy.nanstd(array)`, `sum_=numpy.nansum(array)` over the raw array: NaN skipped but ±inf poisons the result (mean/sum → ±inf, std → nan). Only min/max are finite-filtered (`min_max(..., finite=True)`, `:247`).

Impact: an image with ±inf pixels shows finite mean/std/sum in siplot where silx displays `inf`/`nan`. Same finite-vs-nan-filter family R2-3/R2-11 swept; this sibling was outside both. Counts unaffected (±inf falls outside the range in both).

### R3-3: Pixel-histogram stats line formatted with Rust `{:.4}` (fixed 4 decimals) — silx uses `%.5g` — (R2-25 residual)

**FIXED (R3):** the histogram stat line now renders each of min/max/mean/std/sum
with `format_g_python(v, 5)` — silx's `f"{value:.5g}"` (`_StatWidget.setValue`,
histogram.py:95): 5 significant digits, %g fixed/exponential switching. Because
R3-2 makes mean/std/sum reachably ±inf/nan and silx's `%.5g` prints CPython's
`"inf"`/`"-inf"`/`"nan"` there (not `format_significant`'s `"--"`, which is the
`PositionInfo` convention), a thin `format_g_python` wrapper over
`format_significant` supplies the non-finite spellings. Test
`format_g_python_matches_python_5g`.

Severity: Low

Rust: `src/widget/high_level.rs:8881-8884` — `format!("min {:.4} max {:.4} mean {:.4} std {:.4} sum {:.4}", ...)`.

Reference: `silx/gui/plot/actions/histogram.py:95` — `_StatWidget.setValue`: `f"{value:.5g}"` (5 sig digits, `%g` fixed/exponential switching).

Impact: small stats collapse to `0.0000` (silx `1.2345e-05`), large ones print all integer digits + 4 decimals (silx `1.2346e+08`). Bypasses the R2-25 single-`%g` owner — `format_significant(v, 5)` (stats_widget.rs) is the byte-faithful port and already exists; this is the last `{:.4}` site in high_level.rs.

### R3-4: Stats-table coordinate cells render `%.7g` comma-joined — silx renders `str(tuple)` (parens, 1-tuple trailing comma, full repr precision)

**FIXED (R3):** `format_coord` now renders coordinate stats as silx's
`str(tuple)` fallback (`StatFormatter.format` finds no `numbers.Number` →
`str(val)`, statshandler.py:81-84): a 1-tuple `(x,)` for curve coordinates and
a 2-tuple `(x, y)` for image coordinates, each element via a new
`python_repr_float` that reproduces CPython `repr(float)` — shortest
round-tripping decimal, trailing `.0` for integral values, `%g` exponential
switch at decpt `<=-4`/`>16` — so `0.12345678901` renders in full, not `%.7g`'s
`0.1234568`. The `%.7g` `format_g7`/`coord_component` pair (the divergence) was
removed.

**Anchor sweep:** the sibling `roi_stats_widget::format_coord` (silx
`ROIStatsWidget` shares the same `StatFormatter`/`StatCoord` path) carried the
same defect *plus* `{:.3}` components, a bare (paren-less) 1-tuple, and a
single-dash `-` for undefined; it was unified onto the shared
`stats_widget::format_coord` so both tables render identically. The third
`chrome::format_coord` is an axis tick-label formatter `(v, lo, hi)`, not a
stat cell — distinct, untouched.

Severity: Low

Rust: `src/widget/stats_widget.rs:281-309` — `format_coord`/`coord_component` emit `"2.5"` / `"1, 3"` with `%.7g` components; the doc comment claims the `.7g` float formatting silx applies to coordinate components, citing `PositionInfo.py:310-312`.

Reference: `silx/gui/plot/stats/statshandler.py:71-84` — `StatFormatter.format`: `"{0:.3f}"` applies only to `numbers.Number`; coordinate stats (StatCoordMin/Max, StatCOM, `stats/stats.py:864-877`) always return tuples, which fall through to `str(val)` → `"(2.5,)"` / `"(1.0, 3.0)"` at full float repr. `PositionInfo.valueToString` belongs to the position readout bar, not the stats table.

Impact: cosmetic cell divergence (no parens/trailing comma, 7-sig-fig truncation vs full repr) plus a false reference attribution in the doc comment that would mislead the next round.

### R3-5: TimeSeries axis tick count hardcoded to 5 — the R2-38 adaptive-density fix covered only the numeric sibling — (R2-38 residual)

**FIXED (R3):** the `TickMode::TimeSeries` arm of `axis_ticks_with_mode` now
passes the adaptive `max_ticks` to `calc_ticks_tz` instead of the removed
`TIME_SERIES_NUM_TICKS = 5`, so time axes get the same pixel-density count
(1.3 labels/inch) the numeric arm does — silx runs the identical
`calcTicksAdaptive` density path for both (`GLPlotFrame.py:450-459`). The
microseconds regime (span `<= ~2 s`, `bestUnit == MICRO_SECONDS`) reduces the
density to 1.0 label/inch before the count is taken (`GLPlotFrame.py:451-457`);
this is applied at the caller (where cap-vs-adaptive is resolved, next to
`adaptive_n_ticks`) via the new `adaptive_n_ticks_density` +
`TICK_LABELS_PER_INCH_MICROSECONDS`, so it fires only for the adaptive (uncapped)
TimeSeries-on-linear path. The reference guards the µs case with
`if bestUnit((dtMax - dtMin).total_seconds() == DtUnit.MICRO_SECONDS)` — a
mis-parenthesized silx bug (it tests `bestUnit(bool)`); the port applies the
*intended* condition `best_unit(span) == MicroSeconds`.

Tests: `adaptive_n_ticks_density_reduces_count_in_microseconds_regime` (1.0
density < 1.3 for the same width) and `axis_ticks_time_series_honors_max_ticks_
count` (15 vs 5 budget yields different counts — fails on the old const-5 arm).

Severity: Low

Rust: `src/widget/chrome.rs:405` (`TIME_SERIES_NUM_TICKS = 5`) consumed at `:447` where the `TickMode::TimeSeries` arm of `axis_ticks_with_mode` ignores the `max_ticks` parameter the numeric arm honors; the adaptive count from `:604-607` (the f75f601 R2-38 fix) never reaches time axes.

Reference: `silx/gui/plot/backends/glutils/GLPlotFrame.py:450-459` — time-axis ticks are computed pixel-adaptively via `calcTicksAdaptive(dtMin, dtMax, nbPixels, tickDensity)`; `_utils/dtime_ticklayout.py:423-427` gives `nticks = max(2, int(round(tickDensity * axisLength)))`; GLPlotFrame reduces density 1.3→1.0 labels/inch in the microseconds regime.

Impact: a time-series X axis always shows ~5 ticks regardless of width while a numeric axis in the same widget adapts (~17 at 1200 px). Wide time plots under-labelled, narrow over-labelled, µs density reduction absent. Internal inconsistency introduced by R2-38 rewiring only the numeric arm.

### R3-6: `Colormap::resolved` omits `getColormapRange`'s unconditional ordering clamps — inverted pinned ranges collapse the render — (R2-14/41/46 residual)

**FIXED (R3):** both per-bound resolvers now apply silx `_getColormapRange`'s
ordering-clamp tail (colors.py:739-748), so the auto side is always clamped
against the *pinned* opposite and the result satisfies `vmin <= vmax`:

- `Colormap::resolved` (colormap.rs): after autoscale, `vmin = min(amin, vmax)`
  when vmin is auto and vmax pinned, `vmax = max(amax, vmin)` when vmax is auto —
  instead of filling only the auto side with the raw data bound. Pin `vmax = 2`
  over data `[3, 90]` with vmin auto now yields the ordered `(2, 2)`, not the
  inverted `(3, 2)`. Every consumer (3D `scene3d_items`, `ComplexImageView`, the
  dialog) is fixed transitively through this single owner.
- `colormap_dialog::resolve_bounds` (colormap_dialog.rs): rewritten to be the
  sole owner of the silx tail. The former split — fill the auto side raw, then
  clamp only *invalid* pins via `repair_range` — left a genuinely-auto but
  *valid* data bound on the wrong side of a pinned opposite unclamped. `resolve_
  bounds` now treats "auto" as user-auto OR normalization-invalid (silx's `None`
  switch) and applies `min`/`max` uniformly; the now-redundant `repair_range`
  helper was removed and its per-side-repair tests folded into `resolve_bounds`
  cases.

Tests: `resolved_clamps_auto_bound_against_a_pinned_wrong_side_bound` and
`resolve_bounds_clamps_auto_bound_against_pinned_wrong_side` (both directions),
plus the migrated invalid-repair cases.

Severity: Medium

Rust: `src/core/colormap.rs:1032-1045` — `Colormap::resolved(mode, data)` early-returns when both bounds are pinned and otherwise fills only the auto side(s) with raw `amin`/`amax`, applying no cross-bound ordering clamp and keeping a normalization-invalid pinned bound. `src/widget/colormap_dialog.rs:458-462` (`resolve_bounds`): the auto-filled bound is never clamped against the pinned opposite; `repair_range` (`:410-428`) cross-clamps only when the pinned value is invalid under the normalization, not when it is merely on the wrong side of the data range.

Reference: `silx/gui/colors.py:735-748` (`_getColormapRange`): after per-side repair, ordering clamps run unconditionally on any auto side — `vmin2 = fmin if vmax is None else min(fmin, vmax)` (`:740-741`), `vmax2 = max(fmax, vmin2)` (`:745-746`) — so the returned range always satisfies `vmin2 <= vmax2`.

Impact: pin `vmax=2.0` with data `[3,90]`, vmin auto: silx returns degenerate `(2,2)`; siplot produces inverted `(3,2)`, violating the `vmax>vmin` precondition (`colormap.rs:930-938`) so `norm_bounds` returns `(0,0)` and the whole item renders the low color. Hits the dialog and every `resolved` consumer (3D items, CompareImages, ComplexImageView). R2-41/14/46 fixed the per-bound autoscale primitive but never ported the ordering-clamp tail of the same function.

### R3-7: 3D interactive-mode surface missing — button bindings match no silx composition, no Ctrl swap, doc misattributes them to RotateCameraControl

Severity: Medium

Rust: `src/widget/scene_widget.rs:9-14` — module doc claims a `RotateCameraControl` port with "right-drag pans"; `:445-461` binds Primary to orbit, `:467-484` binds Secondary to pan, unconditionally. No keyboard-modifier handling exists (only `Modifiers::NONE` in a test, `:828`); no `set_interactive_mode`/`interactive_mode` surface.

Reference: `silx/gui/plot3d/scene/interaction.py:448-469` — `RotateCameraControl` = `CameraWheel` + `CameraSelectRotate(LEFT_BTN)`; its ctrl set is `CameraSelectPan(LEFT_BTN)` — the right button is bound to nothing. Pan is reached via Ctrl (FocusManager swap on `Key_Control`, `:435-441`) or by switching to `PanCameraControl` through `Plot3DWidget.setInteractiveMode('rotate'|'pan'|None)` (`Plot3DWidget.py:178-219`, default `'rotate'`). The one two-button composition, `CameraControl` (`:495-511`), is LEFT pan + RIGHT rotate — the mirror of siplot's binding.

Impact: silx's documented gesture contract is unreachable — Ctrl+left orbits instead of panning, there is no pan mode (trackpad/single-button users have no equivalent), and right-drag does something silx never does. The module doc presents the fabricated binding as the port. Not recorded in `doc/plot3d-parity-roadmap.md`.

### R3-8: Isosurface has no visibility flag — silx `Item3D.setVisible` semantics ported for CutPlane only

**FIXED (R3):** `Isosurface` gained a `visible` flag (default `true`, silx
`Item3D` default) with `is_visible`/`set_visible`, mirroring the sibling owned
child `CutPlane`. Both emit helpers now skip a hidden surface —
`append_solid_isosurfaces` (`if !iso.visible`) and `append_colormapped_
isosurface` (`if !iso.is_visible()`). `ComplexIsosurface` exposes the flag via
its embedded `Isosurface`, so `ComplexField3D`'s colormapped surfaces are
covered by the same single flag. Picking is geometry-based (ray vs the emitted
mesh), so suppressing emission also removes the surface from picks — no separate
pick gate needed. Bounds are the volume box regardless, so hiding a surface
leaves them unchanged (matching silx group-bounds skipping invisible children in
effect).

Tests: `hidden_isosurface_emits_no_geometry_but_keeps_level_and_colour` and
`hidden_colormapped_isosurface_emits_no_geometry`.

Severity: Low

Rust: `src/render/scene3d_items.rs:2165-2222` — `Isosurface` carries `level`/`color`/`auto_level` only; `append_solid_isosurfaces` (`:2915`) and `append_colormapped_isosurface` (`:2949`) emit every surface unconditionally. `CutPlane` (`:2260-2273`) does have `visible`/`set_visible`, honored by render (`:3006`) and pick (`:2838`). `ComplexField3D`'s colormapped isosurfaces (`:3231`) also have none.

Reference: `silx/gui/plot3d/items/volume.py:213` — `class Isosurface(Item3D)`; `items/core.py:183-231` — `setVisible` on every item, `_pick` gated on `isVisible()`; `scene/core.py:299-305` — group bounds skip invisible children.

Impact: within the owned-children model (isosurfaces live inside `ScalarField3D`/`ComplexField3D`; the caller cannot omit them like free items), temporarily hiding one iso-level while keeping its level/colour has no equivalent — only remove-and-re-add. Internally inconsistent: the sibling owned child (CutPlane) got the visibility contract, the isosurfaces did not.

### R3-9: R2-51 residual — transparent-texel stand-in for GLSL `discard` still writes depth, occluding later-drawn geometry behind below-min holes — (R2-51 residual)

**FIXED (R3):** `scene3d_image.wgsl` `fs_main` now discards `color.a == 0.0`
texels, the exact port of silx `_Image` (`plot3d/scene/primitives.py:2115-2123`,
`if (color.a == 0.) discard;`) — the discard is in the shared `_Image` base, so
it covers both `ImageData` (a below-min colormap hole → alpha 0) and
`ImageRgba`, which is why the fix belongs in the one shared siplot image shader
rather than at the cut-plane call site. A discarded fragment writes neither
colour nor depth, so a transparent below-min hole no longer stamps the depth
buffer and occludes 3D geometry behind it. The sample is taken before the branch
to keep its implicit derivatives in uniform control flow.

Test: `transparent_front_texel_does_not_occlude_geometry_behind` in
`tests/scene3d_image_render.rs` — a front quad with one transparent texel drawn
before an opaque green quad 1 unit behind it; the hole must reveal green.
Verified it fails without the discard (hole reads `(0,0,0)`, the black clear)
and passes with it.

Severity: Low

Rust: `src/render/scene3d_items.rs:2478-2566` (`build_cut_plane_mesh`, fix 37d9edd) — a below-min texel with `display_values_below_min == false` is emitted `Color32::TRANSPARENT`, but the quad still rasterizes: the image pipeline has `depth_write_enabled: Some(true)` (`src/render/gpu_scene3d.rs:1017-1030`), draw order triangles → lines → meshes → images → points (`:1397-1433`).

Reference: `silx/gui/plot3d/_glutils/function.py` (colormap GLSL, `:462-520`) — `displayValuesBelowMin=False` compiles `if (value == 0.) { discard; }`: the fragment writes neither color nor depth, so the hole never occludes regardless of draw order.

Impact: geometry drawn after the cut plane (the whole points channel, subsequent image/textured mesh) is depth-rejected behind a below-min hole: silx shows the scatter through the punched-out slice, siplot shows only what was drawn before it. Narrow config (flag off + later-channel geometry behind the plane), hence Low. The blending half of the fix is faithful; this is the depth half of `discard`.

### R3-10: R2-47 residual — all four scene pipelines use `CompareFunction::Less` where silx sets `glDepthFunc(GL_LEQUAL)` — (R2-47 residual)

**FIXED (R3):** all four depth-tested scene3d pipelines (line, point, mesh,
image) now use `CompareFunction::LessEqual`, matching silx's global
`glDepthFunc(GL_LEQUAL)` set once for the whole 3D viewport
(`plot3d/scene/viewport.py:360`). `rg 'CompareFunction::Less\b'` confirmed
exactly these four `depth_compare` sites workspace-wide and no fifth scene3d
depth pipeline; the blit pass is depth-off and unaffected. LessEqual lets a
fragment at exactly the stored depth pass (coplanar redraws), which `Less`
rejected.

Severity: Low

Rust: `src/render/gpu_scene3d.rs:814, 888, 961, 1030` — `depth_compare: Some(wgpu::CompareFunction::Less)` on the line/triangle, point, mesh, image pipelines. The R2-47 fix (ad2b7b0) aligned the blend state but not the depth function two lines below the cited block.

Reference: `silx/gui/plot3d/scene/viewport.py:356-360` — the same block that enables `GL_BLEND` sets `glDepthFunc(GL_LEQUAL)` for the whole scene.

Impact: coincident-fragment policy inverted — silx lets the later-drawn primitive win at equal depth (its stroke-over-fill convention relies on it), siplot keeps the first-drawn. Observable on exactly coplanar geometry (cut-plane border vs fill, duplicate iso-levels). Low (no wrong image for non-coincident geometry), but a wire-state divergence inside the very GL block R2-47 cited.

### R3-11: ScenePositionInfo drops the Item field and its `%g` claim is wrong

Severity: Low

Rust: `src/widget/scene_position_info.rs:3-9` — module doc lists four fields (X/Y/Z/Data, silx `_xLabel`/`_yLabel`/`_zLabel`/`_dataLabel`); `:51-69` draws exactly those four; `:77-81` `fn g` claims to format "the way silx's `%g` does" via Rust default float `Display`.

Reference: `silx/gui/plot3d/tools/PositionInfoWidget.py:56-60` — the widget has five fields incl. `_itemLabel = self._addInfoField("Item")`; `:201-202` `self._itemLabel.setText(item.getLabel())`; `:205-215` scalars format `"%g"` (6 sig digits), arrays `"%.3g"`.

Impact: the readout never says what was picked — silx distinguishes an isosurface from a cut-plane hit by label; siplot's `FieldPick` carries no source tag so the caller cannot add it. The doc misrepresents the silx surface (four fields as if complete). Secondary: `Display` is not `%g` (`0.123456789` → `0.123457` in silx vs full precision), same family as recorded R1-19 but this site claims equivalence instead of recording it.

### R3-12: `loc://` fabricates a configured variable from any first listener; PyDM requires `name`+`type`+`init` and defers to the first config-bearing listener

Severity: Medium

Rust: `sidm/src/data_plugins/local_plugin.rs:52-67` — `LocalPlugin::connect` unconditionally publishes a connected state from the first address's params; `initial_local_state` defaults `ty="float"` (`:97`), `parse_init` falls back to a type-zero for absent/unparsable `init` (`:156-165`). `engine.rs:196-199` reuses the pooled connection for every later `loc://name?...`, `address.rs:103-109` pools with the query dropped, so a later config-bearing address is discarded. Module doc (`:7-9`) claims "parameters apply only on the first connection, matching PyDM".

Reference: `pydm/data_plugins/local_plugin.py:26` — `_required_config_keys = ["name", "type", "init"]`; `UrlToPython.get_info` (`:421-438`) returns `(None, name, address)` for a bare/partial address; `_configure_local_plugin` (`:47-82`) returns early with `_is_connection_configured` False, leaving `send_connection_state(False)` (no value); it re-runs on every `add_listener` (`:333-335`). Unparsable init → `value=None` (`:318-327`), never a fabricated zero.

Impact: the standard idiom (one widget declares `loc://x?type=int&init=5`, others reference bare `loc://x`) becomes creation-order-dependent: a bare reader connecting first shows a live connected `0.0` (PyDM: disconnected) and permanently drops the declared type/init when the declaring widget connects later.

### R3-13: `calc://` array-valued children skip evaluation silently and permanently — the R2-59 fail-visible contract does not cover the bind path; PyDM evaluates ndarray children — (R2-59 residual)

Severity: Medium

Rust: `sidm/src/data_plugins/calc_plugin.rs:367` — `let var = value.as_ref().and_then(pv_to_evalexpr)?;` conflates "no value yet" (legitimate skip) with "value present but unbindable" — `pv_to_evalexpr` returns `None` for `FloatArray`/`IntArray`/`StrArray` (`:710`). R2-59's warn-once plumbing (`5207dc5`) fires only on `set_value` errors (`:369-373`) and eval errors (`:384-387`); the `?` at `:367` bypasses both, so a connected waveform child makes the calc channel a permanently dead silent channel (the 50 ms poll re-skips forever).

Reference: `pydm/data_plugins/calc_plugin.py:121-142` — `callback_value` stores any value incl. `np.ndarray`; `calculate_expression` (`:159-178`) skips only while a value is `None` and binds ndarrays into an env whose vocabulary includes `np`/`numpy` (`:51-53`), so `A[0]`, `np.mean(A)`, bare `A` evaluate.

Impact: any `calc://` with a waveform child works in PyDM and is a silent dead channel in sidm — the exact mode R2-59 was raised to make visible. Minimal closure: warn-once at the `:367` distinction; full parity needs array binding.

### R3-14: SidmSpinbox suffix drops the `{units}` half of PyDM's composition and the `showStepExponent=False` tooltip fallback — (R2-54 residual)

Severity: Low

Rust: `sidm/src/widgets/spinbox.rs:187-191` — suffix is only `" Step: 1E{n}"`; no `show_units`/unit path exists; `show_step_exponent=false` produces neither suffix nor tooltip. The R2-54 fix (`278e689`) cites spinbox.py:143-145 but ports only the step half of line `:144`.

Reference: `pydm/widgets/spinbox.py:129-148` — `update_format_string` composes `units = " {}".format(self._unit)` when `_show_units` (`:138-141`), then `setSuffix("{0} Step: 1E{1}".format(units, self.step_exponent))` (`:144`); with `showStepExponent` off, the suffix is units alone and the step moves to the line-edit tooltip `"Step: 1E{0:+d}"` (`:146-148`).

Impact: a spinbox with `showUnits` loses its engineering-units suffix; turning the step display off loses the step entirely instead of relegating it to a tooltip. Defaults match (`_show_units` False), so only opted-in screens are affected.

### R3-15: `loc://` `type=array` ignores the numpy `dtype` kwarg cited in R1-32's reference — (R1-32 residual)

Severity: Low

Rust: `sidm/src/data_plugins/local_plugin.rs:104-116` — `initial_local_state` matches `type/init/precision|prec/unit/upper_limit/lower_limit/enum_string` and drops every other key (`_ => {}`); element types are inferred from the literal (`:196-209`). The R1-32 fix (`e4ed898`) never mentions the dtype kwarg the finding's own Reference named (doc `:348`).

Reference: `pydm/data_plugins/local_plugin.py:30` — `_extra_numpy_config_keys = ["dtype", "copy", "order", "subok", "ndmin"]`; `format_type_params` (`:257-288`) resolves `dtype` to `np.dtype`, `convert_value` (`:301-327`) passes it to `np.array(...)`, so `loc://x?type=array&init=[1,2]&dtype=float` yields float64.

Impact: an integer literal with `dtype=float` stays `IntArray` in sidm (float array in PyDM) — visible in text rendering (`[1, 2]` vs `[1.0, 2.0]`) and any Int/Float-distinguishing consumer. Narrow (only `dtype` has real effect in PyDM too).

### R3-16: Composite-file macros are merged with the parent's; MEDM replaces the table

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:2167-2171` — `merged_macros` builds the inlined subtree's table as `parse_embedded_macros(embedded)` then `extend_from_slice(parent)`; comment `:2109-2111` claims "embedded values winning over inherited" is MEDM behaviour; applied at `:2104-2112`.

C reference: `medm/medmComposite.c:659-668` — `compositeFileParse`: "Only do this if there is a macro string, otherwise use the existing macros"; when `file;a=x,b=y` carries macros, `displayInfo->nameValueTable` is replaced by `generateNameValueTable(macroString)` — parent macros are not consulted while parsing the included file.

Impact: for `"composite file"="child.adl;M=2"` inside a parent opened with `P=ioc1:`, a `$(P)` in child.adl stays literal in MEDM (dead PV / literal text) but is expanded to `ioc1:` by adl2sidm — the inlined subtree connects channels and renders labels MEDM never would. The empty-macro-string inherit case matches.

### R3-17: Composite-file include skips MEDM's bbox refit + child translation

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:2116-2128` — `emit_embedded_display` inlines the target with `child_origin (0,0)`, so each child renders at `frame_origin + (child.x, child.y)`, and the frame keeps the stale `.adl` geometry (`geom` at `:2121`).

C reference: `medm/medmComposite.c:709-736` — after parsing the included file MEDM computes the children's bbox, resizes the composite to `maxX-minX` × `maxY-minY` (`:730-733`), and translates every child by `(oldX-minX, oldY-minY)` via `compositeMove` (`:736`) so the content bbox corner lands exactly at the composite's written x,y.

Impact: included content whose bbox min is not (0,0) — common, since child displays carry layout margins — renders shifted by `(minX, minY)` relative to MEDM, spilling outside the intended spot; the frame rect is the file's stale w/h instead of refit content size.

### R3-18: Strip-chart per-pen `limits` block silently dropped; pens not ranged

Severity: Medium

Rust: `adl2sidm/src/adl_parser.rs:538-571` — `indexed_records("pen[", …)` collects pen fields via `locate_assignments` (`:197-215`), which takes level-0 `key=value` lines only, so a pen's nested `limits { … }` block vanishes at the IR level with no warning; `codegen.rs:1373-1381` — `emit_strip_chart` uses only `chan` + `color` per pen.

C reference: `medm/medmMonitor.c:294-297` — `parsePen` parses a nested `limits` block per pen; `medm/medmStripChart.c:467-509` — at runtime each pen scales to its own `[lopr, hopr]` resolved per-bound through `updatePvLimits` (channel HOPR/LOPR unless the pen's limits set Default/User sources).

Impact: authored per-pen ranges are lost silently (bypasses the warn convention); mixed-range pens (0–300 K temperature with 0–1e-6 Torr pressure) render on one shared auto-scaled axis instead of MEDM's per-pen normalized traces — the chart is unreadable for exactly the screens that set pen limits.

### R3-19: Absent cartesian `style` key means POINT_PLOT — rendered as lines, silently — (R2-68 residual)

Severity: Medium

Rust: `adl2sidm/src/codegen.rs:1587-1600` — `warn_unsupported_cartesian_keys` warns about non-line styles only `if let Some(style) = a.get("style")`; `:1601-1613` same present-key gating for `erase_oldest`. An absent key produces the connected-line `SidmWaveformPlot` (`:1518-1522`) with zero warning.

C reference: `medm/medmCartesianPlot.c:2904` — `createDlCartesianPlot` defaults `style = POINT_PLOT` (and `erase_oldest = ERASE_OLDEST_OFF`, `:2905`); `:3106-3108` — `writeDlCartesianPlot` omits the `style` key whenever it equals POINT_PLOT, so every point-plot `.adl` on disk carries no `style` key.

Impact: the most common cartesian style (MEDM default; guaranteed key-absent on disk) silently renders as a connected line plot with no warning — the R2-68 fix (16c1fd1) satisfied the warn convention for written keys, missing the write-omits-defaults rule that the absent key is the loaded one. Same family: absent `erase_oldest` = "plot n pts & stop", also divergent and silent.

### R3-20: Display colormap fallback chain (external `cmap` file, `defaultDlColormap`) unported — colors silently lost

Severity: Medium

Rust: `adl2sidm/src/adl_parser.rs` — the display block's `cmap` key is never read (mentioned only in the doc comment at `:100`); with no inline `"color map"` block the table is empty and `take_colors` (`:254-270`) returns `None` for every `clr`/`bclr`, so all widgets fall to sidm theme defaults with no warning.

C reference: `medm/medmDisplay.c:388-427` — `executeDlDisplay`: a non-blank `dlDisplay->cmap` triggers `parseAndExtractExternalColormap` (medmCommon.c:1315) on the named file; on failure MEDM prints "Cannot parse … Using the default colormap" and every display with no colormap gets the compile-time `defaultDlColormap` via `createDlColormap` (medmCommon.c:277-284).

Impact: legal MEDM screens referencing a shared `cmap` file (older/site-standard practice) convert with every fg/bg color silently discarded — no external-file parse, no default 65-color palette, no warning naming the unresolved `cmap`.

### R3-21: Per-trace `yaxis`/`yside` ignored — Y2 traces plot against Y1 with no warning

Severity: Low

Rust: `adl2sidm/src/codegen.rs:1455-1511` — the trace loop reads only `color`/`xdata`/`ydata`; `yaxis`/`yside` are never consulted and produce no warning.

C reference: `medm/medmMonitor.c:346-358` — `parseTrace` reads per-trace `yaxis`/`yside`; `:516-518` — `writeDlTrace` writes both unconditionally, and MEDM binds each trace to its Y1/Y2 axis (and side) at execute time.

Impact: a two-axis cartesian plot (current on Y1, pressure on Y2) plots every trace against Y1; the trace-to-axis assignment present in every written trace block is silently dropped, bypassing the warn convention.

### R3-22: `wheel_decimals` unparseable-format fallback diverges from Xc `compute_format` — (R2-69 residual)

Severity: Low

Rust: `adl2sidm/src/codegen.rs:3078-3099` — a format without `%…f` returns `None` (caller `:755-761` warns, precision left to channel); `"%f"` yields `Some(0)`; `"%.3f"` yields `Some(3)`.

C reference: `medm/xc/WheelSwitch.c:1352-1354` — no `%` or no `f` after it → `DEFAULT_FORMAT` `"% 6.2f"` (precision 2, `:44`); `:1380-1396` — the spec must sscanf as `"%d.%d"`: `"%f"` and `"%.3f"` both give `nparsed==0` → width 6, precision 2; width-only (`"%6f"`) → precision 0 (matches Rust).

Impact: for a malformed/degenerate `format` (`"%g"`, `"%f"`, `"%.3f"`), MEDM's wheel switch displays exactly 2 decimals; the converted `SidmSpinBox` shows channel-PREC decimals (or 0, or 3). The R2-69 fix (ca5803c) ported the happy-path `n.m` parse but not Xc's default fallback.

### R3-23: Related-display single-button-vs-menu gate counts names; MEDM counts labels — (R2-64 residual)

Severity: Low

Rust: `adl2sidm/src/codegen.rs:2235` (single plain button iff exactly one non-empty-name entry) and `:2220` (row/col iff `entries.len() >= 2`), with `entries` built from non-empty names (`related_display_entries`, `:2488`).

C reference: `medm/medmRelatedDisplay.c:236-243` — `iNumberOfDisplays` counts entries with non-empty labels, and "Case 1 of 4" (one plain button) fires when that count `<= 1`; the button's activate opens the first non-empty-name entry (`:302-309`); the row/col case builds buttons from slots `0..iNumberOfDisplays-1` (`:537-541`).

Impact: a related display with several named targets but zero/one per-entry labels renders in MEDM as a single plain button opening only the first target (rest unreachable), while adl2sidm renders a dropdown/row exposing all. The R2-64 fix approximated the case-1 gate with target count instead of MEDM's label count.

### R3-24: Runtime `MacroTable::expand` leaves undefined macros literal in related-display args; MEDM drops them

Severity: Low

Rust: `adl2sidm/src/codegen.rs:3340-3356` — the generated `MacroTable::expand` substitutes known names and leaves unknown `$(name)` in place (its doc cites `performMacroSubstitutions` but describes `getToken` passthrough); used at `:2450-2452` to expand a related-display entry's `args` at click time, feeding both the dedup key and the child's macro table.

C reference: `medm/utils.c:3444-3459` — `performMacroSubstitutions` emits nothing for an undefined `$(name)` (the reference is deleted); applied to the args string at click time in `relatedDisplayCreateNewDisplay` (medmRelatedDisplay.c:979-981). Literal passthrough is `getToken`'s contract (medmCommon.c:1455-1462), which governs file parsing, not this path.

Impact: for `args="P=$(X)"` with `X` undefined in the parent, MEDM's child receives `P=""` (child `$(P)val` → `val`); the generated code's child receives `P="$(X)"` (child channel becomes `$(X)val`) — different child channel names and dedup key. Convert-time `substitute_macros` passthrough (the parse path) is correct and unaffected.

## Cleared During Review

Fix round 2026-07-03/04 (one commit per finding; branch merged fast-forward
into main):

**Category A batch (`fix/interaction`, cherry-picked onto main at
`fe4ec3f`):**

- R1-1 — `134d1a5` pan and wheel zoom move y2 with the gesture; single
  owner `commit(plot, next, next_y2)` writes limits AND y2 for the
  pan/wheel/box/arrow paths.
- R1-2 — `40097d1` wheel gated on zoom-enabled axes / keep-aspect
  override / all-disabled no-op (`_onWheel`); box zoom skips the
  disabled-axes substitution under keep-aspect (`_getAxesExtent`).
- R1-3 — `bdb33f6` silx `checkAxisLimits` is the one repair owner
  (degenerate ±10% expansion, ±1e37 clamp) for BOTH reset verbs; the
  divergent `as_non_degenerate` padding and the dead write-only
  `home_limits` removed. Includes silx's "Nothing to autoscale"
  all-axes-pinned early return.
- R1-4 — `ff4c403` log toggles snap to the positive data range at toggle
  time (`_internalSetScale` + `_logFilterData`-style filtering);
  keep-aspect toggle change-gates and forces a reset zoom. *Residual:*
  no `PlotEvent` counterpart for silx's scale/aspect notify (no event
  pattern to attach to; `LimitsChanged` fires from the refits).
- R1-5 — `567a90d` `_forceResetZoom` cross-axis defaults: no-data axis →
  (1,100), left adopts yright when left has no data, y2 adopts left;
  y2-only and itemless plots now refit.
- R1-6 — `b06efc4` box zoom rejected below silx's SURFACE_THRESHOLD
  (pixel area |dx|·|dy| ≥ 5) — no limits write, no history push.
- R1-7 — `8966b21` zoom-mode entry clears limits history; context-menu
  Reset Zoom no longer clears; per-frame wheel pushes removed (pan and
  box-zoom pushes kept per the recorded roadmap decision — roadmap rows
  updated).
- R1-8 — `fe4ec3f` wheel factor exp(ln(1.1)/40·px): one egui discrete
  notch (Line×40pt, sum-conserving smoothing) = exactly ×1.1, N notches
  = 1.1^N; trackpad stays continuous at the same rate.

**Category C batch (`fix/plot3d`, cherry-picked onto main at `1119c54`):**

- R1-24 — `9b7ce07` silx default style constants (grey-51 background,
  white box/text) + foreground/text colour APIs.
- R1-17 — `8c9ec16` orbit pivots on the picked point, pan plane at the
  picked NDC depth, wheel zoom anchored at the cursor pick.
- R1-23 — `553ed28` viewport linear fog + shininess-gated specular
  (ScalarFieldView shininess=32); `paint_scene3d_with`/`snapshot_scene3d_with`.
- R1-21 — `3c0bbb5` cut plane's box-intersection contour stroke + colour/
  visibility API. *Residual:* 1 px stroke vs silx width 2.0 (line
  pipeline has no wide-line support).
- R1-22 — `94cd71f` Scatter2D LINES data-point picking (5 px threshold)
  + image-quad pixel picking (row/col payload).
- R1-18 — `fbfe4e2` `Item3DTransform` stack (translate · rotate-about-
  centre · matrix · scale, silx items/core.py:288-485) baked at append;
  inverse-transpose normals; transformed bounds/pick; SFV set_scale/
  set_translation. *Residual:* rotated image layers convert to textured
  quads and lose row/col picking.
- R1-19 — `0f099b1` LabelledAxes chrome: ticklayout port (verified
  against executable silx), dashed tick lines, egui-overlay axis/tick
  labels, `set_axes_labels`. *Residuals:* labels absent from
  `snapshot()` (overlay text); CPU world-space dashes vs silx screen-px
  fragment dashes; `%g` stand-in decimal-only at extreme magnitudes.
- R1-20 — `1119c54` overview orientation indicator: companion scene
  (half-transparent disc + RGB axes ×2.5), camera slaved at
  −12·direction, second viewport+scissor pass top-right 100×100 px, on
  by default, `set_orientation_indicator_visible`.

**Category B batch (`fix/items-fit`, cherry-picked onto main at `4649200`,
+ follow-up `4ee4ad6`):**

- R1-10 — `4f2be24` `std` statistic (Welford, ddof=0 = numpy.std);
  `STAT_COLUMNS` now exactly silx `DEFAULT_STATS` (sum/delta columns
  removed from the default table; fields kept on `Stats` as API).
- R1-11 — `7a2776c` histogram stats over the N raw counts at
  `_revertComputeEdges` anchors (not the 2N step polyline — sum was
  exactly doubled); `_ScatterContext` port (value-array stats, x-AND-y
  on-limits mask) wired through `StatsInput::{Histogram,Scatter}`.
  Side effects (all silx-faithful): histogram snapping/fit target bin
  centres+counts; value scatters are no longer fit targets or
  CurvesROIWidget feeds; CSV save of a histogram exports
  (centres, counts).
- R1-12 — `5d3a1ef` FitWidget Multi-Gaussian uses FitManager's
  Sensitivity 2.5 (`DEFAULT_FIT_SENSITIVITY`); the standalone pyx
  `peak_search` keeps 3.5 (distinct surface).
- R1-13 — `91aa1a2` `padded_peak_search` ports FitTheories.peak_search
  (fwhm-copy padding, remap, in-range filter; Yscaling=1.0 default
  config documented).
- R1-14 — `560bc9d` step-up/Atan seed: derivative rescaled to max(y),
  fitted deriv-peak height taken when it exceeds max−min; stepdown
  keeps the amplitude.
- R1-15 — `dce1e08` slit beamfwhm REVERTED to silx's exact
  `0.5·(largestup[2] + largestdown[1])` (upstream index quirk
  reproduced deliberately — parity over local correction).
- R1-16 — `1e8af27` default image colormap gray linear (silx
  DEFAULT_COLORMAP_NAME); LUT verified as the `[i,i,i,255]` ramp.
- R1-9 — `4649200` (STRUCTURAL) `AutoscaleMode::range` requires a
  `Normalization` — blind autoscale is unrepresentable;
  `Colormap::autoscale_range` mirrors `_computeAutoscaleRange`:
  per-normalizer minmax (log = min_positive), normalized-space stddev3
  for log/sqrt/arcsinh, is_valid percentile filters, per-normalization
  DEFAULT_RANGE ((1,10) log). Closes the log-image render collapse.
- follow-up — `4ee4ad6` re-export `revert_compute_edges` at the crate
  root beside `histogram_edges`.

**Category E batch (`fix/adl2sidm`, cherry-picked onto main at `57ceb01`,
+ follow-up `dec7568`):**

- R1-33 — `cc278de` `calc://` gains a `dialect=medm` mode evaluated by
  `epics_base_rs::calc` (the libCom postfix/calcPerform port — grammar
  superset of medmCalc.c, double-typed throughout, so `A=0` on a Float
  channel is finally true at 0.0); children bind scalar-or-0.0; E–L
  operand metadata from the first channel; invalid expressions fail
  VISIBLE (publish 1.0 + warn once — deliberate deviation from MEDM's
  hide, fail-safe for operator screens). *Residual:* the `I` (alarm
  status-code) operand binds 0.0 — ChannelState carries no status code.
- R1-34 — `08bfbd6` visibility gates carry the ORIGINAL MEDM CALC
  verbatim under `dialect=medm` (only %/& percent-encoded); the lossy
  `translate_calc_to_evalexpr` table and the `&` bail-out are deleted —
  closes the whole translation-gap family (functions, ternary, `**`,
  bitwise keywords, lowercase and E–L operands).
- R1-35 — `5de60ea` old-format `ctrl`/`rdbk` channel keys accepted
  (medmControl.c:36-37, medmMonitor.c:77-78).
- R1-36 — `0d7f3ab` plotcom title/labels/colours + cartesian
  user-specified axis ranges reach the three sidm plots via new
  builders; `set_x_range` added beside `set_y_range` (one owner shared
  with plot_menu); non-portable rangeStyles warn.
- R1-37 — `68d0657` valuator up/down → vertical slider
  (`SidmSlider::with_orientation`); bar down/left →
  `with_inverted_appearance`; indicator keeps MEDM's own down→up/
  left→right override. *Residual:* valuator down/left max-end reversal
  warn-only (no slider surface in sidm or PyDM).
- R1-38 — `3156be9` absent sbit/ebit default 15/0 (medmByte.c:279-280)
  → stock bytes render 16 bits MSB-first; ALSO fixed the inverted
  endianness mapping vs xc/Byte.c:513-519 (`sbit > ebit` → MSB-first) —
  adl2pydm has both bugs; MEDM C is the contract.
- R1-39 — `bca110b` value-label suppression (label ∉ {limits,channel})
  now uniform across bar/indicator/meter; `fillmod="from center"` →
  `SidmScaleIndicator::with_origin_at_center` (geometric-midpoint
  anchor per BarGraph.c, deliberately NOT PyDM's value-zero
  originAtZero).
- R1-40 — `57ceb01` row-stacked choice buttons size fonts from
  per-button height `h / max(2, round(h/20))`.
- follow-up — `dec7568` cfg-gate the R1-30 read-only env helpers to the
  ca/pva features (dead code under adl2sidm's default-features=false
  sidm build).

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
  recurses; NTNDArray ubyte lands as `Bytes`. *Residual closed* `3f7dbfc`:
  `pva_codec.rs` decompresses lz4 (raw block via lz4_flex), bslz4
  (bitshuffle stream, transpose + block walk ported from bitshuffle C)
  and blosc (c-blosc 1.x frame incl. a BloscLZ decoder port; LZ4/ZLIB
  sub-codecs). `codec.parameters` = pvData ScalarType ordinal; ordinal 9
  decodes f32 (deliberate deviation from PyDM's f64-typed index 9).
  *Remaining residual closed* `654db4a` (user approved rust-zstd + the
  remaining crates): blosc zstd sub-codec via rust-zstd, snappy via
  snap, jpeg via the image crate's zune-jpeg decoder (`image/jpeg`
  feature on the dependency sidm already carries — no new crate). The
  one-time-warn + metadata-only path now fires only for unknown codec
  names and malformed streams (deviation from PyDM, which emits the
  raw compressed bytes as the value on any codec error).
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

**Round 2 fix batch (structural clusters first; one commit per finding;
on `main`):**

- R2-1 — `9921117` ImageStack autoscales each frame to its own data
  (minmax) with the default gray colormap, via a split-out
  `frame_colormap()` helper (base LUT/normalization preserved, range
  re-derived); no more frozen `viridis(0.0, 1.0)`. Completes the R1-16
  default-colormap family at the ImageStack site. Regression tests:
  range tracks frame data (not the stored span), uniform frame → (v, v).
- R2-23 — `3bde59a` ComplexImageView now holds one persistent gray base
  colormap shared across the non-phase scalar modes (silx binds a single
  ColormapMixIn colormap), with a `colormap()`/`set_colormap()` surface;
  each image's range is derived through the new normalization-aware
  `Colormap::autoscaled()` core primitive instead of a hardcoded viridis
  + the (removed) non-normalization-aware `finite_range`. Phase keeps its
  fixed hsv colormap. Regression tests: `Colormap::autoscaled` preserves
  LUT/nan/normalization while replacing range, and honors log
  normalization for the derived bound.
- R2-46 (palette half) — `61b2872` the six plot3d item constructors
  (Scatter3D/Scatter2D/ColormapMesh3D/ImageData3D/HeightMapData/CutPlane)
  and CompareImages now default to gray, not viridis — the R1-16 family
  sweep across the last seven `Colormap::viridis(0.0, 1.0)`-at-
  construction sites. **Autoscale-follows-data half NOW closed** (`649135c`)
  via the per-bound `vmin_auto`/`vmax_auto` model — see the RESOLVED
  autoscale-representability note below; pinned ranges from
  `with_colormap(Colormap::new(name, -0.5, 1.0))` are preserved.

**Round 2 profile-subsystem cluster (on `main`):**

- R2-4 — `d00ae1c` ProfileWindow retains the (image, ROI) it extracted
  from (`ProfileSource`) and owns one `recompute()`; `set_line_width`/
  `set_method` recompute from it, `refresh_image` re-derives on a new
  image (wired into `ImageView::set_image` and `StackView`'s per-frame
  dirty upload), and the precomputed-curve path clears the source. Width/
  Method edits and image/frame changes now update the profile without a
  fresh drag (silx `invalidateProfile` / recompute-on-DATA). Tests:
  width/method recompute; frame scrub recompute; precomputed-curve clear.
- R2-3 — `2cef1d4` axis-aligned (`aligned_profile_values`) and rect
  (`rect_profile_values`) band reductions are NaN-aware (numpy
  nanmean/nansum): NaN pixels are skipped, Mean divides by the finite
  count (NaN for an all-NaN band), Sum sums the finite pixels (0.0 for an
  all-NaN band). Was plain sum ÷ full band, so one masked NaN poisoned the
  whole sample. Tests: aligned + rect skip a NaN; all-NaN → sum 0 / mean
  NaN.
- R2-5 — `09b8f40` StackView's 2D stack profile reads the shared line
  width / method from the profile window (silx Profile3DToolBar's single
  setting) instead of hardcoded 1 / Mean, and its line profile is the
  bilinear band (`line_profile_band`) matching the 1D line. StackProfile
  is retained (`StackProfileWindow::profile()`) for observability. Tests:
  stack line profile varies with width/method; 2D profile equals a
  width-3 Sum extraction, not width-1 Mean.

**RESOLVED — autoscale-representability cluster (model decision made and
implemented; this whole cluster is now closed):**

- The shared root of R2-14 (per-bound autoscale), R2-46's second half,
  and the pin-gap left open by the R2-1/R2-23 "always autoscale on
  rebuild" fixes was that `Colormap` carried plain `f64` vmin/vmax with
  no way to express silx's `None`-means-autoscale-this-bound contract.
  This was closed by adding **per-bound autoscale to `Colormap`**:
  `vmin_auto` / `vmax_auto` flags alongside the concrete `vmin` / `vmax`
  (silx `vmin/vmax is None`), with `Colormap::autoscale(name)` (both
  bounds auto) and `Colormap::resolved(mode, data)` refreshing *only* the
  auto bounds while preserving pinned ones. This is the per-bound model
  the cluster was waiting on — it closes R2-14 per-bound, lets R2-1/R2-23
  autoscale the default while still honouring a user-pinned range, and
  lets the six 3D items + CompareImages autoscale on `set_data` without
  clobbering explicit `with_colormap`/`set_images` ranges. Landed across
  `649135c` (R2-46 3D items → gray autoscale), `517e479` (R2-1 CompareImages
  seals both images' combined range), `4b5fbfb` (R2-23 pinned range honoured),
  `565b9b0` (R2-14 per-bound Auto toggles in the colormap dialog). The
  individual R2-1/R2-14/R2-23/R2-46 finding blocks above all now read FIXED.

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

- 2026-07-03/04: **fix round complete — all 40 findings cleared** (see
  the batches above; every fix one commit, per-crate gates green at
  every commit, full-workspace gate green at the end). Two extra
  defects found and fixed during the round beyond the inventory:
  the R1-38 byte endianness mapping was inverted vs xc/Byte.c (also a
  live adl2pydm bug), and the R1-30 read-only helpers were dead code
  under a no-default-features sidm build (`dec7568`). Recorded
  residuals (deliberate/blocked, not silent): compressed NTNDArray
  codecs (closed post-round by `3f7dbfc` + `654db4a` — all four
  NDPluginCodec codecs decode incl. jpeg and the blosc snappy/zstd
  sub-codecs; only unknown-codec/malformed streams warn-and-skip), pva
  subfield writes (NTTable deferral),
  calc `I` status-code operand (ChannelState gap), cut-plane stroke
  1 px (no wide-line pipeline), LabelledAxes labels absent from
  `snapshot()` (egui overlay text), valuator down/left max-end
  reversal warn-only, revoked-write-rights path unit-tested only.

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

- 2026-07-04: **round 2 opened**; same 5 read-only agents, scopes
  rotated to surfaces R1 left uncovered (A: silx tools/widget layer,
  B: items/colors/ticks/fit-engine internals, C: plot3d round 2,
  D: sidm widget/display semantics vs PyDM, E: adl2sidm remaining
  widget/attribute surface vs MEDM C).
- 2026-07-04: round 2 consolidated — **69 findings** (High 3,
  Medium 44, Low 22), renumbered R2-1..R2-69 (A: 1–26, B: 27–45,
  C: 46–52, D: 53–60, E: 61–69). Baseline: post-R1-fix-round HEAD
  `4ba56d2`.

  Thematic clusters:
  - **The R1-16 fix never swept its family:** R2-1 (ImageStack
    `viridis(0,1)`), R2-23 (ComplexImageView), R2-46 (six 3D item
    sites), plus the CompareImages sibling at `high_level.rs:8577`
    flagged by agent A. The R1-16 fix changed one construction site;
    the `Colormap::viridis(0.0, 1.0)`-at-construction anchor was never
    `rg`-swept. Textbook violation of the fixes-from-reported-defects
    rule — the fix round must start with that sweep.
  - **Autoscale is unrepresentable in the `Colormap` model:** R2-1,
    R2-14, R2-23, R2-46 share one structural cause — `Colormap`
    carries plain `f64` vmin/vmax where silx's contract is
    `None`-means-autoscale per bound. Every consumer therefore invents
    a frozen range. Structural fix: optional bounds resolved against
    item data at apply time; the four symptoms then close together.
  - **Defaults are the R2 negative space.** Beyond the colormap
    family: R2-18 (grid Major-on vs silx none), R2-52 (Side vs front
    viewpoint), R2-55 (alarm-border on PushButton/Spinbox/Slider where
    PyDM ships border-off), R2-57 (CLike/Viridis vs PyDM
    Fortranlike/Inferno), R2-58 (scatter/event buffer 18000 vs 1200),
    R2-62/R2-66 (MEDM write-omitted defaults: period 60 s, HOPR 1.0).
    R1 audited behaviours; R2 shows constructor defaults and
    absent-key defaults were the unaudited half.
  - **MEDM write-omits-defaults, parser treats absent as absent:**
    R2-61 (missing `vis` fabricated as "if not zero" where MEDM's
    default is V_STATIC — hides alarm-coloured widgets at value 0),
    R2-62, R2-66 — the same class R1-38 opened (`sbit=15` omitted).
  - **adl2sidm silent drops bypass its own warn convention:** R2-63
    (old-format attribute inheritance), R2-64 (`visual`/`policy`),
    R2-65 (format types with existing sidm surfaces), R2-67
    (controller `clrmod=alarm`), R2-68 (cartesian runtime keys) — the
    emitter warns on other unsupported paths but not these.
  - **Fit stack: mode/config parity, not formula parity:** R2-27..33 —
    R1 verified the formulas; what diverges is which mode the deployed
    FitManager path actually runs (central differences, per-λ budget,
    strip-background default TRUE with three sites asserting the
    opposite, erfc relative precision, CFACTOR sigma, constrained
    uncertainties, NaN filtering).
  - **Profile subsystem retains no ROI:** R2-2..R2-6 — width/method
    edits and data changes have no recompute trigger to act on
    (structural), plus the −0.5 corner→centre convention dropped at
    every caller and nan-policy drift.
  - **Recurrences of closed R1 families in new sites:** R2-48 (3D
    wheel per-frame vs per-event — R1-8 family), R2-59 (calc:// plain
    dialect silent-dead-channel — R1-25/29 family), R2-56 (a unit
    test locking in the divergent behaviour — the R1 test-skepticism
    class).

  Classification (per port-translation-lessons):
  - Reference-independent defects (real regardless of upstream):
    R2-4 (dead profile controls), R2-7 (median filter compounds),
    R2-13 (colorbar ticks at wrong positions), R2-30 (erfc collapse →
    zero-gradient stall), R2-41 (log Min:0 render collapse), R2-47
    (translucent surfaces hide interior data), R2-53 (per-tick puts
    to hardware), R2-56 (Fortran reshape wrong geometry, locked by a
    unit test), R2-59 (silent dead calc channels), R2-61 (widgets
    wrongly hidden at value 0).
  - Reference-faithful gaps: R2-2, R2-3, R2-5, R2-6, R2-8..R2-11,
    R2-15, R2-19..R2-22, R2-24..R2-29, R2-31..R2-40, R2-42..R2-46,
    R2-48, R2-50, R2-52, R2-54, R2-55, R2-57, R2-58, R2-60.
  - Interop-contract gaps (.adl file format is the contract):
    R2-62..R2-69 (and R2-61 doubles here).
  - Unimplemented surface (port or record a scope decision): R2-12
    (scatter-mask ellipse/line/pencil), R2-14 (per-bound autoscale),
    R2-16 (asinh axis scale + tool buttons), R2-17 (SyncAxes
    scale/direction), R2-49 (complex per-child modes), R2-51
    (displayValuesBelowMin).

- 2026-07-04/05: **R2 fix round complete — all 69 findings cleared**
  (one commit per finding, per-crate gates green at each commit,
  full-workspace gate green at the end; 141 fix commits on top of the
  doc-only baseline `227cdec`). One reported UNFIXED item — a
  `sidm::ca_ioc` test flaky under concurrent nextest — was diagnosed as
  a **real sidm concurrency bug**, not environmental: `StateWriter::
  post_value` released the state write lock before publishing the value
  event, so a subscriber registering in that window received an
  already-past value. Fixed structurally (`0a51e4e`) by publishing under
  the lock + a 2000-iteration race regression test.

- 2026-07-05: **round 3 opened** as a CONVERGENCE CHECK; same 5-agent
  split, scopes rotated to surfaces R2 left thin (A: silx actions/
  legend/stats/positioninfo, B: items/colors/ticks/fit-engine round 3,
  C: plot3d interaction/tools round 3, D: sidm↔PyDM plugin/format round
  3, E: adl2sidm↔MEDM composite/macro/geometry round 3). Added a
  mandatory **fix-verification lens**: every R2 fix in `227cdec..HEAD`
  was re-read line-by-line against the exact upstream it claims to port.
- 2026-07-05: round 3 consolidated — **24 findings** (High 0, Medium 9,
  Low 15), renumbered R3-1..R3-24 (A: 1–4, B: 5–6, C: 7–11, D: 12–15,
  E: 16–24). Baseline: post-R2-fix + post-flake-fix HEAD `0a51e4e`.

  **Fix-verification result:** all 69 R2 fixes verified faithful *at the
  cited site* — no R2 fix was wrong where it landed. The value of the
  lens was the residuals it surfaced: **10 of the 24 R3 findings are
  fix-residuals** — a fix that closed the cited line but not the sibling
  in the same reference function, or whose doc note mis-classified the
  remaining half: R3-1 (R2-18), R3-2/R3-3 (R2-20/25), R3-5 (R2-38), R3-6
  (R2-14/41/46), R3-9 (R2-51), R3-10 (R2-47), R3-13 (R2-59), R3-14
  (R2-54), R3-15 (R1-32), R3-19 (R2-68), R3-22 (R2-69), R3-23 (R2-64).
  This is the textbook "citation is a sample of the family, not the
  population" pattern from the fixes-from-reported-defects rule, now
  measured: the R2 fixes were correct but under-swept, and a fix whose
  reference is a single upstream function must port that whole function,
  not the one branch the finding named.

  Thematic clusters:
  - **Whole-function under-sweep (the dominant R3 theme):** R3-6 ported
    the per-bound autoscale of `_getColormapRange` but not its ordering-
    clamp tail; R3-5 rewired the numeric tick arm but not its TimeSeries
    sibling; R3-9/R3-10 aligned the blend state of the scene GL block but
    not the depth-write/depth-func lines in the same block. The fix and
    the residual live in the same reference function every time.
  - **Format-owner bypass (silx `%g`):** R3-3 (`{:.4}` where the byte-
    faithful `format_significant` owner exists), R3-4 (`%.7g` coord cells
    vs `str(tuple)`), R3-11 (`Display` claimed as `%g`) — the R2-25
    single-owner fix did not reach every call site, and three doc
    comments now falsely attribute the divergent format to silx.
  - **adl2sidm composite-file is a whole-feature gap, not an edge:**
    R3-16 (macros merged vs replaced) + R3-17 (no bbox refit/translate)
    are two halves of `compositeFileParse`/`medmComposite.c:659-736`
    that the include path skips — the only genuinely large R3 cluster.
  - **Silent-drop family persists in adl2sidm:** R3-18 (per-pen limits),
    R3-19 (absent-key POINT_PLOT default), R3-20 (external cmap chain),
    R3-21 (per-trace yaxis/yside) all bypass the emitter's own warn
    convention — the same class as R2-63/64/65/67/68, now in new sites.
  - **sidm plugin config semantics:** R3-12 (`loc://` fabricates from
    first listener vs PyDM's configured-listener gate) + R3-13 (`calc://`
    array child is the silent-dead-channel R2-59 was raised to close).

  Classification (per port-translation-lessons):
  - Reference-independent defects (real regardless of upstream): R3-6
    (inverted-range render collapse), R3-9 (below-min hole occludes),
    R3-12 (`loc://` fabricated connected value), R3-13 (silent dead calc
    channel).
  - Reference-faithful gaps (adopt upstream posture): R3-1..R3-5, R3-7,
    R3-8, R3-10, R3-11, R3-14, R3-15.
  - Interop-contract gaps (.adl file format / macro grammar is the
    contract): R3-16..R3-24.
  - Unimplemented surface (port or record a scope decision): R3-7 (3D
    interactive-mode surface), R3-8 (Isosurface visibility), R3-20
    (external colormap file).

  **Convergence verdict:** R1 40 → R2 69 → R3 24 (and 0 High vs R1's 4 /
  R2's 3). The count is falling and the severity ceiling dropped to
  Medium, but the surface is NOT yet converged: 9 Medium findings remain,
  two of them whole-feature gaps (composite-file include R3-16/17,
  external colormap R3-20). Per port-translation-lessons, this is
  consistent with "audit cycle K of K+1/K+2 before the language-gap
  categories close structurally" — the dominant remaining mechanism is
  under-swept fixes (10/24), which a fixes-from-reported-defects
  `rg`-the-whole-reference-function discipline on the next fix round
  would close as a class rather than one site at a time.
