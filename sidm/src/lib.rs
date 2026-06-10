//! `sidm` — PyDM-style EPICS display layer for [`siplot`].
//!
//! A Rust port of [PyDM](https://github.com/slaclab/pydm)'s core engine and
//! widgets (a PyQt EPICS display manager), built on top of `siplot`'s
//! egui/wgpu plotting and `epics-rs` (Channel Access + pvAccess) as the data
//! backend. PyDM depends on pyqtgraph the way this crate depends on `siplot`.
//!
//! The crate mirrors PyDM's package layout:
//!
//! - **`data_plugins`** — the channel/connection engine: a `protocol://address`
//!   registry of [`DataPlugin`]s (`loc`, `fake`, `ca`, `pva`, `calc`), each
//!   owning per-PV connections that publish a [`ChannelState`] snapshot read by
//!   widgets every frame. Qt's per-slot signals collapse into one
//!   `Arc`-shared, repaint-on-update state cell because egui re-renders from
//!   current state each frame.
//! - **`widgets`** — retained widget structs (`PydmLabel`, `PydmLineEdit`,
//!   `PydmByteIndicator`, the time/waveform/scatter plots, the camera image
//!   view, …) that read their channel's state and draw with alarm-severity
//!   styling, connection gating, and precision/unit formatting.
//!
//! Backends are feature-gated: `ca` and `pva` pull in `epics-ca-rs` /
//! `epics-pva-rs`; `calc` pulls in the expression evaluator. `loc://` and
//! `fake://` are always available so the engine and widgets are exercised
//! headlessly with no live IOC.
//!
//! [`siplot`]: https://docs.rs/siplot
//!
//! The live engine (`Engine`, `Channel`, the `DataPlugin` registry) and the
//! widgets land in subsequent commits; the pure address/value/macro cores are
//! available now.

pub mod address;
pub mod channel;
pub mod utilities;

pub use address::PvAddress;
pub use channel::{AlarmSeverity, ChannelState, PvValue};
