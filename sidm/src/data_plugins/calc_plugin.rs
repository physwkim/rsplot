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
//! # The MEDM CALC dialect (`?dialect=medm`)
//!
//! A second, explicitly selected dialect evaluates the expression as a **MEDM
//! CALC** (EPICS calcRecord) expression instead of an `evalexpr` one:
//!
//! ```text
//! calc://name?dialect=medm&expr=A%230&A=<child-addr>&update=A
//! ```
//!
//! It exists for `adl2sidm`-converted MEDM `dynamic attribute` visibility rules,
//! whose grammar (`medm/medmCalc.c`) — `=`/`#` equality, `**`, ternary `?:`,
//! `ABS`..`NOT` functions, `AND`/`OR`/`XOR` keywords, `PI`/`D2R`/`R2D`, `RNDM`,
//! operands `A`–`L` in both cases — is evaluated double-typed throughout
//! (`calcPerform(valueArray…)`, `medm/utils.c:4486-4508`). Translating that onto
//! `evalexpr` is lossy: evalexpr's `==`/`!=` are type-strict
//! (`Value::Float(0.0) != Value::Int(0)`), and most of the operator surface has
//! no evalexpr spelling. The dialect therefore reuses the EPICS libCom calc
//! engine ported by [`epics_base_rs::calc`] — the same grammar MEDM's
//! `medmCalc.c` embeds (a superset: libCom also has `<<`, `>?`, `ISNAN`, …).
//!
//! Dialect specifics, all matching MEDM (`medm/utils.c` `calcVisibility`):
//!
//! - Each variable is a single letter `A`–`U` (either case) naming the calc
//!   operand it binds; a connected child binds its scalar value, and a child
//!   with no/non-numeric value binds `0.0` (MEDM `Record.value` is a double
//!   initialised to 0.0 — `utils.c:4491-4496`).
//! - Operands `E`–`L` not bound to an explicit child are record metadata of the
//!   **first** channel (operand `A`, MEDM `records[0]`; `utils.c:4498-4505`):
//!   `E`,`F` = 0, `G` = element count, `H` = hopr, `I` = alarm status,
//!   `J` = severity, `K` = precision, `L` = lopr. sidm's `ChannelState` carries
//!   no EPICS alarm-status code, so `I` binds `0.0` (documented gap).
//! - The `expr` query value is **percent-decoded** (`%26` → `&`, `%25` → `%`),
//!   because the raw query splits on `&`; `adl2sidm` encodes exactly those two
//!   bytes. The plain (PyDM) dialect stays raw — PyDM does not decode either.
//! - **Fail-visible:** an expression that does not compile, a variable that is
//!   not a single `A`–`U` letter, or an evaluation error publishes `1.0` and
//!   warns once, so a visibility gate leaves its widget SHOWN. Deliberate
//!   deviation from MEDM, which *hides* on an invalid calc
//!   (`utils.c:4484-4531`: `validCalc == False` and `calcPerform` failure both
//!   return `False`) — an operator screen that silently hides controls is the
//!   worse failure, and this matches the converter's established warn-and-stay-
//!   visible posture for untranslatable rules.
//! - The result is always a [`PvValue::Float`] (the engine is double-typed).
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

use epics_base_rs::calc as medm_calc;
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
                "calc:// requires ?expr=… and at least one variable=child-address \
                 (and `dialect`, when given, must be `medm`)"
                    .to_owned(),
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
            address.raw().to_owned(),
            config,
            children,
            writer,
            writes,
            cancel,
        ));
        Ok(())
    }
}

/// The compiled per-connection evaluator — the dialect decision and (for MEDM)
/// the expression compilation happen exactly once, at task start, so the poll
/// loop cannot re-decide them.
enum Evaluator {
    /// PyDM dialect: [`evalexpr`] re-parses the expression per evaluation (its
    /// API shape; the expressions are tiny).
    Evalexpr(String),
    /// MEDM CALC dialect: the postfix-compiled expression plus each child's
    /// operand index (`A` = 0 … `U` = 20, parallel to the children vec).
    Medm {
        compiled: medm_calc::CompiledExpr,
        operand_indices: Vec<usize>,
    },
    /// MEDM dialect whose expression failed to compile (or a variable is not a
    /// single `A`–`U` letter): fail-visible — `1.0` was published at task
    /// start, and no evaluation ever runs.
    Invalid,
}

impl Evaluator {
    /// Build the evaluator for `config`, warning once and falling back to
    /// [`Evaluator::Invalid`] when the MEDM expression/variables are unusable.
    fn build(id: &str, config: &CalcConfig) -> Self {
        match config.dialect {
            Dialect::Evalexpr => Self::Evalexpr(config.expr.clone()),
            Dialect::Medm => {
                let mut operand_indices = Vec::with_capacity(config.vars.len());
                for (name, _) in &config.vars {
                    let Some(idx) = medm_var_index(name) else {
                        log::warn!(
                            "{id}: MEDM CALC variable {name:?} is not a single A–U letter; \
                             leaving the channel at 1.0 (fail-visible)"
                        );
                        return Self::Invalid;
                    };
                    operand_indices.push(idx);
                }
                match medm_calc::compile(&config.expr) {
                    Ok(compiled) => Self::Medm {
                        compiled,
                        operand_indices,
                    },
                    Err(err) => {
                        log::warn!(
                            "{id}: MEDM CALC expression {:?} does not compile ({err:?}); \
                             leaving the channel at 1.0 (fail-visible)",
                            config.expr
                        );
                        Self::Invalid
                    }
                }
            }
        }
    }
}

/// Service one `calc://` connection: poll children, recompute, publish.
async fn run_channel(
    id: String,
    config: CalcConfig,
    children: Vec<(String, Channel)>,
    writer: StateWriter,
    mut writes: mpsc::UnboundedReceiver<PvValue>,
    cancel: CancellationToken,
) {
    let update = config.update.clone();
    let evaluator = Evaluator::build(&id, &config);
    if matches!(evaluator, Evaluator::Invalid) {
        // Fail-visible: publish 1.0 immediately so a visibility gate shows its
        // widget. (MEDM itself *hides* on an invalid calc — utils.c:4484-4531 —
        // see the module docs for why this deviates.)
        writer.post_value(|s| {
            s.connected = true;
            s.value = Some(PvValue::Float(1.0));
        });
    }
    // Warn at most once per connection when evaluation errors (fail-visible).
    let mut warned_eval = false;

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
                    && let Some(value) = evaluate(
                        &id,
                        &evaluator,
                        &children,
                        prev_value.as_ref(),
                        &mut warned_eval,
                    )
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

/// Evaluate one trigger under the connection's dialect. `None` means "publish
/// nothing" (evalexpr skip, or an invalid MEDM expression that already
/// published its fail-visible 1.0 at task start).
fn evaluate(
    id: &str,
    evaluator: &Evaluator,
    children: &[(String, Channel)],
    prev: Option<&PvValue>,
    warned_eval: &mut bool,
) -> Option<PvValue> {
    match evaluator {
        Evaluator::Evalexpr(expr) => evaluate_evalexpr(expr, children, prev),
        Evaluator::Medm {
            compiled,
            operand_indices,
        } => Some(match eval_medm(compiled, children, operand_indices, prev) {
            Ok(result) => PvValue::Float(result),
            Err(err) => {
                // Fail-visible on evaluation errors too, warning once (MEDM
                // hides here — utils.c:4519-4523; see the module docs).
                if !*warned_eval {
                    *warned_eval = true;
                    log::warn!(
                        "{id}: MEDM CALC evaluation failed ({err:?}); \
                         publishing 1.0 (fail-visible)"
                    );
                }
                PvValue::Float(1.0)
            }
        }),
        Evaluator::Invalid => None,
    }
}

/// Evaluate `expr` against the children's current scalar values plus `prev_res`.
/// Returns `None` (skip) when any variable lacks a scalar value or the
/// expression fails to evaluate — matching PyDM's "skip until all values set".
fn evaluate_evalexpr(
    expr: &str,
    children: &[(String, Channel)],
    prev: Option<&PvValue>,
) -> Option<PvValue> {
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

/// Evaluate a compiled MEDM CALC expression over the children (MEDM
/// `calcVisibility`, `medm/utils.c:4484-4517`).
///
/// Operand binding, matching the C:
/// - Each child binds its operand (`utils.c:4491-4496`): the scalar value when
///   it has one, else `0.0` (MEDM's `Record.value` is a double initialised to
///   0.0; strings/arrays have no scalar there).
/// - Operands `E`(4)–`L`(11) *not* bound to an explicit child are record
///   metadata of the first channel — operand `A` when bound, else the first
///   child in address order (MEDM `records[0]`; `utils.c:4498-4505`):
///   `E`,`F` reserved 0, `G` = element count, `H` = hopr, `I` = alarm status,
///   `J` = severity, `K` = precision, `L` = lopr. sidm's [`ChannelState`]
///   carries no EPICS alarm-status code (only the severity), so `I` stays
///   `0.0` — a documented binding gap.
/// - `prev_val` (the `VAL` token) is the previous published result.
///
/// [`ChannelState`]: crate::channel::ChannelState
fn eval_medm(
    compiled: &medm_calc::CompiledExpr,
    children: &[(String, Channel)],
    operand_indices: &[usize],
    prev: Option<&PvValue>,
) -> Result<f64, medm_calc::CalcError> {
    let mut inputs = medm_calc::NumericInputs::new();
    let mut child_bound = [false; medm_calc::CALC_NARGS];
    for ((_, ch), &idx) in children.iter().zip(operand_indices) {
        inputs.vars[idx] = ch
            .read(|s| s.value.as_ref().and_then(PvValue::as_f64))
            .unwrap_or(0.0);
        child_bound[idx] = true;
    }

    // The "first channel" whose metadata backs E–L: operand A when a child
    // binds it, else the first listed child (MEDM requires channel A for a
    // valid calc — utils.c:4543 — so A is the normal case).
    let first = operand_indices
        .iter()
        .position(|&idx| idx == 0)
        .or(if children.is_empty() { None } else { Some(0) })
        .map(|i| &children[i].1);
    if let Some(ch) = first {
        ch.read(|s| {
            if !child_bound[6] {
                // G: element count (scalar = 1; no value yet = 0, as an MEDM
                // Record's elementCount starts 0 before the first get).
                inputs.vars[6] = s.value.as_ref().map(|v| v.len() as f64).unwrap_or(0.0);
            }
            if !child_bound[7] {
                inputs.vars[7] = s.display_limits.map(|(_, hopr)| hopr).unwrap_or(0.0);
            }
            // I (8): EPICS alarm STATUS — not carried by ChannelState; 0.0.
            if !child_bound[9] {
                inputs.vars[9] = f64::from(s.severity.as_code());
            }
            if !child_bound[10] {
                inputs.vars[10] = f64::from(s.precision.unwrap_or(0));
            }
            if !child_bound[11] {
                inputs.vars[11] = s.display_limits.map(|(lopr, _)| lopr).unwrap_or(0.0);
            }
        });
    }

    inputs.prev_val = prev.and_then(PvValue::as_f64).unwrap_or(0.0);
    medm_calc::eval(compiled, &mut inputs)
}

/// The calc-operand index for an MEDM-dialect variable name: a single letter
/// `A`–`U` in either case (`medm/medmCalc.c:212-236` accepts both cases;
/// `A`–`U` is the engine's [`medm_calc::CALC_NARGS`] operand range, a superset
/// of MEDM's `A`–`L`). Anything else is not a valid operand.
fn medm_var_index(name: &str) -> Option<usize> {
    let mut chars = name.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let upper = first.to_ascii_uppercase();
    upper
        .is_ascii_uppercase()
        .then_some((upper as usize).wrapping_sub('A' as usize))
        .filter(|&idx| idx < medm_calc::CALC_NARGS)
}

/// Decode `%XX` percent-escapes (the MEDM-dialect `expr` transport encoding);
/// an invalid escape passes through literally. `adl2sidm` encodes only `%` and
/// `&` — the two bytes the raw `calc://` query cannot carry.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Which expression language a `calc://` connection evaluates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Dialect {
    /// PyDM-style [`evalexpr`] expression (the default; PyDM parity).
    #[default]
    Evalexpr,
    /// MEDM CALC expression via [`epics_base_rs::calc`] (`?dialect=medm`).
    Medm,
}

/// The parsed `calc://` configuration (PyDM `UrlToPython` + `CalcThread` config).
#[derive(Debug, PartialEq)]
struct CalcConfig {
    /// The expression to evaluate (percent-decoded for the MEDM dialect).
    expr: String,
    /// Which language `expr` is written in (`dialect` query key).
    dialect: Dialect,
    /// Variable name → child channel address, in URL order.
    vars: Vec<(String, String)>,
    /// Variables whose update triggers a recompute; `None` = any variable
    /// (PyDM's `update` query key, omitted → recompute on every value).
    update: Option<Vec<String>>,
}

impl CalcConfig {
    /// Parse the query into expression + dialect + variables + update list.
    /// Returns `None` when there is no non-empty `expr`, no variables, or an
    /// unknown `dialect` value — an unusable config.
    fn parse(address: &PvAddress) -> Option<Self> {
        // Reserved keys that are not variables: PyDM's RESERVED_FIELD (`expr`,
        // `update`, `name`) plus the sidm dialect selector.
        let mut expr = None;
        let mut dialect = Dialect::default();
        let mut update = None;
        let mut vars = Vec::new();
        for (key, value) in address.query_params() {
            match key.as_str() {
                "expr" => expr = Some(value),
                "dialect" => match value.as_str() {
                    "medm" => dialect = Dialect::Medm,
                    // Empty = the default; anything else is an unknown
                    // language — refuse rather than mis-evaluate.
                    "" => {}
                    _ => return None,
                },
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
        let mut expr = expr.filter(|e| !e.is_empty())?;
        if dialect == Dialect::Medm {
            // The MEDM transport contract percent-encodes `%`/`&` in `expr`
            // (the raw query splits on `&`); decode here, once.
            expr = percent_decode(&expr);
        }
        if vars.is_empty() {
            return None;
        }
        Some(Self {
            expr,
            dialect,
            vars,
            update,
        })
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

    #[test]
    fn config_parses_medm_dialect_and_percent_decodes_expr() {
        // `%26%26` → `&&` (the transport encoding for the query splitter).
        let addr =
            PvAddress::parse("calc://v?dialect=medm&expr=A%230%26%26B%230&A=loc://x&B=loc://y");
        let config = CalcConfig::parse(&addr).expect("valid medm config");
        assert_eq!(config.dialect, Dialect::Medm);
        assert_eq!(config.expr, "A#0&&B#0");
    }

    #[test]
    fn config_defaults_to_evalexpr_dialect_and_leaves_expr_raw() {
        // No `dialect` key → PyDM dialect, and NO percent-decoding (PyDM does
        // not decode; `a%25` must stay a modulo-by-25 expression).
        let addr = PvAddress::parse("calc://v?expr=a%25&a=loc://x");
        let config = CalcConfig::parse(&addr).expect("valid config");
        assert_eq!(config.dialect, Dialect::Evalexpr);
        assert_eq!(config.expr, "a%25");
    }

    #[test]
    fn config_rejects_unknown_dialect() {
        assert_eq!(
            CalcConfig::parse(&PvAddress::parse(
                "calc://v?dialect=python&expr=a&a=loc://x"
            )),
            None
        );
    }

    #[test]
    fn percent_decode_handles_escapes_and_passes_invalid_through() {
        assert_eq!(percent_decode("A%230"), "A#0");
        assert_eq!(percent_decode("%26%25"), "&%");
        // Invalid/truncated escapes pass through literally.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("A%ZZB"), "A%ZZB");
        assert_eq!(percent_decode("A%2"), "A%2");
    }

    #[test]
    fn medm_var_index_accepts_single_letters_in_both_cases() {
        assert_eq!(medm_var_index("A"), Some(0));
        assert_eq!(medm_var_index("a"), Some(0));
        assert_eq!(medm_var_index("D"), Some(3));
        assert_eq!(medm_var_index("l"), Some(11));
        assert_eq!(medm_var_index("U"), Some(20));
        // Beyond the operand range, multi-char, or non-letters are invalid.
        assert_eq!(medm_var_index("V"), None);
        assert_eq!(medm_var_index("AA"), None);
        assert_eq!(medm_var_index("1"), None);
        assert_eq!(medm_var_index(""), None);
    }
}
