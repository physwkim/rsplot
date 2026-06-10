# sidm → PyDM parity roadmap

Tracks the port of [PyDM](https://github.com/slaclab/pydm) (`~/codes/pydm`,
a PyQt EPICS display manager) into the **`sidm`** workspace crate, layered on
`siplot` (egui/wgpu plotting) with `epics-rs` (`~/codes/epics-rs` — crates.io
`epics-ca-rs` / `epics-pva-rs` / `epics-base-rs` 0.18.x) as the EPICS backend.
crates.io dependencies are permitted for this crate (an explicit deviation from
siplot's no-new-dependency rule).

PyDM depends on pyqtgraph the way `sidm` depends on `siplot`. The port mirrors
PyDM's package shape: a `data_plugins` engine (channel/connection registry) and
a `widgets` set, with pure cores tested headlessly and GPU/UI honestly reported
"GPU-unverified" / "IOC-unverified".

Plan of record: `~/.claude/plans/deep-growing-balloon.md`.

## Architecture decisions

- **Workspace + new crate `sidm`.** siplot stays tokio/EPICS-free (it is a
  published plotting library). `sidm` carries the runtime + EPICS dependencies.
- **Qt signals → per-frame snapshot.** No slot fan-out. The tokio side writes an
  `Arc<RwLock<ChannelState>>` (with a monotonic `stamp` for change detection)
  and calls `egui::Context::request_repaint()`. Writes (GUI → engine) flow the
  other way over an unbounded mpsc.
- **Feature gating.** `ca`, `pva`, `calc` are features, all default-on (`ca`/
  `pva` pull the EPICS backends, `calc` pulls pure-Rust evalexpr); `loc://`/
  `fake://` are always compiled, so `--no-default-features` is the headless,
  dependency-light core.
- **Deferred** (tracked, not dropped): rules engine, `.ui`/`.adl` display
  loading, `archiver://` + archiver time plot, embedded display / template
  repeater / related-display navigation / shell command / log display.

## Status legend

✅ Done · ◐ Partial · ☐ Missing · N/A not applicable

## Engine (`data_plugins/`, `channel`, `engine`, `address`, `utilities`)

| # | Item | Status | Notes |
|---|------|--------|-------|
| E1 | Workspace + `sidm` crate scaffold | ✅ | scaffold commit |
| E2 | `PvAddress` parse + macro substitution | ✅ | `address.rs`, `utilities/macros.rs` |
| E3 | `PvValue` / `AlarmSeverity` / `ChannelState` core | ✅ | `channel.rs` |
| E4 | `Engine` + `DataPlugin` registry + `loc://` | ✅ | `engine.rs`, `channel.rs` live types, `local_plugin.rs` |
| E5 | `fake://` generators | ✅ | `fake_plugin.rs`, `tests/engine_fake.rs` |
| E6 | `ca://` plugin + in-process IOC test | ✅ | `epics_plugins/ca_plugin.rs`, `tests/ca_ioc.rs`; feature `ca` (default-on), crates.io epics-ca-rs/epics-base-rs 0.18 |
| E7 | Write path (`PvValue`→`EpicsValue`, string→enum) | ✅ | `ca_plugin.rs` `pv_to_epics` (native-type coercion, label→enum), disconnected-drop, no local echo; `CaPlugin::with_addresses`; enum-put IOC test |
| E8 | `pva://` plugin (`apply_ntscalar`) | ✅ | `epics_plugins/pva_plugin.rs`, `tests/pva_ioc.rs`; feature `pva` (default-on), crates.io epics-pva-rs 0.18. Monitor-callback → NTScalar/NTEnum `apply_ntscalar` (value/alarm/timeStamp/display/control/valueAlarm); write path `pv_to_pva_put` (`.value` string PUT, NTEnum label→`value.index`). **Live path verified** via in-process `PvaServer::isolated` round-trip (not IOC-unverified) |
| E9 | `calc://` (evalexpr) | ✅ | `calc_plugin.rs`, `tests/calc_derived.rs`; feature `calc` (default-on), crates.io evalexpr 13. PyDM `calc://name?expr=…&A=child&B=child&update=A,B`: pure `CalcConfig::parse` + `pv_to_evalexpr`/`evalexpr_to_pv`; engine injects a `Weak`-capturing `ChildConnector` (closes the plugin↔engine cycle); poll-based recompute (no async waker in the snapshot model); connected iff all children connected; scalar children only; `prev_res` supported |

## Widgets (`widgets/`)

| # | Item | Status | Notes |
|---|------|--------|-------|
| W0 | `display_format` formatter (pure) | ✅ | `widgets/display_format.rs`; `DisplayFormat` + `FormatSpec` + `format_value` porting `display_format.py` `parse_value_for_display` + `base.py` precision/unit + `label.py` enum→label. Deviations documented: no-value → `""` (no stray unit suffix), negative/out-of-range enum index → `**INVALID**`. 38 unit tests |
| W1 | `ChannelBase` + alarm styling | ✅ | `widgets/base.rs`; `severity_color`/`alarm_border` (PyDM `default_stylesheet.qss` palette: MINOR `#EBEB00`, MAJOR `#FF0000`, INVALID `#EB00EB`, DISCONNECTED dashed `#FFFFFF`) + `ChannelBase` (border/content_color/enabled/tooltip/`framed`). Pure decisions unit-tested; `framed` border rendering verified by headless wgpu readback (`tests/widget_base_render.rs`: solid red/yellow, dashed-white-with-gaps, no-border) |
| W2 | PydmLabel | ✅ | `widgets/label.rs`; read-only value display over `ChannelBase` + `format_value`. Disconnected → shows the channel address (PyDM `check_enable_state`); content (text) recolour via `alarmSensitiveContent`. Pure `display_text` headlessly tested (precision/units, enum→label, disconnected→address, live write) |
| W3 | PydmLineEdit | ✅ | `widgets/line_edit.rs`; writable entry over `ChannelBase`. Pure `parse_input` ports `send_value` (channeltype-keyed: float/int/bool/enum/str/array; format-aware radix/float; unit-strip); focus-frozen buffer, Enter commits → `put`, no local echo (resyncs from monitor); returns `Option<PvValue>`. 14 parse tests (hex±prefix, binary, decimal-truncates-int, float-hex-widen, strtobool, enum index/label, units strip, array round-trip, char-waveform) |
| W4 | PydmByteIndicator | ✅ | `widgets/byte.rs`; per-bit LED grid. Pure `extract_bits` (shift<0 ⇒ `<<\|shift\|` else `>>shift`, bit i = `(v>>i)&1`, LSB-first) + `bit_color` (byte palette on `0,255,0` / off `100,100,100` / disconnected white / INVALID `255,0,255`). H/V orientation, circles/squares, big-endian display order, per-bit labels. 9 unit tests; on/off rendering verified by wgpu readback (`tests/widget_byte_render.rs`). Blink mode not ported |
| W5 | PydmCheckbox + PydmPushButton | ✅ | `widgets/checkbox.rs` (checked iff value>0; toggle writes 1/0, Bool channel keeps Bool) + `widgets/push_button.rs` (pure `compute_send_value`: absolute or `current+press` for numeric `relative`; optional release write; confirm via `egui::Modal`). Pure logic + live-write tests (18 tests total). Password protection / momentary press-vs-release timing not ported |
| W6 | PydmEnumComboBox + PydmSpinbox + PydmSlider | ✅ | `widgets/enum_combo_box.rs` (items = enum strings; current index from int/enum/bool, or string via `findText`; pick writes the integer index) + `widgets/spinbox.rs` (decimals from precision, range from `control_range`, writes float on change; PyDM `step_exponent` → builder `step` default `10^-precision`) + `widgets/slider.rs` (101 positions default, range from `control_range`, `step_by = (hi-lo)/(num_steps-1)`, disabled when no limits — PyDM `needs_limit_info`). Shared `control_range` (user limits over ctrl limits) added to `base.rs`. Pure logic (options/current_index, decimals/step_size, control_range) + live-write tests, 12 tests |
| P1 | `ring_buffer` (pure) | ✅ | `widgets/ring_buffer.rs`; `TimeSeriesBuffer` — capacity-bounded FIFO of `(x,y)` samples, overwrite-oldest, yielding oldest→newest (ports `TimePlotCurveItem` `np.roll` + `points_accumulated` cap as a `VecDeque`). `push`/`ordered_into`/`oldest`/`newest`/`set_capacity` + `MINIMUM_BUFFER_SIZE`=2 / `DEFAULT_BUFFER_SIZE`=18000. Deviation: `set_capacity` keeps newest samples (PyDM `setBufferSize` clears). 6 tests |
| P2 | PydmTimePlot | ✅ | `widgets/time_plot.rs`; scrolling strip chart over `Plot1D`. Per channel = curve + [`TimeSeriesBuffer`] + item handle; both PyDM update modes (`UpdateMode::OnValueChange` via stamp-change detection, `AtFixedRate` at `update_rate_hz`, default 1 Hz). Pure `CurveFeed::ingest` + `is_rate_due`/`update_interval` unit-tested; curve rendering verified by headless wgpu readback (`tests/widget_time_plot_render.rs`: injected ramp renders the curve colour, empty plot does not). **Deviation — relative-time X, not absolute datetime:** siplot's GPU vertices + ortho are `f32`, so absolute epoch X (~1.7e9) collapses under catastrophic cancellation and no curve renders; the buffer keeps absolute epochs but feeds siplot `t - t0` (PyDM "plot by relative time"). Absolute `TickMode::TimeSeries` axis needs a siplot `f64` vertex rebase (out of scope). 7 tests |
| P3 | PydmWaveformPlot + PydmScatterPlot | ✅ | `widgets/waveform_plot.rs` (Y array channel + optional X array channel; X/Y length-aligned, Y-vs-index when no X — PyDM `redrawCurve`) + `widgets/scatter_plot.rs` (paired X/Y scalar channels accumulated into a `(x,y)` [`TimeSeriesBuffer`], drawn as markers). Shared `RedrawMode` (OnEither/OnX/OnY/OnBoth) + pure `mode_allows` gate (PyDM `updateData`/`update_buffer`, `pending_*` = inverse of `needs_new_*`) + pure `value_to_waveform` (array/scalar → `Vec<f64>`). Both poll snapshots by stamp; scatter `inject` for replay. Pure gating/extraction unit-tested; curve + markers rendering verified by headless wgpu readback (`tests/widget_array_plots_render.rs`). 5 tests |
| P4 | PydmImageView | ✅ | `widgets/image_view.rs`; a flat array channel (+ optional width channel, PyDM `widthChannel`) reshaped to `height × width` and pushed to `siplot::ImageView::set_image`. Pure `reshape_image` (C/Fortran reading order — Fortran transposed into row-major; width 0 or sub-row data → no image, trailing partial row dropped, PyDM `ImageUpdateThread.run`), `value_to_image` (float/int arrays only), and `color_range` (manual `colorMapMin`/`colorMapMax` vs `normalizeData` data min/max, degenerate range widened) unit-tested. Width-channel/image-channel polled by stamp; image re-uploaded only when the array or width changed (`dirty`), colormap `ColormapName::Viridis` default. Rendering verified by headless wgpu readback (`tests/widget_image_view_render.rs`: a 16×16 gradient renders a colour-mapped image, an image-less view does not — colorbar/side-histograms hidden in the test to isolate the array→image pipeline). 6 unit tests. **Deviation:** scalar/string values are not images (PyDM accepts only array data here); 2-D dimension-order beyond a single width is out of scope (`PvValue` arrays are 1-D) |

## Examples

| # | Item | Status | Notes |
|---|------|--------|-------|
| X1 | `pydm_local_panel` (`loc://`, no IOC) | ✅ | `examples/pydm_local_panel.rs`; eframe/wgpu window. A `fake://` sine drives a `PydmLabel` + scrolling `PydmTimePlot` (one pooled connection); a shared `loc://` float setpoint is edited from `PydmLineEdit` + `PydmSlider` and read back by a `PydmLabel` (single-owner value, no local echo); a `loc://` int is entered as hex and shown on a `PydmByteIndicator`. `required-features = []` (runs on the headless core). `eframe = "0.34"` added to dev-deps |
| X2 | `pydm_ca_panel` (`ca://`) | ✅ | `examples/pydm_ca_panel.rs`; same widgets over live `ca://` PVs named on the command line (`-- <scalar_pv> [<flags_pv>]`). Disconnected PVs render the disconnected state (address in the label, dashed border); `required-features = ["ca"]`. IOC-unverified (no PVs running) |

## Tier 2 (follow-on, one commit each)

| # | Item | Status | Notes |
|---|------|--------|-------|
| T1 | PydmFrame | ✅ | `widgets/frame.rs`; channel-connected grouping container (PyDM `PyDMFrame`). Pure `frame_enabled` (always enabled unless `disable_on_disconnect` set and disconnected — PyDM `check_enable_state`); alarm border default *off* (PyDM frame default, unlike value widgets). `show(ui, add)` wraps a content closure via the new `ChannelBase::framed_with_enabled` (factored out of `framed` so the frame's enable rule differs without touching the value-widget call sites). 2 unit tests |
| T2 | PydmEnumButton | ✅ | `widgets/enum_button.rs`; an exclusive button group bound to the PV's enum strings (PyDM `PyDMEnumButton`). Push or radio buttons (`widgetType`), vertical/horizontal (`orientation`, reuses `byte::Orientation`); clicking writes the integer index. Pure `order_indices` (natural / `customOrder` with out-of-range dropped / `invertOrder`) unit-tested + live-write `select` test. The choices / current index / written value are the new shared `widgets/enum_choice.rs` owner (`enum_options`/`enum_current_index`/`enum_index_value`), which `PydmEnumComboBox` now also delegates to (one owner for the enum→index mapping, no drift). 4 tests |
| T3 | PydmSymbol | ✅ | `widgets/symbol.rs`; `PydmSymbol` drawing a distinct symbol per integer channel value (PyDM `PyDMSymbol`). Pure `value_as_state_key` (value → integer key; integral floats match int keys per Python dict semantics, fractional/non-finite/non-numeric → no key) + `symbol_index_for_value` (exact-key lookup, first match wins, miss/`None` → nothing — PyDM `_state_images.get(_current_key)`). 4 unit tests + headless wgpu readback (`tests/widget_symbol_render.rs`: matching state renders its symbol, unconfigured value renders nothing). Reuses `drawing::shape_points` (now `pub(crate)`) as the one owner of shape geometry. **Deviation:** PyDM maps each state to an image *file* (SVG/raster, Qt aspect-ratio scaling); loading arbitrary images needs a path resolver + decoders (out of scope), so each state is a `DrawingShape` + fill colour filling the bounds, and the aspect-ratio mode (an image-scaling concept) is not ported |
| T4 | drawing shapes (PyDMDrawing*) | ✅ | `widgets/drawing.rs`; `PydmDrawing` painting a `DrawingShape` (Rectangle/Ellipse/Circle/Triangle/Line) with fill (brush) + border (pen) + rotation, porting `PyDMDrawing` + the common subclasses. Pure `effective_colors` (alarm overrides fill via `alarmSensitiveContent` / border via `alarmSensitiveBorder`, default off) + `rotate`/`shape_points` (filled shapes unified through one `convex_polygon` path, ellipse/circle sampled as a polygon so rotation is free; border-width inset per PyDM `get_bounds`). 6 unit tests + headless wgpu readback (`tests/widget_drawing_render.rs`: red rectangle vs transparent control). **Deviation:** Polyline/Polygon/Arc/Pie/Chord/Image subclasses not ported (arcs/images need extra primitives) |
| T5 | PydmDateTimeLabel | ✅ | `widgets/datetime_label.rs`; read-only label rendering a numeric time channel as a date/time string (PyDM `PyDMDateTimeLabel`). Pure `value_epoch_ms` (truncate-then-scale, ms/s `TimeBase`, relative-to-now vs absolute epoch — PyDM `value_changed`) + `format_datetime_ms` (epoch ms → `YYYY/MM/DD hh:mm:ss.zzz` UTC via Howard Hinnant `civil_from_days`, no date dep). 7 unit tests. **Deviation:** only PyDM's default format is produced; arbitrary Qt `textFormat` strings are not ported (would require Qt's date-format mini-language) |
| T6 | PydmAnalogIndicator / PydmScaleIndicator | ✅ | `widgets/scale_indicator.rs`; `PydmScaleIndicator` drawing the value as a filled bar (`barIndicator`) or pointer on a `num_divisions` tick scale, horizontal or vertical, porting `QScale` + `PyDMScaleIndicator`. Pure `value_proportion` (mirrors PyDM `calculate_position_for_value`: non-finite / out-of-`[lower,upper]` / zero-span → off-scale `None`) + `division_proportions` (i/n ticks). Limits resolve via `control_range` (user override → PV control limits); optional `format_value` label; the bar is alarm-coloured when `alarmSensitiveContent` is set (folding in the analog indicator's alarm colouring). 3 unit tests + headless wgpu readback (`tests/widget_scale_indicator_render.rs`: bar grows with value, off-scale renders no bar). **Consolidation:** one widget covers the plain scale and the alarm-coloured bar; the analog indicator's separate set-point pointer and multi-region alarm shading are not ported |

## Tier 3 — plot features (one commit each)

Plot-focused extensions (UI/`.ui` loading and Qt-Designer curve editors are out
of scope by decision). Builds on the Wave-D plot widgets.

| # | Item | Status | Notes |
|---|------|--------|-------|
| PT1 | per-curve styling + Y-axis assignment | ✅ | `widgets/plot_style.rs`; shared `CurveStyle` (colour, `line_style`, `line_width`, `symbol`, `symbol_size`, `y_axis`) mirroring PyDM `BasePlotCurveItem` styling, with `to_spec` mapping to siplot `CurveSpec` (one owner). `PydmTimePlot`/`PydmWaveformPlot`/`PydmScatterPlot` gained `set_curve_style(index, style)`; assigning `YAxis::Right` enables the right (y2) axis autoscale. `DEFAULT_SYMBOL_SIZE` ownership moved to `plot_style` (scatter re-exports). 4 unit tests + headless wgpu readback (`tests/widget_plot_style_render.rs`: restyle recolours the curve, OOR index rejected). **Deviation:** PyDM `MultiAxisPlot`'s arbitrary named Y axes (`yAxisName`) reduce to siplot's two-axis left/right model |
| PT2 | PyDMEventPlot | ✅ | `widgets/event_plot.rs`; `PydmEventPlot` plotting `(x, y)` pairs selected from a single event-array PV (PyDM `PyDMEventPlot` + `EventPlotCurveItem`). Each update delivers an array; a fixed `(x_idx, y_idx)` selects the sample appended to a rolling `TimeSeriesBuffer`, drawn as markers. Pure `event_sample` (out-of-range index → ignored, per PyDM `len <= idx → return`) + the shared `value_to_waveform`; stamp-driven poll matches the sibling plot widgets. `add_channel(addr, x_idx, y_idx, color, legend)`, `inject`, `point_count`, `set_curve_style`. 2 unit tests + headless wgpu readback (`tests/widget_event_plot_render.rs`: real `loc://` event arrays accumulate and render markers; empty control). Reuses `CurveStyle::markers` + `TimeSeriesBuffer` (no new buffer/marker machinery) |
