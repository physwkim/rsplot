//! Channel address parsing — `protocol://netloc/path?query`.
//!
//! A faithful port of PyDM's `pydm/utilities/remove_protocol.py`
//! (`parsed_address`, `protocol_and_address`, `remove_protocol`). The same
//! decomposition regex is used: `scheme` is required for a fully-qualified
//! address, while `netloc`, `path`, and `query` are optional but must appear in
//! that order. When an address carries no `scheme://` prefix the engine applies
//! its default protocol (PyDM `config.DEFAULT_PROTOCOL`) via
//! [`PvAddress::with_default_protocol`].

/// A parsed channel address.
///
/// `scheme` is `None` when the raw address had no `protocol://` prefix; the
/// engine fills it from the default protocol before looking up a plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PvAddress {
    scheme: Option<String>,
    netloc: String,
    path: String,
    query: String,
    raw: String,
}

impl PvAddress {
    /// Parse a raw address into its components, mirroring PyDM `parsed_address`.
    ///
    /// No default protocol is applied here (the pure layer does not know it);
    /// `scheme` stays `None` for a bare address. Decomposition matches PyDM's
    /// `(.*?)://([^/?]*)(?:(/[^?]*)?(?:\?(.*))?)?` regex without pulling in a
    /// regex dependency.
    pub fn parse(raw: &str) -> Self {
        let (scheme, rest) = match raw.find("://") {
            Some(idx) => (Some(raw[..idx].to_owned()), &raw[idx + 3..]),
            None => (None, raw),
        };

        // netloc = everything up to the first '/' or '?'.
        let netloc_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let netloc = rest[..netloc_end].to_owned();
        let after_netloc = &rest[netloc_end..];

        // path = a leading '/...' segment up to the first '?'.
        let (path, query) = match after_netloc.split_once('?') {
            Some((p, q)) => (p.to_owned(), q.to_owned()),
            None => (after_netloc.to_owned(), String::new()),
        };

        Self {
            scheme,
            netloc,
            path,
            query,
            raw: raw.to_owned(),
        }
    }

    /// Apply a default protocol when the address had no `scheme://` prefix
    /// (PyDM prepends `config.DEFAULT_PROTOCOL`). A no-op if a scheme is set.
    #[must_use]
    pub fn with_default_protocol(mut self, default: &str) -> Self {
        if self.scheme.is_none() {
            self.scheme = Some(default.to_owned());
        }
        self
    }

    /// The protocol (`"ca"`, `"loc"`, …), or `None` for a bare address.
    pub fn scheme(&self) -> Option<&str> {
        self.scheme.as_deref()
    }

    /// The network-location component (the PV name for `ca`/`pva`, the variable
    /// name for `loc`).
    pub fn netloc(&self) -> &str {
        &self.netloc
    }

    /// The `/path` component (empty when absent).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The raw query string after `?` (empty when absent).
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The address as originally given.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// `netloc` + `path` — PyDM's `get_full_address`. This is the PV identity
    /// independent of any query parameters.
    pub fn full_address(&self) -> String {
        format!("{}{}", self.netloc, self.path)
    }

    /// The connection-pool key — PyDM's `connection_id`: `scheme://full_address`
    /// with the query **dropped**. Two addresses that differ only in query
    /// parameters (e.g. `loc://x?init=1` and `loc://x`) share one connection;
    /// the parameters apply only on the first connection, matching PyDM.
    pub fn connection_id(&self) -> String {
        format!(
            "{}://{}",
            self.scheme.as_deref().unwrap_or(""),
            self.full_address()
        )
    }

    /// Query parameters parsed as `key=value` pairs separated by `&`
    /// (`loc://x?type=float&init=1.5`). A bare `key` with no `=` yields an empty
    /// value; empty segments are skipped. Order is preserved.
    pub fn query_params(&self) -> Vec<(String, String)> {
        if self.query.is_empty() {
            return Vec::new();
        }
        self.query
            .split('&')
            .filter(|seg| !seg.is_empty())
            .map(|seg| match seg.split_once('=') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => (seg.to_owned(), String::new()),
            })
            .collect()
    }
}

/// Split an address into `(protocol, rest)` — PyDM `protocol_and_address`.
/// `protocol` is `None` when there is no `://`.
pub fn protocol_and_address(address: &str) -> (Option<&str>, &str) {
    match address.find("://") {
        Some(idx) => (Some(&address[..idx]), &address[idx + 3..]),
        None => (None, address),
    }
}

/// Strip the `protocol://` prefix — PyDM `remove_protocol`.
pub fn remove_protocol(address: &str) -> &str {
    protocol_and_address(address).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_pv_has_scheme_and_netloc_only() {
        let a = PvAddress::parse("ca://SOME:PV:NAME");
        assert_eq!(a.scheme(), Some("ca"));
        assert_eq!(a.netloc(), "SOME:PV:NAME");
        assert_eq!(a.path(), "");
        assert_eq!(a.query(), "");
        assert_eq!(a.full_address(), "SOME:PV:NAME");
        assert_eq!(a.connection_id(), "ca://SOME:PV:NAME");
    }

    #[test]
    fn loc_query_params_parse_in_order() {
        let a = PvAddress::parse("loc://x?type=float&init=1.5");
        assert_eq!(a.scheme(), Some("loc"));
        assert_eq!(a.netloc(), "x");
        assert_eq!(a.query(), "type=float&init=1.5");
        assert_eq!(
            a.query_params(),
            vec![
                ("type".to_owned(), "float".to_owned()),
                ("init".to_owned(), "1.5".to_owned()),
            ]
        );
    }

    #[test]
    fn bare_address_gets_default_protocol() {
        let a = PvAddress::parse("BARE:PV");
        assert_eq!(a.scheme(), None);
        assert_eq!(a.netloc(), "BARE:PV");
        let a = a.with_default_protocol("ca");
        assert_eq!(a.scheme(), Some("ca"));
        assert_eq!(a.connection_id(), "ca://BARE:PV");
    }

    #[test]
    fn default_protocol_is_noop_when_scheme_present() {
        let a = PvAddress::parse("pva://DEV").with_default_protocol("ca");
        assert_eq!(a.scheme(), Some("pva"));
    }

    #[test]
    fn connection_id_drops_query_so_loc_vars_share_by_name() {
        let with_init = PvAddress::parse("loc://x?init=1").connection_id();
        let bare = PvAddress::parse("loc://x").connection_id();
        assert_eq!(with_init, bare);
        assert_eq!(with_init, "loc://x");
    }

    #[test]
    fn path_separates_from_netloc_and_query() {
        let a = PvAddress::parse("pva://dev/sub?q=1");
        assert_eq!(a.netloc(), "dev");
        assert_eq!(a.path(), "/sub");
        assert_eq!(a.query(), "q=1");
        assert_eq!(a.full_address(), "dev/sub");
    }

    #[test]
    fn protocol_helpers() {
        assert_eq!(protocol_and_address("ca://PV"), (Some("ca"), "PV"));
        assert_eq!(protocol_and_address("PV"), (None, "PV"));
        assert_eq!(remove_protocol("loc://x?init=1"), "x?init=1");
        assert_eq!(remove_protocol("PV"), "PV");
    }

    #[test]
    fn bare_query_key_without_equals_yields_empty_value() {
        let a = PvAddress::parse("fake://gen?sine");
        assert_eq!(a.query_params(), vec![("sine".to_owned(), String::new())]);
    }
}
