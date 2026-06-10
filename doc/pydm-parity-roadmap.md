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
- **Feature gating.** `ca`, `pva`, `calc` are features (`ca`/`pva` default-on
  once wired); `loc://`/`fake://` are always compiled for headless tests.
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
| E5 | `fake://` generators | ☐ | commit 4 |
| E6 | `ca://` plugin + in-process IOC test | ☐ | commit 5 |
| E7 | Write path (`PvValue`→`EpicsValue`, string→enum) | ☐ | commit 6 |
| E8 | `pva://` plugin (`ntscalar_to_state`) | ☐ | commit 7 |
| E9 | `calc://` (evalexpr) | ☐ | commit 8 |

## Widgets (`widgets/`)

| # | Item | Status | Notes |
|---|------|--------|-------|
| W0 | `display_format` formatter (pure) | ☐ | commit 9 |
| W1 | `ChannelBase` + alarm styling | ☐ | commit 10 |
| W2 | PydmLabel | ☐ | commit 11 |
| W3 | PydmLineEdit | ☐ | commit 12 |
| W4 | PydmByteIndicator | ☐ | commit 13 |
| W5 | PydmCheckbox + PydmPushButton | ☐ | commit 14 |
| W6 | PydmEnumComboBox + PydmSpinbox + PydmSlider | ☐ | commit 15 |
| P1 | `ring_buffer` (pure) | ☐ | commit 16 |
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
