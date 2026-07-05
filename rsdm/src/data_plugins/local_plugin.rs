//! `loc://` — in-process variables.
//!
//! Ports `pydm/data_plugins/local_plugin.py`. A local variable's type and
//! initial value come from the address query parameters
//! (`loc://name?type=float&init=1.5&precision=3`); writes replace the value and
//! echo to every listener. Because the engine pools connections by
//! `scheme://full_address` (query dropped), all `loc://name?...` addresses with
//! the same `name` share one connection.
//!
//! Configuration is *config-bearing*-gated, not first-connection-wins: PyDM
//! requires `name`+`type`+`init` (`_required_config_keys`, local_plugin.py:26)
//! before a variable is connected. A bare `loc://name` reader, or a partial
//! address missing `type` or `init`, connects **disconnected with no value** —
//! never a fabricated `0.0` — and stays that way until the first config-bearing
//! address arrives, *regardless of connect order*: PyDM re-runs
//! `_configure_local_plugin` on every `add_listener` (:333-335), and rsdm
//! mirrors that by forwarding each later listener's address to the connection
//! task (see [`crate::data_plugins::ConnectionCtx::listeners`]). So a screen
//! where one widget declares `loc://x?type=int&init=5` and others reference
//! bare `loc://x` configures correctly whichever widget the engine reaches
//! first. An unparsable `init` connects with a `None` value, not a type-zero
//! (`convert_value` → `None`, :318-323).
//!
//! Supported `type`s: `float` (default), `int`, `bool`, `str`, `array` (a
//! bracketed list — all-integer elements make an Int waveform, any float
//! element promotes it to Float, like numpy dtype unification under PyDM's
//! `np.array(ast.literal_eval(init))`, local_plugin.py:32 + :321-323). A numpy
//! `dtype` kwarg (`type=array&init=[1,2]&dtype=float`, PyDM
//! `_extra_numpy_config_keys`, local_plugin.py:30 + :257-288) overrides that
//! inference — `dtype=float` yields a Float waveform, `dtype=int` an Int one.
//!
//! Extras (`parse_channel_extras`, local_plugin.py:103-121): `precision`,
//! `unit`, `upper_limit`/`lower_limit` (numeric types only → ctrl limits),
//! `enum_string` (a `('A','B')` literal → enum strings). A float variable
//! without an explicit `precision` derives it from the value's decimal digits
//! (capped at 8) on the initial value and on every float write
//! (local_plugin.py:341-345, :377-382, :384-388).

use std::sync::Arc;
use std::time::SystemTime;

use crate::channel::{AlarmSeverity, ChannelState, PvValue};
use crate::data_plugins::{ConnectionCtx, DataPlugin};
use crate::engine::EngineError;

/// The `loc://` data plugin.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalPlugin;

impl DataPlugin for LocalPlugin {
    fn protocol(&self) -> &'static str {
        "loc"
    }

    fn connect(&self, ctx: ConnectionCtx) -> Result<(), EngineError> {
        let ConnectionCtx {
            writer,
            mut writes,
            mut listeners,
            cancel,
            runtime,
            address,
        } = ctx;

        // PyDM configures a local variable only from a *config-bearing* address
        // — one carrying both `type` and `init` (`_required_config_keys`,
        // local_plugin.py:26). A bare `loc://name` reader (or a partial address
        // missing either key) leaves the connection **disconnected with no
        // value** — never a fabricated `0.0` — until a config-bearing listener
        // arrives (`_configure_local_plugin` returns early with
        // `send_connection_state(False)`, :47-61). `_precision_set` tracks
        // whether an explicit precision was configured, so float writes know
        // whether to re-derive it (:103-109, :377-382).
        let params = address.query_params();
        let mut precision_set = has_explicit_precision(&params);
        // `done_configuring` is true once we have configured (from the creating
        // address or a later listener), or once no more listeners can arrive —
        // either way we stop watching the listener stream.
        let mut done_configuring;
        if is_config_bearing(&params) {
            // post_value emits the initial value as the first sample (or
            // nothing, when an unparsable `init` left it `None`).
            let init = initial_local_state(&params);
            writer.post_value(move |s| {
                *s = init;
                s.timestamp = Some(SystemTime::now());
            });
            done_configuring = true;
        } else {
            // Metadata/connection-only change (no value) → `update`, so no
            // spurious sample is emitted for the disconnected state.
            writer.update(|s| {
                *s = disconnected_local_state();
                s.timestamp = Some(SystemTime::now());
            });
            done_configuring = false;
        }

        runtime.spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    // Defer to the first config-bearing listener regardless of
                    // connect order (PyDM's per-`add_listener`
                    // `_configure_local_plugin`). Disabled once configured — the
                    // configuration is applied exactly once.
                    maybe = listeners.recv(), if !done_configuring => match maybe {
                        Some(addr) => {
                            let p = addr.query_params();
                            if is_config_bearing(&p) {
                                precision_set = has_explicit_precision(&p);
                                let init = initial_local_state(&p);
                                writer.post_value(move |s| {
                                    *s = init;
                                    s.timestamp = Some(SystemTime::now());
                                });
                                done_configuring = true;
                            }
                        }
                        // No more listeners can arrive; stop watching so the
                        // disabled branch does not busy-loop on a closed channel.
                        None => done_configuring = true,
                    },
                    maybe = writes.recv() => match maybe {
                        Some(value) => writer.post_value(|s| {
                            // PyDM re-derives the precision on every float
                            // write when none was configured
                            // (put_value, local_plugin.py:377-382). A write
                            // updates the value but does not itself connect the
                            // variable (put_value never touches the connection
                            // state), matching PyDM.
                            if !precision_set && let PvValue::Float(v) = &value {
                                s.precision = Some(precision_for_value(*v));
                            }
                            s.value = Some(value);
                            s.timestamp = Some(SystemTime::now());
                        }),
                        None => break, // all Channels dropped
                    },
                }
            }
        });

        Ok(())
    }
}

/// Whether a `loc://` address carries a complete configuration — a non-empty
/// `type` **and** a non-empty `init` query parameter. PyDM configures a local
/// variable only when `name`, `type` and `init` are all present
/// (`_required_config_keys`, local_plugin.py:26); `UrlToPython.get_info`
/// (:421-438) returns `(None, name, address)` — the bare/partial case — when
/// either is missing. `name` is the `loc://name` host, always present for a
/// valid address, so the predicate reduces to the two query keys. Empty values
/// (`type=`) do not count: PyDM's `parse_qs` drops blank values, so
/// `config["type"]`/`config["init"]` would raise `KeyError` there too.
fn is_config_bearing(params: &[(String, String)]) -> bool {
    let has = |k: &str| params.iter().any(|(key, val)| key == k && !val.is_empty());
    has("type") && has("init")
}

/// Whether an explicit `precision`/`prec` that parses to an integer was
/// configured — PyDM's `_precision_set` (local_plugin.py:103-109). When it was,
/// float writes must not re-derive the precision from the value.
fn has_explicit_precision(params: &[(String, String)]) -> bool {
    params
        .iter()
        .any(|(k, v)| matches!(k.as_str(), "precision" | "prec") && v.parse::<i32>().is_ok())
}

/// The state of a `loc://` connection opened by a bare or partial address, i.e.
/// one that is not config-bearing. PyDM's constructor ends with
/// `send_connection_state(False)` + `self.connected = False` and never sets a
/// value for such a connection (local_plugin.py:43-45), while still advertising
/// write access (`send_access_state`, :333) so a later `put` is accepted.
fn disconnected_local_state() -> ChannelState {
    ChannelState {
        connected: false,
        write_access: true,
        value: None,
        severity: AlarmSeverity::NoAlarm,
        ..Default::default()
    }
}

/// Build the initial connected [`ChannelState`] for a local variable from its
/// query parameters. Pure (no clock): the connection task stamps the timestamp.
pub(crate) fn initial_local_state(params: &[(String, String)]) -> ChannelState {
    let mut ty = "float";
    let mut init: Option<&str> = None;
    let mut dtype: Option<&str> = None;
    let mut precision: Option<i32> = None;
    let mut units: Option<Arc<str>> = None;
    let mut upper: Option<f64> = None;
    let mut lower: Option<f64> = None;
    let mut enum_strings: Option<Arc<[String]>> = None;
    for (key, value) in params {
        match key.as_str() {
            "type" => ty = value.as_str(),
            "init" => init = Some(value.as_str()),
            // The numpy element dtype for `type=array` (PyDM
            // `_extra_numpy_config_keys`, local_plugin.py:30, :257-288). The
            // other numpy kwargs (copy/order/subok/ndmin) have no effect on the
            // value, matching PyDM in practice, so they stay dropped below.
            "dtype" => dtype = Some(value.as_str()),
            // Extras (parse_channel_extras, local_plugin.py:103-121). `prec`
            // is a rsdm-accepted alias for `precision`.
            "precision" | "prec" => precision = value.parse().ok(),
            "unit" => units = Some(Arc::from(value.as_str())),
            "upper_limit" => upper = value.trim().parse().ok(),
            "lower_limit" => lower = value.trim().parse().ok(),
            "enum_string" => enum_strings = parse_enum_string(value),
            _ => {}
        }
    }

    // PyDM's `convert_value` returns `None` when `init` cannot be converted to
    // the declared type (`except ValueError`, local_plugin.py:318-323), and the
    // connection is still marked connected with a `None` value — never a
    // fabricated type-zero. `parse_init` mirrors that: `None` for an unparsable
    // (or, off the config-bearing path, absent) `init`.
    let value = parse_init(ty, init, dtype);

    // A float variable without an explicit precision derives it from the
    // value (add_listener, local_plugin.py:341-345). No value → nothing to
    // derive from, matching `isinstance(self.value, float)` being False.
    if precision.is_none()
        && let Some(PvValue::Float(v)) = &value
    {
        precision = Some(precision_for_value(*v));
    }

    // Ctrl limits apply to numeric types only ("float" or "int",
    // local_plugin.py:113-118; rsdm treats every non-bool/str/array type as
    // float). PyDM emits each side independently through its own signal;
    // rsdm's state carries the pair, so an absent side defaults to 0.
    let int_ty = matches!(ty, "int" | "integer");
    let numeric = !matches!(ty, "bool" | "boolean" | "str" | "string" | "array");
    let fix = |v: f64| if int_ty { v.trunc() } else { v };
    let ctrl_limits = match (numeric, lower, upper) {
        (false, _, _) | (_, None, None) => None,
        (true, lo, hi) => Some((fix(lo.unwrap_or(0.0)), fix(hi.unwrap_or(0.0)))),
    };

    ChannelState {
        connected: true,
        write_access: true,
        value,
        severity: AlarmSeverity::NoAlarm,
        precision,
        units,
        ctrl_limits,
        enum_strings,
        ..Default::default()
    }
}

/// Parse the `init` string under the declared `type`, returning `None` when the
/// value cannot be converted (PyDM's `convert_value` `except ValueError` path,
/// local_plugin.py:318-323) — the connection is still connected, just with no
/// value. `dtype` is the numpy element dtype kwarg, meaningful only for
/// `type=array`. Numeric (`int`/`float`) and `array` inits can fail to parse
/// and yield `None`; `bool` and `str` never fail (Python `bool(str)`/`str(x)`
/// raise no `ValueError`), so they always carry a value.
fn parse_init(ty: &str, init: Option<&str>, dtype: Option<&str>) -> Option<PvValue> {
    match ty {
        "int" | "integer" => init.and_then(|s| s.trim().parse().ok()).map(PvValue::Int),
        "bool" | "boolean" => Some(PvValue::Bool(init.map(parse_bool).unwrap_or(false))),
        "str" | "string" => Some(PvValue::Str(Arc::from(init.unwrap_or("")))),
        "array" => parse_array(init.unwrap_or(""), dtype),
        // float and anything unrecognized.
        _ => init.and_then(|s| s.trim().parse().ok()).map(PvValue::Float),
    }
}

/// The rsdm array element kind a numpy `dtype` string forces.
enum ArrayKind {
    Int,
    Float,
}

/// Classify a numpy `dtype` kwarg (PyDM `np.dtype(dtype)`,
/// local_plugin.py:257-288) into one of rsdm's two array element kinds. `None`
/// means "not a recognized numeric dtype", so the element type is inferred from
/// the literal instead — matching numpy's default `dtype=object`, which
/// preserves each literal's own int/float type. Only the readable numeric names
/// a `loc://` URL would carry are honored (`float`/`float64`/…, `int`/`int32`/…,
/// `uint*`); exotic numpy typecodes (`f8`, `i4`, complex, str) fall through to
/// inference.
fn array_dtype_kind(dtype: &str) -> Option<ArrayKind> {
    let d = dtype.trim().to_ascii_lowercase();
    if d == "double" || d == "single" || d == "half" || d.starts_with("float") {
        return Some(ArrayKind::Float);
    }
    if d == "long"
        || d == "short"
        || d == "byte"
        || d == "longlong"
        || d.starts_with("int")
        || d.starts_with("uint")
    {
        return Some(ArrayKind::Int);
    }
    None
}

fn parse_bool(s: &str) -> bool {
    let s = s.trim();
    s.eq_ignore_ascii_case("true") || s == "1"
}

/// Parse a `type=array` init — PyDM's `np.array(ast.literal_eval(init),
/// **type_kwargs)` (local_plugin.py:32 + :321-323). With no `dtype` kwarg the
/// element type is inferred: a bracketed (or parenthesized) list of integers
/// becomes an Int waveform; any float element promotes the whole array to Float,
/// like numpy dtype unification. An explicit `dtype` forces the element type
/// (`dtype=float` → Float even for an integer literal; `dtype=int` → Int,
/// truncating float literals toward zero as numpy's int cast does). A valid but
/// empty literal (`[]`) is an empty Float waveform (numpy's `np.array([])`
/// default dtype). Input that is not a list literal, or whose elements are not
/// all numeric, yields `None` — PyDM's `ast.literal_eval` raises `ValueError`
/// there and `convert_value` returns `None` (no value published).
fn parse_array(init: &str, dtype: Option<&str>) -> Option<PvValue> {
    let empty = || PvValue::FloatArray(Arc::from(Vec::new()));
    let s = init.trim();
    let inner = s
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .or_else(|| s.strip_prefix('(').and_then(|t| t.strip_suffix(')')))?;
    let mut tokens: Vec<&str> = inner.split(',').map(str::trim).collect();
    // A Python literal tolerates a trailing comma (`[1, 2,]`).
    if tokens.last() == Some(&"") {
        tokens.pop();
    }
    if tokens.is_empty() {
        // A valid empty list literal → empty Float waveform (not a parse error).
        return Some(empty());
    }

    // An explicit numpy `dtype` forces the element type over the literal's own.
    match dtype.and_then(array_dtype_kind) {
        Some(ArrayKind::Float) => {
            return tokens
                .iter()
                .map(|t| t.parse::<f64>().ok())
                .collect::<Option<Vec<f64>>>()
                .map(|v| PvValue::FloatArray(v.into()));
        }
        Some(ArrayKind::Int) => {
            // numpy's int cast truncates toward zero and accepts float literals
            // (`np.array([1.9], dtype=int)` → `[1]`).
            return tokens
                .iter()
                .map(|t| {
                    t.parse::<i64>()
                        .ok()
                        .or_else(|| t.parse::<f64>().ok().map(|f| f.trunc() as i64))
                })
                .collect::<Option<Vec<i64>>>()
                .map(|v| PvValue::IntArray(v.into()));
        }
        None => {}
    }

    // Inference (no explicit dtype): all-integer → Int, any float → Float.
    if let Some(ints) = tokens
        .iter()
        .map(|t| t.parse::<i64>().ok())
        .collect::<Option<Vec<i64>>>()
    {
        return Some(PvValue::IntArray(ints.into()));
    }
    if let Some(floats) = tokens
        .iter()
        .map(|t| t.parse::<f64>().ok())
        .collect::<Option<Vec<f64>>>()
    {
        return Some(PvValue::FloatArray(floats.into()));
    }
    // Non-numeric elements (`[1, abc]`) — PyDM would raise and publish nothing.
    None
}

/// Parse an `enum_string` extra — PyDM's `tuple(ast.literal_eval(v))`
/// (local_plugin.py:250-255): a `('A','B')` / `["A","B"]` literal of quoted
/// strings. `None` (skipped, like PyDM's caught ValueError) when the literal
/// is not a quoted-string sequence. Commas inside the quoted strings are not
/// supported by this simple grammar.
fn parse_enum_string(v: &str) -> Option<Arc<[String]>> {
    let s = v.trim();
    let inner = s
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .or_else(|| s.strip_prefix('[').and_then(|t| t.strip_suffix(']')))?;
    let mut out = Vec::new();
    for tok in inner.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            // A trailing comma, as in the one-element tuple `('A',)`.
            continue;
        }
        let unquoted = tok
            .strip_prefix('\'')
            .and_then(|t| t.strip_suffix('\''))
            .or_else(|| tok.strip_prefix('"').and_then(|t| t.strip_suffix('"')))?;
        out.push(unquoted.to_owned());
    }
    (!out.is_empty()).then(|| out.into())
}

/// PyDM's `precision_for_value` (local_plugin.py:384-388): the number of
/// digits after the decimal point of `str(value)`, capped at 8. Rust's
/// shortest-roundtrip `Display` matches Python's `str(float)` except that
/// integral floats print without `.0` — those count as 1 digit, like
/// Python's `"1.0"`. (PyDM raises on exponent-form reprs; Rust's f64
/// `Display` never produces exponents, so huge magnitudes land in the
/// no-fraction case instead.)
fn precision_for_value(v: f64) -> i32 {
    let s = format!("{v}");
    match s.split_once('.') {
        Some((_, frac)) => (frac.len() as i32).min(8),
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn float_is_the_default_type() {
        let s = initial_local_state(&params(&[("init", "1.5")]));
        assert!(s.connected);
        assert!(s.write_access);
        assert_eq!(s.value, Some(PvValue::Float(1.5)));
    }

    #[test]
    fn int_type_and_precision() {
        let s = initial_local_state(&params(&[
            ("type", "int"),
            ("init", "7"),
            ("precision", "3"),
        ]));
        assert_eq!(s.value, Some(PvValue::Int(7)));
        assert_eq!(s.precision, Some(3));
    }

    #[test]
    fn bool_and_string_types() {
        assert_eq!(
            initial_local_state(&params(&[("type", "bool"), ("init", "true")])).value,
            Some(PvValue::Bool(true))
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "bool"), ("init", "0")])).value,
            Some(PvValue::Bool(false))
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "str"), ("init", "hello")])).value,
            Some(PvValue::Str(Arc::from("hello")))
        );
    }

    #[test]
    fn missing_or_unparsable_init_has_no_value_but_stays_connected() {
        // PyDM's convert_value returns None when init cannot be converted to
        // the declared type (local_plugin.py:318-323); the connection is still
        // marked connected, just with no value — never a fabricated type-zero.
        // (In production initial_local_state is only reached for config-bearing
        // addresses — both type and init present — but an unparsable init still
        // lands here, as can an absent one off that path.)
        assert_eq!(initial_local_state(&params(&[])).value, None); // float, no init
        assert_eq!(initial_local_state(&params(&[("type", "int")])).value, None);
        assert_eq!(
            initial_local_state(&params(&[("type", "int"), ("init", "notanint")])).value,
            None
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "float"), ("init", "notafloat")])).value,
            None
        );
        // The connection stays connected even with no value.
        assert!(initial_local_state(&params(&[("type", "int"), ("init", "notanint")])).connected);

        // str/bool never fail to convert (Python str(x)/bool(str) raise no
        // ValueError), so they always carry a value, even with no init.
        assert_eq!(
            initial_local_state(&params(&[("type", "str")])).value,
            Some(PvValue::Str(Arc::from("")))
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "bool")])).value,
            Some(PvValue::Bool(false))
        );
    }

    #[test]
    fn array_type_parses_bracketed_lists() {
        // All-integer elements → Int waveform (np.array of ints).
        assert_eq!(
            initial_local_state(&params(&[("type", "array"), ("init", "[1, 2, 3]")])).value,
            Some(PvValue::IntArray(Arc::from([1_i64, 2, 3].as_slice())))
        );
        // Any float element promotes the array to Float (numpy dtype
        // unification), and a Python tuple literal works too.
        assert_eq!(
            initial_local_state(&params(&[("type", "array"), ("init", "(1, 2.5)")])).value,
            Some(PvValue::FloatArray(Arc::from([1.0, 2.5].as_slice())))
        );
        // Trailing comma is a valid Python literal.
        assert_eq!(
            initial_local_state(&params(&[("type", "array"), ("init", "[4,]")])).value,
            Some(PvValue::IntArray(Arc::from([4_i64].as_slice())))
        );
        // A valid but empty list literal is an empty Float waveform
        // (numpy `np.array([])`).
        assert_eq!(
            initial_local_state(&params(&[("type", "array"), ("init", "[]")])).value,
            Some(PvValue::FloatArray(Arc::from([].as_slice())))
        );
        // Absent / non-list / non-numeric init has no value — PyDM's
        // ast.literal_eval raises and convert_value returns None.
        assert_eq!(
            initial_local_state(&params(&[("type", "array")])).value,
            None
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "array"), ("init", "nonsense")])).value,
            None
        );
    }

    #[test]
    fn array_dtype_kwarg_overrides_literal_inference() {
        // dtype=float promotes an all-integer literal to a Float waveform
        // (np.array([1,2,3], dtype=float)); without it inference keeps it Int.
        assert_eq!(
            initial_local_state(&params(&[
                ("type", "array"),
                ("init", "[1, 2, 3]"),
                ("dtype", "float"),
            ]))
            .value,
            Some(PvValue::FloatArray(Arc::from([1.0, 2.0, 3.0].as_slice())))
        );
        // dtype=int truncates float literals toward zero (numpy int cast).
        assert_eq!(
            initial_local_state(&params(&[
                ("type", "array"),
                ("init", "[1.9, 2.1, -3.8]"),
                ("dtype", "int"),
            ]))
            .value,
            Some(PvValue::IntArray(Arc::from([1_i64, 2, -3].as_slice())))
        );
        // Sized aliases resolve too (int64 → Int, float32 → Float).
        assert_eq!(
            initial_local_state(&params(&[
                ("type", "array"),
                ("init", "[4, 5]"),
                ("dtype", "int64"),
            ]))
            .value,
            Some(PvValue::IntArray(Arc::from([4_i64, 5].as_slice())))
        );
        assert_eq!(
            initial_local_state(&params(&[
                ("type", "array"),
                ("init", "[6, 7]"),
                ("dtype", "float32"),
            ]))
            .value,
            Some(PvValue::FloatArray(Arc::from([6.0, 7.0].as_slice())))
        );
        // An unrecognized dtype falls back to literal inference (Int here).
        assert_eq!(
            initial_local_state(&params(&[
                ("type", "array"),
                ("init", "[8, 9]"),
                ("dtype", "complex"),
            ]))
            .value,
            Some(PvValue::IntArray(Arc::from([8_i64, 9].as_slice())))
        );
        // No dtype: inference is unchanged (all-int → Int).
        assert_eq!(
            initial_local_state(&params(&[("type", "array"), ("init", "[1, 2]")])).value,
            Some(PvValue::IntArray(Arc::from([1_i64, 2].as_slice())))
        );
        // dtype only affects arrays: a scalar int with a stray dtype is untouched.
        assert_eq!(
            initial_local_state(&params(&[
                ("type", "int"),
                ("init", "5"),
                ("dtype", "float"),
            ]))
            .value,
            Some(PvValue::Int(5))
        );
    }

    #[test]
    fn unit_extra_sets_units() {
        // parse_channel_extras emits the unit string (local_plugin.py:110-112).
        let s = initial_local_state(&params(&[("init", "1.0"), ("unit", "mm")]));
        assert_eq!(s.units.as_deref(), Some("mm"));
    }

    #[test]
    fn limit_extras_set_ctrl_limits_for_numeric_types() {
        // Both sides (local_plugin.py:113-118 → send_upper/lower_limit).
        let s = initial_local_state(&params(&[
            ("type", "float"),
            ("init", "0.5"),
            ("lower_limit", "-1.5"),
            ("upper_limit", "2.5"),
        ]));
        assert_eq!(s.ctrl_limits, Some((-1.5, 2.5)));

        // int type coerces the limits to integers (int(v), :209-212).
        let s = initial_local_state(&params(&[
            ("type", "int"),
            ("init", "1"),
            ("lower_limit", "-1.7"),
            ("upper_limit", "9.9"),
        ]));
        assert_eq!(s.ctrl_limits, Some((-1.0, 9.0)));

        // One side alone still publishes (PyDM emits sides independently);
        // the missing side defaults to 0.
        let s = initial_local_state(&params(&[("init", "0.0"), ("upper_limit", "5.0")]));
        assert_eq!(s.ctrl_limits, Some((0.0, 5.0)));

        // Non-numeric types get no limits (the type gate at :114/:117).
        let s = initial_local_state(&params(&[
            ("type", "str"),
            ("init", "x"),
            ("upper_limit", "5"),
        ]));
        assert_eq!(s.ctrl_limits, None);
    }

    #[test]
    fn enum_string_extra_parses_quoted_sequences() {
        // tuple(ast.literal_eval(...)) of a tuple/list of quoted strings
        // (local_plugin.py:250-255).
        let s = initial_local_state(&params(&[
            ("type", "int"),
            ("init", "0"),
            ("enum_string", "('Off', 'On')"),
        ]));
        assert_eq!(
            s.enum_strings.as_deref(),
            Some(["Off".to_owned(), "On".to_owned()].as_slice())
        );

        // A list literal with double quotes works too.
        let s = initial_local_state(&params(&[("enum_string", r#"["A", "B"]"#)]));
        assert_eq!(
            s.enum_strings.as_deref(),
            Some(["A".to_owned(), "B".to_owned()].as_slice())
        );

        // A malformed literal is skipped, like PyDM's caught ValueError.
        let s = initial_local_state(&params(&[("enum_string", "not a tuple")]));
        assert_eq!(s.enum_strings, None);
    }

    #[test]
    fn float_auto_precision_derives_from_the_value() {
        // precision_for_value: decimal digits of str(value), capped at 8
        // (local_plugin.py:384-388), applied when no explicit precision is
        // configured (:341-345).
        assert_eq!(
            initial_local_state(&params(&[("init", "1.25")])).precision,
            Some(2)
        );
        // An integral float counts as 1 digit (Python str(1.0) == "1.0").
        assert_eq!(
            initial_local_state(&params(&[("init", "3")])).precision,
            Some(1)
        );
        // No init → no value → nothing to derive a precision from (PyDM's
        // `isinstance(self.value, float)` is False when the value is None).
        assert_eq!(initial_local_state(&params(&[])).precision, None);
        // Cap at 8 digits.
        assert_eq!(
            initial_local_state(&params(&[("init", "0.1234567891")])).precision,
            Some(8)
        );
        // An explicit precision wins over the derived one.
        assert_eq!(
            initial_local_state(&params(&[("init", "1.25"), ("precision", "5")])).precision,
            Some(5)
        );
        // Non-float types derive nothing.
        assert_eq!(
            initial_local_state(&params(&[("type", "int"), ("init", "7")])).precision,
            None
        );
    }

    #[test]
    fn config_bearing_requires_both_nonempty_type_and_init() {
        // PyDM configures only with name+type+init (_required_config_keys,
        // local_plugin.py:26). name is the loc host; the predicate is the two
        // query keys, both non-empty.
        assert!(is_config_bearing(&params(&[
            ("type", "int"),
            ("init", "5")
        ])));
        // Bare reader: neither key.
        assert!(!is_config_bearing(&params(&[])));
        // Partial: one key missing.
        assert!(!is_config_bearing(&params(&[("type", "int")])));
        assert!(!is_config_bearing(&params(&[("init", "5")])));
        // Empty values do not count (PyDM's parse_qs drops blank values).
        assert!(!is_config_bearing(&params(&[("type", ""), ("init", "5")])));
        assert!(!is_config_bearing(&params(&[
            ("type", "int"),
            ("init", "")
        ])));
        // Extra keys alongside a complete pair still configure.
        assert!(is_config_bearing(&params(&[
            ("type", "float"),
            ("init", "1.5"),
            ("precision", "3"),
        ])));
    }

    #[test]
    fn explicit_precision_detected_only_when_it_parses() {
        // _precision_set is set only when precision converts to int
        // (local_plugin.py:103-109).
        assert!(has_explicit_precision(&params(&[("precision", "3")])));
        assert!(has_explicit_precision(&params(&[("prec", "0")])));
        assert!(!has_explicit_precision(&params(&[])));
        // A non-integer precision does not set it (PyDM's caught ValueError).
        assert!(!has_explicit_precision(&params(&[("precision", "abc")])));
        assert!(!has_explicit_precision(&params(&[("precision", "")])));
    }

    #[test]
    fn disconnected_state_has_no_value_and_write_access() {
        // Bare/partial addresses connect disconnected with no value but with
        // write access advertised (PyDM constructor :43-45 + send_access_state).
        let s = disconnected_local_state();
        assert!(!s.connected);
        assert_eq!(s.value, None);
        assert!(s.write_access);
        assert_eq!(s.severity, AlarmSeverity::NoAlarm);
        // effective_severity reports Disconnected while not connected.
        assert_eq!(s.effective_severity(), AlarmSeverity::Disconnected);
    }
}
