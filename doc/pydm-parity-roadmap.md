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
| P2 | PydmTimePlot | ☐ | commit 17 |
| P3 | PydmWaveformPlot + PydmScatterPlot | ☐ | commit 18 |
| P4 | PydmImageView | ☐ | commit 19 |

## Examples

| # | Item | Status | Notes |
|---|------|--------|-------|
| X1 | `pydm_local_panel` (`loc://`, no IOC) | ☐ | commit 20 |
| X2 | `pydm_ca_panel` (`ca://`) | ☐ | commit 20 |

## Tier 2 (follow-on, one commit each)

PydmFrame, PydmEnumButton, PydmSymbol, drawing shapes, PydmDateTimeLabel,
PydmAnalogIndicator / PydmScaleIndicator.
