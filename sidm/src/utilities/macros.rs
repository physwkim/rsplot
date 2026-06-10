//! Macro substitution and macro-string parsing.
//!
//! Ports the two pieces of `pydm/utilities/macro.py` a display layer needs:
//!
//! - [`substitute`] — replace `${name}` / `$name` in a template, leaving
//!   unknown macros intact (Python `string.Template.safe_substitute`), with
//!   PyDM's up-to-100-round nested expansion.
//! - [`parse_macro_string`] — parse an EPICS-style `"name=value,name2=value2"`
//!   string into a map, a direct port of the `macParseDefns` state machine
//!   PyDM reuses.
//!
//! PyDM tries JSON (`{"name": "value"}`) before the EPICS form; JSON macro
//! strings are deferred here (a launcher/CLI concern — programmatic displays
//! build the map directly). PyDM also escapes quotes inside values before
//! substitution for shell/python safety; that is specific to embedding macros
//! in command strings and is intentionally not done here.

use std::collections::HashMap;

/// Error returned when a macro string is neither JSON nor EPICS-style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MacroParseError {
    /// The string contained no `=`, so it cannot be an EPICS macro string.
    NotMacroSyntax,
}

impl std::fmt::Display for MacroParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMacroSyntax => {
                write!(f, "could not parse macro argument (no '=' present)")
            }
        }
    }
}

impl std::error::Error for MacroParseError {}

/// Maximum macro-expansion recursion depth. PyDM caps re-expansion at 100
/// rounds; this is the equivalent guard against self-referential macros.
const MAX_EXPAND_DEPTH: u32 = 100;

/// Substitute `${name}` and `$name` references in `template` from `macros`,
/// leaving unknown references untouched. `$$` is an escaped literal `$`.
///
/// A macro value that itself contains `${...}` is expanded recursively (the
/// inserted value is scanned for further references), bounded by
/// [`MAX_EXPAND_DEPTH`] so a self-referential macro terminates by emitting the
/// reference verbatim. This differs structurally from PyDM's
/// "re-`Template` the whole string up to 100 times" loop in one way that
/// matters: a `$` produced by the `$$` escape is **never re-scanned**, so `$$`
/// is a stable literal even when the following text is itself a macro name
/// (PyDM's loop would re-expand `$$P` to the value of `P`). Results are
/// identical to PyDM for all non-pathological inputs.
pub fn substitute(template: &str, macros: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    expand_into(&mut out, template, macros, 0);
    out
}

/// Expand `template` into `out`, recursing into macro values. At the depth cap
/// the template is emitted verbatim (cycle / runaway guard).
fn expand_into(out: &mut String, template: &str, macros: &HashMap<String, String>, depth: u32) {
    if depth > MAX_EXPAND_DEPTH {
        out.push_str(template);
        return;
    }
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Copy this UTF-8 char whole (non-`$` bytes only start here).
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&template[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        // bytes[i] == '$'
        match bytes.get(i + 1) {
            Some(b'$') => {
                // Escaped literal '$' — emitted directly, never re-scanned.
                out.push('$');
                i += 2;
            }
            Some(b'{') => {
                // ${name}
                if let Some(close) = template[i + 2..].find('}') {
                    let name = &template[i + 2..i + 2 + close];
                    match macros.get(name) {
                        Some(v) => expand_into(out, v, macros, depth + 1),
                        None => out.push_str(&template[i..i + 2 + close + 1]),
                    }
                    i += 2 + close + 1;
                } else {
                    // No closing brace: emit the rest verbatim.
                    out.push_str(&template[i..]);
                    break;
                }
            }
            Some(&c) if is_ident_start(c) => {
                // $name (identifier = [A-Za-z_][A-Za-z0-9_]*)
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && is_ident_continue(bytes[end]) {
                    end += 1;
                }
                let name = &template[start..end];
                match macros.get(name) {
                    Some(v) => expand_into(out, v, macros, depth + 1),
                    None => out.push_str(&template[i..end]),
                }
                i = end;
            }
            _ => {
                // Lone '$' (end of string or followed by a non-identifier).
                out.push('$');
                i += 1;
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Parse states for the EPICS macro-string machine (PyDM `macParseDefns`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PreName,
    InName,
    PreVal,
    InVal,
}

/// Parse an EPICS-style macro string `"name=value,name2=value2"` into a map.
///
/// A direct port of the `libCom/macUtil.c` `macParseDefns` state machine that
/// PyDM reuses in `parse_macro_string` (after its JSON attempt, which is not
/// implemented here). Whitespace around names/values is trimmed, single and
/// double quotes group values containing commas/spaces, and `\` escapes the
/// next character. Returns [`MacroParseError::NotMacroSyntax`] when the string
/// contains no `=`.
pub fn parse_macro_string(macro_string: &str) -> Result<HashMap<String, String>, MacroParseError> {
    if macro_string.is_empty() {
        return Ok(HashMap::new());
    }
    if !macro_string.contains('=') {
        return Err(MacroParseError::NotMacroSyntax);
    }

    let chars: Vec<char> = macro_string.chars().collect();
    let n = chars.len();
    let mut macros = HashMap::new();
    let mut state = State::PreName;
    let mut quote: Option<char> = None;
    let mut name_start: Option<usize> = None;
    let mut name_end: Option<usize> = None;
    let mut val_start: Option<usize> = None;
    let mut val_end: Option<usize> = None;

    for i in 0..n {
        let c = chars[i];

        // Quote handling: a closing quote clears the quote; an opening quote is
        // consumed (PyDM `continue`s on the opening quote).
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }
        // `escape` looks at the previous char; unlike PyDM's `s[i-1]` (which
        // wraps to the last char for i==0), i==0 is treated as un-escaped.
        let escape = i > 0 && chars[i - 1] == '\\';

        match state {
            State::PreName => {
                if quote.is_none() && !escape && (c.is_whitespace() || c == ',') {
                    continue;
                }
                name_start = Some(i);
                state = State::InName;
            }
            State::InName => {
                if quote.is_some() || escape {
                    continue;
                }
                if c == '=' || c == ',' {
                    name_end = Some(i);
                    state = State::PreVal;
                }
            }
            State::PreVal => {
                if quote.is_none() && !escape && c.is_whitespace() {
                    continue;
                }
                val_start = Some(i);
                state = State::InVal;
                if i == n - 1 {
                    val_end = Some(i + 1);
                }
            }
            State::InVal => {
                if quote.is_some() || escape {
                    continue;
                }
                if c == ',' {
                    val_end = Some(i);
                    state = State::PreName;
                } else if i == n - 1 {
                    val_end = Some(i + 1);
                    state = State::PreName;
                } else {
                    continue;
                }
            }
        }

        if let (Some(ns), Some(ne), Some(vs), Some(ve)) = (name_start, name_end, val_start, val_end)
        {
            let key: String = chars[ns..ne]
                .iter()
                .collect::<String>()
                .trim()
                .replace('\\', "");
            let val: String = chars[vs..ve]
                .iter()
                .collect::<String>()
                .trim()
                .trim_matches(|ch| ch == '"' || ch == '\'')
                .replace('\\', "");
            macros.insert(key, val);
            name_start = None;
            name_end = None;
            val_start = None;
            val_end = None;
            state = State::PreName;
        }
    }

    Ok(macros)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn brace_substitution() {
        let m = map(&[("P", "S1"), ("R", "m1")]);
        assert_eq!(substitute("${P}:${R}", &m), "S1:m1");
    }

    #[test]
    fn bare_dollar_name_substitution() {
        let m = map(&[("MOTOR", "m1")]);
        assert_eq!(substitute("$MOTOR.RBV", &m), "m1.RBV");
    }

    #[test]
    fn unknown_macro_left_intact() {
        let m = map(&[]);
        assert_eq!(substitute("${X}", &m), "${X}");
        assert_eq!(substitute("$Y_end", &m), "$Y_end");
    }

    #[test]
    fn escaped_dollar_is_literal() {
        let m = map(&[("P", "S1")]);
        assert_eq!(substitute("$$P=${P}", &m), "$P=S1");
    }

    #[test]
    fn nested_macros_expand() {
        let m = map(&[("A", "${B}"), ("B", "v")]);
        assert_eq!(substitute("${A}", &m), "v");
    }

    #[test]
    fn self_referential_macro_terminates_verbatim() {
        // A → A cycle must not hang; at the depth cap the reference is emitted
        // verbatim rather than expanded further.
        let m = map(&[("A", "${A}")]);
        assert_eq!(substitute("${A}", &m), "${A}");
    }

    #[test]
    fn lone_trailing_dollar_is_literal() {
        let m = map(&[]);
        assert_eq!(substitute("cost is 5$", &m), "cost is 5$");
    }

    #[test]
    fn parse_simple_pairs() {
        let m = parse_macro_string("P=S1,R=m1").unwrap();
        assert_eq!(m.get("P"), Some(&"S1".to_owned()));
        assert_eq!(m.get("R"), Some(&"m1".to_owned()));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn parse_trims_spaces() {
        let m = parse_macro_string(" P = S1 , R = m1 ").unwrap();
        assert_eq!(m.get("P"), Some(&"S1".to_owned()));
        assert_eq!(m.get("R"), Some(&"m1".to_owned()));
    }

    #[test]
    fn parse_quoted_value_with_comma() {
        let m = parse_macro_string("CMD=\"a,b\",R=2").unwrap();
        assert_eq!(m.get("CMD"), Some(&"a,b".to_owned()));
        assert_eq!(m.get("R"), Some(&"2".to_owned()));
    }

    #[test]
    fn parse_single_pair_no_trailing_comma() {
        let m = parse_macro_string("DEV=test").unwrap();
        assert_eq!(m.get("DEV"), Some(&"test".to_owned()));
    }

    #[test]
    fn parse_empty_is_empty_map() {
        assert_eq!(parse_macro_string("").unwrap().len(), 0);
    }

    #[test]
    fn parse_without_equals_errors() {
        assert_eq!(
            parse_macro_string("not a macro"),
            Err(MacroParseError::NotMacroSyntax)
        );
    }
}
