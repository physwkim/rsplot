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
//! built-in `math::*` functions), in the PyDM calc vocabulary
//! ([`pydm_calc_context`]): every bare `math` name PyDM injects (`sin(A)`,
//! `pi`, `floor(B)`, …) plus `epics_string` / `epics_unsigned`
//! (`CalcThread.eval_env`, `calc_plugin.py:50-53`; the gaps — `np`, dotted
//! `math.` spellings, Python builtins — are enumerated on that function). Only
//! **scalar** children participate — a child with **no value yet** silently
//! defers evaluation (PyDM waits until every child has a value), but a child
//! that *is* connected with a **non-scalar (waveform) value** cannot bind (PyDM
//! evaluates array children through its ndarray vocabulary; sidm's calc value
//! model is scalar-only) and so **warns once**, then skips — fail-visible per
//! the R2-59 contract, not the silent permanent dead channel a bare skip would
//! leave. A [`PvValue::Bytes`] char waveform is the one exception that binds, as
//! its NUL-terminated string (the `epics_string` transform). An expression that
//! fails to evaluate likewise publishes nothing and **warns once** per
//! connection (PyDM `logger.exception`s every failure, `calc_plugin.py:174-179`;
//! sidm's 50 ms poll would repeat the message indefinitely, so it logs once).
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
use evalexpr::{
    ContextWithMutableFunctions, ContextWithMutableVariables, EvalexprError, Function,
    HashMapContext, Value, eval_with_context,
};
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
        Evaluator::Evalexpr(expr) => evaluate_evalexpr(id, expr, children, prev, warned_eval),
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

/// Evaluate `expr` against the children's current scalar values plus `prev_res`,
/// in the PyDM calc vocabulary ([`pydm_calc_context`]). Returns `None` (skip) in
/// three cases, two silent and one fail-visible:
///
/// - a child has **no value yet** — a silent skip, matching PyDM's "skip until
///   all values set" (`calc_plugin.py:170-172`); it resolves once the child
///   delivers a value;
/// - a child has a **non-scalar (waveform) value** that sidm's scalar-only calc
///   cannot bind — **warn-once**, then skip. PyDM evaluates array children; sidm
///   does not, so R2-59's fail-visible contract turns what was a permanently
///   silent dead channel (the old bare `?`) into a logged skip;
/// - the **expression itself fails** — **warn-once**, matching PyDM's
///   `logger.exception` on an `eval` failure (`calc_plugin.py:174-179`).
///
/// PyDM logs every failure; sidm logs once per connection (`warned_eval`) —
/// the 50 ms poll would otherwise repeat the same message indefinitely.
fn evaluate_evalexpr(
    id: &str,
    expr: &str,
    children: &[(String, Channel)],
    prev: Option<&PvValue>,
    warned_eval: &mut bool,
) -> Option<PvValue> {
    let mut ctx = pydm_calc_context();
    for (name, ch) in children {
        let value = ch.read(|s| s.value.clone());
        let Some(value) = value else {
            // No value yet — a legitimate skip (PyDM waits until every child has
            // a value, calc_plugin.py:170-172). Silent: it resolves once the
            // child delivers a value.
            return None;
        };
        let Some(var) = pv_to_evalexpr(&value) else {
            // Value present but not scalar-bindable — a waveform child
            // (FloatArray/IntArray/StrArray; `pv_to_evalexpr` returns `None`).
            // sidm's calc value model is scalar-only (a documented scope
            // decision), so it cannot bind; PyDM would evaluate it through its
            // ndarray vocabulary. Fail-visible per the R2-59 contract — warn
            // once, then skip — instead of the bare `?`'s permanently silent
            // dead channel that re-skipped every 50 ms poll.
            if !*warned_eval {
                *warned_eval = true;
                log::warn!(
                    "{id}: calc variable {name:?} has a non-scalar (waveform) value; \
                     sidm's calc is scalar-only so it cannot bind — publishing nothing. \
                     PyDM evaluates array children; full array binding is not ported."
                );
            }
            return None;
        };
        if let Err(err) = ctx.set_value(name.clone(), var) {
            if !*warned_eval {
                *warned_eval = true;
                log::warn!("{id}: cannot bind calc variable {name:?} ({err}); publishing nothing");
            }
            return None;
        }
    }
    if let Some(prev_var) = prev.and_then(pv_to_evalexpr) {
        // Best-effort: a missing `prev_res` only matters if the expression uses
        // it, in which case the eval below fails and we skip — same as PyDM.
        let _ = ctx.set_value("prev_res".to_owned(), prev_var);
    }
    match eval_with_context(expr, &ctx) {
        Ok(value) => evalexpr_to_pv(&value),
        Err(err) => {
            if !*warned_eval {
                *warned_eval = true;
                log::warn!("{id}: calc expression failed ({err}); publishing nothing");
            }
            None
        }
    }
}

/// The PyDM calc evaluation vocabulary (`CalcThread.eval_env`,
/// `calc_plugin.py:50-53`): every non-underscore name of Python's `math` module
/// injected **bare** (`sin(A)`, `pi`, `floor(B)` — no `math.` prefix needed),
/// plus the two EPICS helpers `epics_string` / `epics_unsigned`. evalexpr's own
/// builtins (`min`, `max`, `len`, `math::*` under the `::` spelling) remain
/// available alongside.
///
/// Deliberate gaps, all *visible* (an unknown name is an eval error, which
/// [`evaluate_evalexpr`] now logs): the `np`/`numpy` namespaces and the dotted
/// `math.sin` spelling (evalexpr has no attribute syntax; sidm's value model is
/// scalar, so the array vocabulary has nothing to operate on); Python's
/// implicit `__builtins__` (`abs`, `round`, `int`, `str`, …) beyond what
/// evalexpr provides; the tuple-returning `frexp`/`modf`; the iterable-consuming
/// `fsum`/`prod`/`dist`/`sumprod`/`isqrt`; the integer-combinatoric
/// `factorial`/`comb`/`perm`/`gcd`/`lcm`; and `gamma`/`lgamma`/`nextafter`/
/// `ulp`/`remainder` (no std implementation; `erf`/`erfc` are covered by
/// siplot's SunPro port).
fn pydm_calc_context() -> HashMapContext {
    /// A bare math-vocabulary entry: Python name -> f64 implementation.
    type Unary = (&'static str, fn(f64) -> f64);
    type Binary = (&'static str, fn(f64, f64) -> f64);
    type Predicate = (&'static str, fn(f64) -> bool);

    let mut ctx = HashMapContext::new();

    // Constants (math.pi, math.e, math.tau, math.inf, math.nan).
    for (name, value) in [
        ("pi", std::f64::consts::PI),
        ("e", std::f64::consts::E),
        ("tau", std::f64::consts::TAU),
        ("inf", f64::INFINITY),
        ("nan", f64::NAN),
    ] {
        ctx.set_value(name.to_owned(), Value::Float(value))
            .expect("HashMapContext is mutable");
    }

    // 1-argument float functions.
    let unary: [Unary; 28] = [
        ("acos", f64::acos),
        ("acosh", f64::acosh),
        ("asin", f64::asin),
        ("asinh", f64::asinh),
        ("atan", f64::atan),
        ("atanh", f64::atanh),
        ("cbrt", f64::cbrt),
        ("ceil", f64::ceil),
        ("cos", f64::cos),
        ("cosh", f64::cosh),
        ("degrees", f64::to_degrees),
        ("erf", siplot::core::fitting::erf),
        ("erfc", siplot::core::fitting::erfc),
        ("exp", f64::exp),
        ("exp2", f64::exp2),
        ("expm1", f64::exp_m1),
        ("fabs", f64::abs),
        ("floor", f64::floor),
        ("log10", f64::log10),
        ("log1p", f64::ln_1p),
        ("log2", f64::log2),
        ("radians", f64::to_radians),
        ("sin", f64::sin),
        ("sinh", f64::sinh),
        ("sqrt", f64::sqrt),
        ("tan", f64::tan),
        ("tanh", f64::tanh),
        ("trunc", f64::trunc),
    ];
    for (name, f) in unary {
        ctx.set_function(
            name.to_owned(),
            Function::new(move |arg| Ok(Value::Float(f(arg.as_number()?)))),
        )
        .expect("HashMapContext is mutable");
    }

    // 2-argument float functions.
    let binary: [Binary; 5] = [
        ("atan2", f64::atan2),
        ("copysign", f64::copysign),
        ("fmod", |x, y| x % y), // C fmod: sign of x, as Python's
        ("hypot", f64::hypot),
        ("pow", f64::powf),
    ];
    for (name, f) in binary {
        ctx.set_function(
            name.to_owned(),
            Function::new(move |arg| {
                let args = arg.as_fixed_len_tuple(2)?;
                Ok(Value::Float(f(args[0].as_number()?, args[1].as_number()?)))
            }),
        )
        .expect("HashMapContext is mutable");
    }

    // log(x) = ln, log(x, base) — Python math.log's two arities.
    ctx.set_function(
        "log".to_owned(),
        Function::new(|arg| match arg {
            Value::Tuple(args) if args.len() == 2 => {
                let (x, base): (f64, f64) = (args[0].as_number()?, args[1].as_number()?);
                Ok(Value::Float(x.log(base)))
            }
            _ => {
                let x: f64 = arg.as_number()?;
                Ok(Value::Float(x.ln()))
            }
        }),
    )
    .expect("HashMapContext is mutable");

    // ldexp(x, i) = x * 2^i.
    ctx.set_function(
        "ldexp".to_owned(),
        Function::new(|arg| {
            let args = arg.as_fixed_len_tuple(2)?;
            let (x, i): (f64, f64) = (args[0].as_number()?, args[1].as_number()?);
            Ok(Value::Float(x * i.exp2()))
        }),
    )
    .expect("HashMapContext is mutable");

    // Predicates → Boolean, as Python's.
    let predicates: [Predicate; 3] = [
        ("isnan", f64::is_nan),
        ("isinf", f64::is_infinite),
        ("isfinite", f64::is_finite),
    ];
    for (name, f) in predicates {
        ctx.set_function(
            name.to_owned(),
            Function::new(move |arg| Ok(Value::Boolean(f(arg.as_number()?)))),
        )
        .expect("HashMapContext is mutable");
    }

    // isclose(a, b) with Python's defaults (rel_tol=1e-9, abs_tol=0.0);
    // the keyword arguments have no evalexpr spelling.
    ctx.set_function(
        "isclose".to_owned(),
        Function::new(|arg| {
            let args = arg.as_fixed_len_tuple(2)?;
            let (a, b): (f64, f64) = (args[0].as_number()?, args[1].as_number()?);
            Ok(Value::Boolean((a - b).abs() <= 1e-9 * a.abs().max(b.abs())))
        }),
    )
    .expect("HashMapContext is mutable");

    // epics_string(value): PyDM's char-waveform→string helper
    // (calc_plugin.py:19-33). sidm already binds a Bytes child as its
    // NUL-terminated UTF-8 string (`pv_to_evalexpr`), so this is identity on
    // strings — it exists so PyDM screens spelling `epics_string(A)` work
    // unchanged. The `string_encoding` second argument is not supported
    // (sidm decodes UTF-8 lossily); passing one is a visible eval error.
    ctx.set_function(
        "epics_string".to_owned(),
        Function::new(|arg| Ok(Value::String(arg.as_string()?))),
    )
    .expect("HashMapContext is mutable");

    // epics_unsigned(value, bits=32): reinterpret a negative signed integer
    // as unsigned (calc_plugin.py:36-47).
    ctx.set_function(
        "epics_unsigned".to_owned(),
        Function::new(|arg| {
            let (value, bits) = match arg {
                Value::Tuple(args) if args.len() == 2 => {
                    (args[0].as_int()?, u32::try_from(args[1].as_int()?).ok())
                }
                _ => (arg.as_int()?, Some(32)),
            };
            let bits = bits.ok_or_else(|| {
                EvalexprError::CustomMessage("epics_unsigned: bits must be >= 0".to_owned())
            })?;
            Ok(if value >= 0 {
                Value::Int(value)
            } else if bits < 63 {
                Value::Int((1i64 << bits) + value)
            } else {
                // 2^bits overflows i64 — return the float Python's arbitrary
                // precision would print (exact for bits <= 52 either way).
                Value::Float((bits as f64).exp2() + value as f64)
            })
        }),
    )
    .expect("HashMapContext is mutable");

    ctx
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
        // A char waveform binds as its NUL-terminated UTF-8 string — the
        // `epics_string` transform (calc_plugin.py:19-33) applied at binding
        // time, since evalexpr has no byte-array value. PyDM binds the raw
        // ndarray and lets `epics_string(A)` convert; sidm's evalexpr
        // `epics_string` is therefore identity-on-string.
        PvValue::Bytes(b) => {
            let nul_terminated = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
            Value::String(String::from_utf8_lossy(nul_terminated).into_owned())
        }
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
    /// exercises the same vocabulary-bearing context `evaluate_evalexpr` uses.
    fn eval_vars(expr: &str, vars: &[(&str, Value)]) -> Option<PvValue> {
        let mut ctx = pydm_calc_context();
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
    fn bare_math_names_evaluate_like_pydm() {
        // R2-59: PyDM injects every non-underscore math.__dict__ name bare
        // (CalcThread.eval_env, calc_plugin.py:50-53) — no `math.` prefix.
        assert_eq!(
            eval_vars("sin(a)", &[("a", Value::Float(0.0))]),
            Some(PvValue::Float(0.0))
        );
        assert_eq!(eval_vars("cos(pi)", &[]), Some(PvValue::Float(-1.0)));
        assert_eq!(
            eval_vars("floor(a) + ceil(a)", &[("a", Value::Float(1.5))]),
            Some(PvValue::Float(3.0))
        );
        assert_eq!(eval_vars("log(e)", &[]), Some(PvValue::Float(1.0)));
        // Two-arity log(x, base), atan2, and a predicate.
        assert_eq!(eval_vars("log(8, 2)", &[]), Some(PvValue::Float(3.0)));
        assert_eq!(eval_vars("atan2(0, 1)", &[]), Some(PvValue::Float(0.0)));
        assert_eq!(
            eval_vars("isnan(a)", &[("a", Value::Float(f64::NAN))]),
            Some(PvValue::Bool(true))
        );
        // Int arguments coerce (as_number), as Python's math does.
        assert_eq!(
            eval_vars("sqrt(a)", &[("a", Value::Int(9))]),
            Some(PvValue::Float(3.0))
        );
        assert_eq!(eval_vars("erf(0)", &[]), Some(PvValue::Float(0.0)));
    }

    #[test]
    fn epics_unsigned_reinterprets_negative_ints() {
        // calc_plugin.py:36-47: 2**bits + value for negative values.
        assert_eq!(
            eval_vars("epics_unsigned(a)", &[("a", Value::Int(-1))]),
            Some(PvValue::Int(4294967295)) // default bits=32
        );
        assert_eq!(
            eval_vars("epics_unsigned(a, 16)", &[("a", Value::Int(-1))]),
            Some(PvValue::Int(65535))
        );
        // Non-negative values pass through.
        assert_eq!(
            eval_vars("epics_unsigned(a)", &[("a", Value::Int(7))]),
            Some(PvValue::Int(7))
        );
    }

    #[test]
    fn bytes_child_binds_as_nul_terminated_string_for_epics_string() {
        // A CHAR waveform binds as its NUL-terminated string, so PyDM screens
        // spelling `epics_string(A)` work (calc_plugin.py:19-33).
        let bound = pv_to_evalexpr(&PvValue::Bytes(Arc::from([b'h', b'i', 0, b'x'].as_slice())));
        assert_eq!(bound, Some(Value::String("hi".to_owned())));
        assert_eq!(
            eval_vars("epics_string(a)", &[("a", Value::String("hi".to_owned()))]),
            Some(PvValue::Str(Arc::from("hi")))
        );
        // No NUL: the whole buffer decodes.
        assert_eq!(
            pv_to_evalexpr(&PvValue::Bytes(Arc::from([b'o', b'k'].as_slice()))),
            Some(Value::String("ok".to_owned()))
        );
    }

    #[test]
    fn eval_failure_warns_once_and_publishes_nothing() {
        // R2-59's silent half: an unknown name must return None AND flip the
        // warn-once flag (the log call is behind it), not `.ok()?` silently.
        let mut warned = false;
        assert_eq!(
            evaluate_evalexpr("calc://t", "no_such_fn(1)", &[], None, &mut warned),
            None
        );
        assert!(warned, "eval failure must trip the warn-once flag");

        // A successful expression must NOT trip it.
        let mut warned = false;
        assert_eq!(
            evaluate_evalexpr("calc://t", "1 + 1", &[], None, &mut warned),
            Some(PvValue::Int(2))
        );
        assert!(!warned);
    }

    /// A child channel whose state the caller sets directly (value-less until
    /// posted). Dangling pool weak → the `Drop` prune is a no-op.
    fn child_channel() -> (Channel, StateWriter) {
        let (conn, writer, _writes, _cancel) = crate::channel::Connection::new(
            PvAddress::parse("loc://calc_child"),
            crate::channel::RepaintHook::default(),
            std::sync::Weak::new(),
            "loc://calc_child".to_owned(),
        );
        (Channel::new(conn), writer)
    }

    #[test]
    fn array_child_warns_once_while_missing_value_stays_silent() {
        // R3-13: distinguish "no value yet" (silent skip) from "value present
        // but unbindable" (fail-visible warn-once). The old bare `?` conflated
        // both into a permanently silent dead channel.

        // No value yet → silent skip: PyDM waits until every child has a value.
        let (absent, _w) = child_channel();
        let mut warned = false;
        assert_eq!(
            evaluate_evalexpr(
                "calc://t",
                "A + 1.0",
                &[("A".to_owned(), absent)],
                None,
                &mut warned,
            ),
            None
        );
        assert!(!warned, "a value-less child must skip silently");

        // Waveform value → the scalar calc cannot bind it: fail-visible.
        let (arr, arr_w) = child_channel();
        arr_w.post_value(|s| {
            s.connected = true;
            s.value = Some(PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0].as_slice())));
        });
        let mut warned = false;
        assert_eq!(
            evaluate_evalexpr(
                "calc://t",
                "A + 1.0",
                &[("A".to_owned(), arr)],
                None,
                &mut warned,
            ),
            None
        );
        assert!(
            warned,
            "an unbindable waveform child must trip the warn-once flag"
        );

        // Scalar value → binds and evaluates as before (unchanged).
        let (scalar, scalar_w) = child_channel();
        scalar_w.post_value(|s| {
            s.connected = true;
            s.value = Some(PvValue::Float(4.0));
        });
        let mut warned = false;
        assert_eq!(
            evaluate_evalexpr(
                "calc://t",
                "A + 1.0",
                &[("A".to_owned(), scalar)],
                None,
                &mut warned,
            ),
            Some(PvValue::Float(5.0))
        );
        assert!(!warned, "a bound scalar child must not warn");
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
