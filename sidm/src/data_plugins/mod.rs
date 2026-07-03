//! Data plugins — the protocol backends behind `protocol://address` channels.
//!
//! Mirrors `pydm/data_plugins/`. A [`DataPlugin`] owns the logic for one
//! protocol (`loc`, `fake`, `ca`, `pva`, `calc`). The engine creates a
//! connection (shared state + write queue + cancellation token) and hands the
//! plugin a [`ConnectionCtx`]; the plugin spawns a task on the supplied runtime
//! handle that publishes [`crate::ChannelState`] updates through
//! [`crate::channel::StateWriter`] and consumes queued writes.

use crate::address::PvAddress;
use crate::channel::{PvValue, StateWriter};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "calc")]
pub mod calc_plugin;
pub mod epics_plugins;
pub mod fake_plugin;
pub mod local_plugin;

/// Everything a plugin needs to service one connection.
///
/// Destructure it in [`DataPlugin::connect`] and move the parts into the task
/// you spawn on [`ConnectionCtx::runtime`].
pub struct ConnectionCtx {
    /// Publishes state updates to the GUI (bumps the stamp, requests repaint).
    pub writer: StateWriter,
    /// Values queued by `Channel::put` on the GUI thread.
    pub writes: mpsc::UnboundedReceiver<PvValue>,
    /// Fired when the last `Channel` for this connection drops — the task must
    /// observe it and exit.
    pub cancel: CancellationToken,
    /// The runtime to spawn the connection task on.
    pub runtime: tokio::runtime::Handle,
    /// The parsed address (including query parameters, e.g. `loc` init values).
    pub address: PvAddress,
}

/// Whether global read-only mode is enabled via the `SIDM_READ_ONLY`
/// environment variable — the sidm equivalent of PyDM's `pydm --read-only`
/// flag (`data_plugins.set_read_only` / `is_read_only`,
/// `pydm/data_plugins/__init__.py:288-309`). Read once at plugin
/// construction; in read-only mode the EPICS plugins drop every put.
#[cfg(any(feature = "ca", feature = "pva"))]
pub(crate) fn env_read_only() -> bool {
    read_only_flag(std::env::var("SIDM_READ_ONLY").ok().as_deref())
}

/// Parse the `SIDM_READ_ONLY` value: unset, empty and the usual "off" spellings
/// (`0`, `false`, `no`, `off`, any case) disable it; anything else enables it.
#[cfg(any(feature = "ca", feature = "pva"))]
fn read_only_flag(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
    }
}

/// A protocol backend. One instance is registered per protocol; the engine
/// calls [`DataPlugin::connect`] once per distinct connection (PyDM keys the
/// connection pool by `scheme://full_address`).
pub trait DataPlugin: Send + Sync + 'static {
    /// The protocol scheme this plugin handles (`"loc"`, `"ca"`, …).
    fn protocol(&self) -> &'static str;

    /// Start servicing a connection. Spawn the connection task on
    /// `ctx.runtime`; return `Ok(())` once spawned (connection liveness is
    /// reported asynchronously through `ctx.writer`).
    fn connect(&self, ctx: ConnectionCtx) -> Result<(), crate::engine::EngineError>;
}

#[cfg(all(test, any(feature = "ca", feature = "pva")))]
mod tests {
    use super::read_only_flag;

    #[test]
    fn read_only_flag_parses_common_spellings() {
        // Unset / empty / usual "off" spellings → not read-only.
        assert!(!read_only_flag(None));
        assert!(!read_only_flag(Some("")));
        assert!(!read_only_flag(Some("  ")));
        assert!(!read_only_flag(Some("0")));
        assert!(!read_only_flag(Some("false")));
        assert!(!read_only_flag(Some("FALSE")));
        assert!(!read_only_flag(Some("no")));
        assert!(!read_only_flag(Some("off")));
        assert!(!read_only_flag(Some(" Off ")));
        // Anything else enables it.
        assert!(read_only_flag(Some("1")));
        assert!(read_only_flag(Some("true")));
        assert!(read_only_flag(Some("TRUE")));
        assert!(read_only_flag(Some("yes")));
        assert!(read_only_flag(Some("on")));
    }
}
