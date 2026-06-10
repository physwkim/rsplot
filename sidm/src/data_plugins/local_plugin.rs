//! `loc://` — in-process variables.
//!
//! Ports `pydm/data_plugins/local_plugin.py`. A local variable's type and
//! initial value come from the address query parameters
//! (`loc://name?type=float&init=1.5&precision=3`); writes replace the value and
//! echo to every listener. Because the engine pools connections by
//! `scheme://full_address` (query dropped), all `loc://name?...` addresses with
//! the same `name` share one connection — so the variable is shared by name and
//! the parameters apply only on the first connection, matching PyDM.
//!
//! Supported `type`s: `float` (default), `int`, `bool`, `str`. Array variables
//! are deferred.

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
            cancel,
            runtime,
            address,
        } = ctx;

        // Publish the initial (connected) state from the query parameters.
        let init = initial_local_state(&address.query_params());
        writer.update(move |s| {
            *s = init;
            s.timestamp = Some(SystemTime::now());
        });

        runtime.spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    maybe = writes.recv() => match maybe {
                        Some(value) => writer.update(|s| {
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

/// Build the initial connected [`ChannelState`] for a local variable from its
/// query parameters. Pure (no clock): the connection task stamps the timestamp.
pub(crate) fn initial_local_state(params: &[(String, String)]) -> ChannelState {
    let mut ty = "float";
    let mut init: Option<&str> = None;
    let mut precision: Option<i32> = None;
    for (key, value) in params {
        match key.as_str() {
            "type" => ty = value.as_str(),
            "init" => init = Some(value.as_str()),
            "precision" | "prec" => precision = value.parse().ok(),
            _ => {}
        }
    }

    ChannelState {
        connected: true,
        write_access: true,
        value: Some(parse_init(ty, init)),
        severity: AlarmSeverity::NoAlarm,
        precision,
        ..Default::default()
    }
}

/// Parse the `init` string under the declared `type`, falling back to a
/// type-appropriate zero when `init` is absent or unparsable.
fn parse_init(ty: &str, init: Option<&str>) -> PvValue {
    match ty {
        "int" | "integer" => PvValue::Int(init.and_then(|s| s.trim().parse().ok()).unwrap_or(0)),
        "bool" | "boolean" => PvValue::Bool(init.map(parse_bool).unwrap_or(false)),
        "str" | "string" => PvValue::Str(Arc::from(init.unwrap_or(""))),
        // float and anything unrecognized.
        _ => PvValue::Float(init.and_then(|s| s.trim().parse().ok()).unwrap_or(0.0)),
    }
}

fn parse_bool(s: &str) -> bool {
    let s = s.trim();
    s.eq_ignore_ascii_case("true") || s == "1"
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
    fn missing_init_is_type_zero() {
        assert_eq!(
            initial_local_state(&params(&[])).value,
            Some(PvValue::Float(0.0))
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "int")])).value,
            Some(PvValue::Int(0))
        );
        assert_eq!(
            initial_local_state(&params(&[("type", "str")])).value,
            Some(PvValue::Str(Arc::from("")))
        );
    }

    #[test]
    fn unparsable_init_falls_back_to_zero() {
        assert_eq!(
            initial_local_state(&params(&[("type", "int"), ("init", "notanint")])).value,
            Some(PvValue::Int(0))
        );
    }
}
