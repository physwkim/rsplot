# rsdm

**RsDM — a PyDM-style EPICS display layer for [rsplot](https://crates.io/crates/rsplot).**

`rsdm` is a Rust port of [PyDM](https://github.com/slaclab/pydm)'s core engine
and widgets (a PyQt EPICS display manager), built on top of `rsplot`'s
egui/wgpu plotting and `epics-rs` (Channel Access + pvAccess) as the data
backend. PyDM depends on pyqtgraph the way this crate depends on `rsplot`.

License: MIT OR Apache-2.0.

---

## Layout

The crate mirrors PyDM's package layout:

- **`data_plugins`** — the channel/connection engine: a `protocol://address`
  registry of `DataPlugin`s (`loc`, `fake`, `ca`, `pva`, `calc`), each owning
  per-PV connections that publish a `ChannelState` snapshot read by widgets
  every frame. Qt's per-slot signals collapse into one `Arc`-shared,
  repaint-on-update state cell because egui re-renders from current state each
  frame.
- **`widgets`** — retained widget structs (`RsdmLabel`, `RsdmLineEdit`,
  `RsdmByteIndicator`, the time/waveform/scatter plots, the camera image view,
  …) that read their channel's state and draw with alarm-severity styling,
  connection gating, and precision/unit formatting.

## Backends

Backends are feature-gated. `loc://` and `fake://` are always compiled, so
`--no-default-features` gives the headless, dependency-light core exercised
with no live IOC.

| Feature | Default | Protocol | Pulls |
| :-- | :--: | :-- | :-- |
| `ca` | ✓ | `ca://` (Channel Access) | `epics-ca-rs`, `epics-base-rs` |
| `pva` | ✓ | `pva://` (pvAccess) | `epics-pva-rs` (+ compressed-NTNDArray codecs) |
| `calc` | ✓ | `calc://` (derived expressions) | the pure-Rust expression evaluator |

## Relationship to PyDM

`rsdm` ports PyDM's engine and widgets to Rust, following the upstream
behaviour to fine detail (alarm styling, connection gating, the `calc://`
trigger rules, the `ca://`/`pva://` metadata model). PyDM itself is the
reference; throughout the code and docs, bare `pydm` refers to the upstream
Python project, not to this crate.

## License

Licensed under either of

- Apache License, Version 2.0
- MIT license

at your option.
