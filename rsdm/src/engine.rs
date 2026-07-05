//! The data engine — plugin registry, connection pool, and the tokio runtime
//! that connection tasks live on.
//!
//! Mirrors `pydm/data_plugins/__init__.py` (the plugin registry and
//! `establish_connection`) plus the per-plugin connection pool of
//! `pydm/data_plugins/plugin.py`. A single [`Engine`] owns:
//!
//! - a tokio runtime (its own, or a borrowed [`tokio::runtime::Handle`]),
//! - a protocol → [`DataPlugin`] registry,
//! - a connection pool keyed by `scheme://full_address` holding `Weak`
//!   references, so repeated [`Engine::connect`] calls for the same address
//!   share one connection and refcounting closes it when the last `Channel`
//!   drops.
//!
//! [`DataPlugin`]: crate::data_plugins::DataPlugin

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, Weak};

use rsplot::egui;

use crate::address::PvAddress;
use crate::channel::{Channel, Connection, RepaintHook};
use crate::data_plugins::fake_plugin::FakePlugin;
use crate::data_plugins::local_plugin::LocalPlugin;
use crate::data_plugins::{ConnectionCtx, DataPlugin};

/// Errors from connecting a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineError {
    /// The address had no `scheme://` prefix and no default protocol is set.
    NoProtocol(String),
    /// No plugin is registered for the address's protocol.
    UnknownProtocol(String),
    /// The plugin failed to start the connection.
    PluginError(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProtocol(addr) => {
                write!(f, "no protocol in address '{addr}' and no default set")
            }
            Self::UnknownProtocol(proto) => write!(f, "no data plugin for protocol '{proto}'"),
            Self::PluginError(msg) => write!(f, "data plugin error: {msg}"),
        }
    }
}

impl std::error::Error for EngineError {}

type Pool = Arc<Mutex<HashMap<String, Weak<Connection>>>>;

struct EngineInner {
    handle: tokio::runtime::Handle,
    plugins: RwLock<HashMap<String, Arc<dyn DataPlugin>>>,
    pool: Pool,
    default_protocol: RwLock<Option<String>>,
    repaint: RepaintHook,
}

/// The data engine. Cheap to clone (`Arc` inside) and `Send + Sync`, so it can
/// be stored in an app struct and shared with widgets.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
    // Keeps an owned runtime alive for the engine's lifetime (RAII; never read).
    // `None` when the engine was built on a borrowed handle
    // ([`Engine::with_handle`]).
    _runtime: Option<Arc<tokio::runtime::Runtime>>,
}

impl Engine {
    /// Create an engine with its own 2-worker tokio runtime and the always-on
    /// plugins registered (`loc://`). Must not be called from within a tokio
    /// runtime.
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rsdm-engine")
            .enable_all()
            .build()
            .expect("build rsdm tokio runtime");
        let handle = runtime.handle().clone();
        let engine = Self::with_handle(handle);
        Self {
            _runtime: Some(Arc::new(runtime)),
            ..engine
        }
    }

    /// Create an engine on an existing tokio runtime handle (the caller owns
    /// the runtime), with the always-on plugins registered.
    pub fn with_handle(handle: tokio::runtime::Handle) -> Self {
        let inner = Arc::new(EngineInner {
            handle,
            plugins: RwLock::new(HashMap::new()),
            pool: Arc::new(Mutex::new(HashMap::new())),
            default_protocol: RwLock::new(None),
            repaint: RepaintHook::default(),
        });
        let engine = Self {
            inner,
            _runtime: None,
        };
        // Always-on, dependency-free plugins.
        engine.register_plugin(Arc::new(LocalPlugin));
        engine.register_plugin(Arc::new(FakePlugin));
        // EPICS Channel Access backend (feature `ca`). Replaceable via
        // `register_plugin` (e.g. a test pointing the CA client at a loopback
        // IOC through `EPICS_CA_ADDR_LIST`).
        #[cfg(feature = "ca")]
        engine.register_plugin(Arc::new(
            crate::data_plugins::epics_plugins::ca_plugin::CaPlugin::new(),
        ));
        // EPICS pvAccess backend (feature `pva`). Replaceable via
        // `register_plugin` (e.g. a test pointing the PVA client at a loopback
        // server through `PvaPlugin::with_server`).
        #[cfg(feature = "pva")]
        engine.register_plugin(Arc::new(
            crate::data_plugins::epics_plugins::pva_plugin::PvaPlugin::new(),
        ));
        // Derived-channel backend (feature `calc`). The plugin opens its child
        // channels through a connector capturing a `Weak<EngineInner>`, so the
        // engine→plugin→connector chain holds no strong cycle and the engine
        // still drops when its last clone does.
        #[cfg(feature = "calc")]
        {
            let weak = Arc::downgrade(&engine.inner);
            let connector: crate::data_plugins::calc_plugin::ChildConnector =
                Arc::new(move |addr: &str| match weak.upgrade() {
                    Some(inner) => Engine::connect_inner(&inner, addr),
                    None => Err(EngineError::PluginError(
                        "engine dropped before calc child connect".to_owned(),
                    )),
                });
            engine.register_plugin(Arc::new(crate::data_plugins::calc_plugin::CalcPlugin::new(
                connector,
            )));
        }
        engine
    }

    /// Register (or replace) the plugin for its protocol.
    pub fn register_plugin(&self, plugin: Arc<dyn DataPlugin>) {
        self.inner
            .plugins
            .write()
            .expect("plugin registry poisoned")
            .insert(plugin.protocol().to_owned(), plugin);
    }

    /// Set the protocol applied to addresses with no `scheme://` prefix
    /// (PyDM `PYDM_DEFAULT_PROTOCOL`).
    pub fn set_default_protocol(&self, protocol: &str) {
        *self
            .inner
            .default_protocol
            .write()
            .expect("default protocol poisoned") = Some(protocol.to_owned());
    }

    /// Attach (or replace) the egui context to repaint when channel values
    /// change. Affects existing and future connections.
    pub fn attach_repaint(&self, ctx: egui::Context) {
        self.inner.repaint.set(ctx);
    }

    /// Number of live connections in the pool (a diagnostic; also exercised by
    /// the refcount tests).
    pub fn connection_count(&self) -> usize {
        self.inner
            .pool
            .lock()
            .expect("connection pool poisoned")
            .len()
    }

    /// Connect a channel by address, reusing an existing connection when one is
    /// already pooled for the same `scheme://full_address`.
    pub fn connect(&self, address: &str) -> Result<Channel, EngineError> {
        Self::connect_inner(&self.inner, address)
    }

    /// The body of [`Engine::connect`], operating on the shared internals
    /// directly so a `Weak<EngineInner>`-capturing connector (the `calc://`
    /// plugin's child opener) can drive it without an owned [`Engine`] — which
    /// would form a reference cycle through the plugin registry.
    fn connect_inner(inner: &Arc<EngineInner>, address: &str) -> Result<Channel, EngineError> {
        let parsed = Self::resolve_address(inner, address)?;
        let key = parsed.connection_id();

        // Fast path: reuse a live pooled connection. Hand the plugin task this
        // later listener's full address (with query) so a config-bearing
        // `loc://` address can configure a connection first opened by a bare
        // one — PyDM re-runs `_configure_local_plugin` on every `add_listener`,
        // whereas the pool key drops the query.
        {
            let pool = inner.pool.lock().expect("connection pool poisoned");
            if let Some(existing) = pool.get(&key).and_then(Weak::upgrade) {
                existing.forward_listener(parsed);
                return Ok(Channel::new(existing));
            }
        }

        let scheme = parsed
            .scheme()
            .expect("resolve_address guarantees a scheme")
            .to_owned();
        let plugin = inner
            .plugins
            .read()
            .expect("plugin registry poisoned")
            .get(&scheme)
            .cloned()
            .ok_or(EngineError::UnknownProtocol(scheme))?;

        let (conn, writer, writes, listeners, cancel) = Connection::new(
            parsed.clone(),
            inner.repaint.clone(),
            Arc::downgrade(&inner.pool),
            key.clone(),
        );

        plugin.connect(ConnectionCtx {
            writer,
            writes,
            listeners,
            cancel,
            runtime: inner.handle.clone(),
            address: parsed,
        })?;

        // Insert only after the plugin successfully started the task.
        inner
            .pool
            .lock()
            .expect("connection pool poisoned")
            .insert(key, Arc::downgrade(&conn));

        Ok(Channel::new(conn))
    }

    /// Parse `address`, applying the default protocol; error if it ends up with
    /// no scheme.
    fn resolve_address(inner: &Arc<EngineInner>, address: &str) -> Result<PvAddress, EngineError> {
        let mut parsed = PvAddress::parse(address);
        if parsed.scheme().is_none()
            && let Some(default) = inner
                .default_protocol
                .read()
                .expect("default protocol poisoned")
                .clone()
        {
            parsed = parsed.with_default_protocol(&default);
        }
        if parsed.scheme().is_none() {
            return Err(EngineError::NoProtocol(address.to_owned()));
        }
        Ok(parsed)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
