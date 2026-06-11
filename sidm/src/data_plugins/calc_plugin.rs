//! `calc://` — derived channels (feature `calc`).
//!
//! Ports `pydm/data_plugins/calc_plugin.py`: a channel whose value is an
//! expression evaluated over other channels' scalar values. The address shape
//! mirrors PyDM —
//!
//! ```text
//! calc://name?expr=<expression>&A=<child-addr>&B=<child-addr>&update=A,B
//! ```
//!
//! — where `name` (the netloc) is the connection identity, `expr` is the
//! expression, each remaining query key is a **variable** bound to a child
//! channel address, and the optional `update` list restricts which variables
//! re-trigger evaluation (default: any variable). The connection is `connected`
//! only when **all** child channels are connected, and evaluation is skipped
//! until every variable has a scalar value (PyDM semantics). The previous result
//! is available to the expression as `prev_res`.
//!
//! Unlike PyDM's `eval()` over Python with numpy, sidm evaluates with
//! [`evalexpr`] (a pure-Rust evaluator: arithmetic, comparison, boolean, and the
//! built-in `math::*` functions). Only **scalar** children participate — an
//! array-valued child leaves its variable unset, so an expression referencing it
//! does not evaluate until/unless it carries a scalar.
//!
//! **No async wake on child updates.** The snapshot model publishes child values
//! through an `Arc<RwLock<ChannelState>>` + an egui repaint, not a tokio waker,
//! so the connection task **polls** each child's update `stamp` on a fixed
//! interval and recomputes when a triggering variable changed. This is the
//! right-sized fit for a handful of children at GUI rates; a notification
//! subsystem in `channel.rs` for this one plugin would not be.
//!
//! The plugin has no [`Engine`] handle of its own; the engine injects a
//! [`ChildConnector`] (capturing a `Weak` to the engine internals) so opening a
//! child channel routes through the same pool/plugin machinery as any
//! [`Engine::connect`], without forming a reference cycle.
//!
//! [`Engine`]: crate::Engine
//! [`Engine::connect`]: crate::Engine::connect

use std::sync::Arc;
use std::time::Duration;

use evalexpr::{ContextWithMutableVariables, HashMapContext, Value, eval_with_context};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::address::PvAddress;
use crate::channel::{Channel, PvValue, StateWriter};
use crate::data_plugins::{ConnectionCtx, DataPlugin};
use crate::engine::EngineError;

/// How often the connection task polls its children for new values. Children
/// publish via a shared state cell + egui repaint (no async waker), so the calc
/// task samples their update stamps at this cadence.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Opens a child channel by address, returning the live [`Channel`]. The engine
/// supplies one that captures a `Weak` to its internals, so the plugin can open
/// children through the normal pool/plugin path without keeping the engine
/// alive (which a strong [`crate::Engine`] handle stored in a plugin would).
pub type ChildConnector = Arc<dyn Fn(&str) -> Result<Channel, EngineError> + Send + Sync>;

/// The `calc://` data plugin (PyDM `CalculationPlugin`).
pub struct CalcPlugin {
    connector: ChildConnector,
}

impl CalcPlugin {
    /// Create the plugin with the engine-supplied child connector.
    pub fn new(connector: ChildConnector) -> Self {
        Self { connector }
    }
}

impl DataPlugin for CalcPlugin {
    fn protocol(&self) -> &'static str {
        "calc"
    }

    fn connect(&self, ctx: ConnectionCtx) -> Result<(), EngineError> {
        let ConnectionCtx {
            writer,
            writes,
            cancel,
            runtime,
            address,
        } = ctx;

        let config = CalcConfig::parse(&address).ok_or_else(|| {
            EngineError::PluginError(
                "calc:// requires ?expr=… and at least one variable=child-address".to_owned(),
            )
        })?;

        // Open every child up-front so a bad child address (unknown protocol,
        // dropped engine) surfaces as a connect error rather than a silently
        // never-connecting calc channel.
        let mut children = Vec::with_capacity(config.vars.len());
        for (name, child_addr) in &config.vars {
            let ch = (self.connector)(child_addr)?;
            children.push((name.clone(), ch));
        }

        runtime.spawn(run_channel(
            config.expr,
            config.update,
            children,
            writer,
            writes,
            cancel,
        ));
        Ok(())
    }
}

/// Service one `calc://` connection: poll children, recompute, publish.
async fn run_channel(
    expr: String,
    update: Option<Vec<String>>,
    children: Vec<(String, Channel)>,
    writer: StateWriter,
    mut writes: mpsc::UnboundedReceiver<PvValue>,
    cancel: CancellationToken,
) {
    // `u64::MAX` can never equal a real stamp, so the first poll after a child
    // first publishes registers as a change and triggers the initial eval.
    let mut prev_stamps = vec![u64::MAX; children.len()];
    let mut connected = false;
    let mut prev_value: Option<PvValue> = None;

    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            _ = ticker.tick() => {
                let all_connected = children.iter().all(|(_, ch)| ch.is_connected());

                // Note every child whose value changed; a change to a variable
                // in the `update` list (or any variable when no list is given)
                // triggers a recompute.
                let mut trigger = false;
                for (i, (name, ch)) in children.iter().enumerate() {
                    let stamp = ch.stamp();
                    if stamp != prev_stamps[i] {
                        prev_stamps[i] = stamp;
                        if update.as_ref().is_none_or(|u| u.iter().any(|n| n == name)) {
                            trigger = true;
                        }
                    }
                }

                if all_connected != connected {
                    connected = all_connected;
                    writer.update(move |s| s.connected = all_connected);
                }

                if all_connected && trigger
                    && let Some(value) = evaluate(&expr, &children, prev_value.as_ref())
                {
                    prev_value = Some(value.clone());
                    writer.post_value(move |s| {
                        s.connected = true;
                        s.value = Some(value);
                    });
                }
            }

            // `calc://` is read-only (write_access stays false); drain the queue
            // so a closed sender still breaks the loop.
            maybe = writes.recv() => match maybe {
                Some(_) => {}
                None => break,
            },
        }
    }
}

/// Evaluate `expr` against the children's current scalar values plus `prev_res`.
/// Returns `None` (skip) when any variable lacks a scalar value or the
/// expression fails to evaluate — matching PyDM's "skip until all values set".
fn evaluate(expr: &str, children: &[(String, Channel)], prev: Option<&PvValue>) -> Option<PvValue> {
    let mut ctx = HashMapContext::new();
    for (name, ch) in children {
        let value = ch.read(|s| s.value.clone());
        let var = value.as_ref().and_then(pv_to_evalexpr)?;
        ctx.set_value(name.clone(), var).ok()?;
    }
    if let Some(prev_var) = prev.and_then(pv_to_evalexpr) {
        // Best-effort: a missing `prev_res` only matters if the expression uses
        // it, in which case the eval below fails and we skip — same as PyDM.
        let _ = ctx.set_value("prev_res".to_owned(), prev_var);
    }
    evalexpr_to_pv(&eval_with_context(expr, &ctx).ok()?)
}

/// Bind a scalar [`PvValue`] as an [`evalexpr`] variable. Arrays are unsupported
/// in expressions and yield `None`.
fn pv_to_evalexpr(value: &PvValue) -> Option<Value> {
    Some(match value {
        PvValue::Int(n) => Value::Int(*n),
        PvValue::Float(f) => Value::Float(*f),
        PvValue::Bool(b) => Value::Boolean(*b),
        PvValue::Str(s) => Value::String(s.to_string()),
        PvValue::Enum { index, .. } => Value::Int(i64::from(*index)),
        _ => return None,
    })
}

/// Normalize an [`evalexpr`] result back into a [`PvValue`]. Tuple/empty results
/// have no channel representation and yield `None`.
fn evalexpr_to_pv(value: &Value) -> Option<PvValue> {
    Some(match value {
        Value::Int(n) => PvValue::Int(*n),
        Value::Float(f) => PvValue::Float(*f),
        Value::Boolean(b) => PvValue::Bool(*b),
        Value::String(s) => PvValue::Str(Arc::from(s.as_str())),
        Value::Tuple(_) | Value::Empty => return None,
    })
}

/// The parsed `calc://` configuration (PyDM `UrlToPython` + `CalcThread` config).
#[derive(Debug, PartialEq)]
struct CalcConfig {
    /// The expression to evaluate.
    expr: String,
    /// Variable name → child channel address, in URL order.
    vars: Vec<(String, String)>,
    /// Variables whose update triggers a recompute; `None` = any variable
    /// (PyDM's `update` query key, omitted → recompute on every value).
    update: Option<Vec<String>>,
}

impl CalcConfig {
    /// Parse the query into expression + variables + update list. Returns `None`
    /// when there is no non-empty `expr` or no variables — an unusable config.
    fn parse(address: &PvAddress) -> Option<Self> {
        // PyDM RESERVED_FIELD: `expr`, `update`, `name` are not variables.
        let mut expr = None;
        let mut update = None;
        let mut vars = Vec::new();
        for (key, value) in address.query_params() {
            match key.as_str() {
                "expr" => expr = Some(value),
                "update" => {
                    update = Some(
                        value
                            .split(',')
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    );
                }
                "name" => {}
                _ => vars.push((key, value)),
            }
        }
        let expr = expr.filter(|e| !e.is_empty())?;
        if vars.is_empty() {
            return None;
        }
        Some(Self { expr, vars, update })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_expr_vars_and_update() {
        let addr = PvAddress::parse("calc://sum?expr=a+b&a=loc://x&b=loc://y&update=a");
        let config = CalcConfig::parse(&addr).expect("valid calc config");
        assert_eq!(config.expr, "a+b");
        assert_eq!(
            config.vars,
            vec![
                ("a".to_owned(), "loc://x".to_owned()),
                ("b".to_owned(), "loc://y".to_owned()),
            ]
        );
        assert_eq!(config.update, Some(vec!["a".to_owned()]));
    }

    #[test]
    fn config_without_update_is_none() {
        let addr = PvAddress::parse("calc://v?expr=a*2&a=loc://x");
        let config = CalcConfig::parse(&addr).expect("valid calc config");
        assert_eq!(config.update, None);
    }

    #[test]
    fn config_keeps_child_address_with_embedded_query() {
        // A child's own `?query` is preserved in the variable value (only the
        // calc query's first `?` is consumed by address parsing).
        let addr = PvAddress::parse("calc://v?expr=a&a=loc://x?init=2");
        let config = CalcConfig::parse(&addr).expect("valid calc config");
        assert_eq!(
            config.vars,
            vec![("a".to_owned(), "loc://x?init=2".to_owned())]
        );
    }

    #[test]
    fn config_rejects_missing_expr_or_vars() {
        // No expr.
        assert_eq!(
            CalcConfig::parse(&PvAddress::parse("calc://v?a=loc://x")),
            None
        );
        // Empty expr.
        assert_eq!(
            CalcConfig::parse(&PvAddress::parse("calc://v?expr=&a=loc://x")),
            None
        );
        // No variables.
        assert_eq!(
            CalcConfig::parse(&PvAddress::parse("calc://v?expr=1+1")),
            None
        );
    }

    #[test]
    fn config_treats_reserved_name_key_as_non_variable() {
        let addr = PvAddress::parse("calc://v?expr=a&name=ignored&a=loc://x");
        let config = CalcConfig::parse(&addr).expect("valid calc config");
        assert_eq!(config.vars, vec![("a".to_owned(), "loc://x".to_owned())]);
    }

    #[test]
    fn value_round_trips_through_evalexpr() {
        assert_eq!(pv_to_evalexpr(&PvValue::Int(3)), Some(Value::Int(3)));
        assert_eq!(
            pv_to_evalexpr(&PvValue::Float(1.5)),
            Some(Value::Float(1.5))
        );
        assert_eq!(
            pv_to_evalexpr(&PvValue::Bool(true)),
            Some(Value::Boolean(true))
        );
        assert_eq!(
            pv_to_evalexpr(&PvValue::Enum {
                index: 2,
                label: None
            }),
            Some(Value::Int(2))
        );
        // Arrays cannot be expression variables.
        assert_eq!(
            pv_to_evalexpr(&PvValue::FloatArray(Arc::from([1.0].as_slice()))),
            None
        );

        assert_eq!(evalexpr_to_pv(&Value::Int(7)), Some(PvValue::Int(7)));
        assert_eq!(
            evalexpr_to_pv(&Value::Float(2.5)),
            Some(PvValue::Float(2.5))
        );
        assert_eq!(
            evalexpr_to_pv(&Value::Boolean(false)),
            Some(PvValue::Bool(false))
        );
        assert_eq!(
            evalexpr_to_pv(&Value::String("hi".to_owned())),
            Some(PvValue::Str(Arc::from("hi")))
        );
        assert_eq!(evalexpr_to_pv(&Value::Empty), None);
    }

    /// Evaluate an expression against a fixed variable map (no live channels) —
    /// exercises the same `evalexpr` path `evaluate` uses.
    fn eval_vars(expr: &str, vars: &[(&str, Value)]) -> Option<PvValue> {
        let mut ctx = HashMapContext::new();
        for (name, value) in vars {
            ctx.set_value((*name).to_owned(), value.clone()).ok()?;
        }
        evalexpr_to_pv(&eval_with_context(expr, &ctx).ok()?)
    }

    #[test]
    fn arithmetic_expression_evaluates() {
        assert_eq!(
            eval_vars("a + b * 2", &[("a", Value::Int(1)), ("b", Value::Int(3))]),
            Some(PvValue::Int(7))
        );
    }

    #[test]
    fn comparison_yields_boolean() {
        assert_eq!(
            eval_vars(
                "a > b",
                &[("a", Value::Float(5.0)), ("b", Value::Float(2.0))]
            ),
            Some(PvValue::Bool(true))
        );
    }

    #[test]
    fn builtin_math_function_is_available() {
        assert_eq!(
            eval_vars("math::sqrt a", &[("a", Value::Float(9.0))]),
            Some(PvValue::Float(3.0))
        );
    }

    #[test]
    fn missing_variable_fails_and_skips() {
        // `b` is undefined → eval errors → skip (None), not a panic.
        assert_eq!(eval_vars("a + b", &[("a", Value::Int(1))]), None);
    }
}
