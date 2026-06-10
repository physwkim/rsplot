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
