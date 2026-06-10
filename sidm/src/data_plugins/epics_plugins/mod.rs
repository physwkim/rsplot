//! EPICS protocol backends (`ca://`, later `pva://`).
//!
//! Mirrors `pydm/data_plugins/epics_plugin.py` (and `epics_plugins/`): the CA
//! and PVA plugins that bridge PVs to [`crate::ChannelState`]. They are the only
//! plugins that pull a network/IOC dependency, so each lives behind a Cargo
//! feature (`ca`, `pva`). `loc://`/`fake://` are always compiled.

#[cfg(feature = "ca")]
pub mod ca_plugin;
#[cfg(feature = "pva")]
pub mod pva_plugin;
