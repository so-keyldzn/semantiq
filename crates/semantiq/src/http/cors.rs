//! Validation of `--cors-origin`.
//!
//! The REST API has no authentication and the `Host` guard (`host.rs`) only
//! stops DNS rebinding: a page on any origin can still send a request to
//! `http://127.0.0.1:<port>/search`, and whether its script may read the
//! answer is decided by CORS alone. `--cors-origin '*'` would therefore hand
//! the indexed source to every website the developer visits while `serve`
//! runs, so the flag only accepts named `http(s)://host[:port]` origins.

use anyhow::{Result, bail};
use axum::http::{HeaderValue, Uri};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// The origins `--cors-origin` allows, validated and normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorsOrigins(Vec<HeaderValue>);

impl CorsOrigins {
    /// Parse the flag: one origin or a comma-separated list, each an absolute
    /// `http://` or `https://` origin without path, query or credentials.
    /// `*` and `null` are refused.
    pub(crate) fn parse(spec: &str) -> Result<Self> {
        let origins = spec
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_origin)
            .collect::<Result<Vec<_>>>()?;
        if origins.is_empty() {
            bail!("--cors-origin is empty; expected an origin such as https://example.com");
        }
        Ok(Self(origins))
    }

    /// The allowed origins in the form the browser sends them.
    pub(crate) fn origins(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|v| v.to_str().unwrap_or_default())
    }

    /// The CORS middleware allowing exactly these origins.
    pub(super) fn layer(&self) -> CorsLayer {
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(self.0.iter().cloned()))
            .allow_methods(Any)
            .allow_headers(Any)
    }
}

/// Validate one origin and normalise it (`scheme://host[:port]`, lower-case),
/// which is the exact form of the `Origin` header a browser sends.
fn parse_origin(origin: &str) -> Result<HeaderValue> {
    if origin == "*" {
        bail!(
            "--cors-origin '*' would let every website read the indexed source; \
             name the page's origin instead, e.g. https://example.com"
        );
    }
    if origin.eq_ignore_ascii_case("null") {
        bail!("--cors-origin 'null' is not allowed: it matches sandboxed and file:// pages");
    }
    let uri: Uri = origin.parse().map_err(|e| {
        anyhow::anyhow!("invalid --cors-origin {origin:?}: {e}; expected https://host[:port]")
    })?;
    let scheme = match uri.scheme_str() {
        Some(s) if s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https") => {
            s.to_ascii_lowercase()
        }
        Some(other) => {
            bail!("invalid --cors-origin {origin:?}: scheme {other:?} is not http or https")
        }
        None => bail!("invalid --cors-origin {origin:?}: expected https://host[:port]"),
    };
    let Some(authority) = uri.authority() else {
        bail!("invalid --cors-origin {origin:?}: missing host");
    };
    if authority.as_str().contains('@') {
        bail!("invalid --cors-origin {origin:?}: an origin carries no credentials");
    }
    let path_is_empty = matches!(uri.path(), "" | "/");
    if !path_is_empty || uri.query().is_some() || origin.contains('#') {
        bail!("invalid --cors-origin {origin:?}: an origin has no path, query or fragment");
    }
    let host = authority.host().to_ascii_lowercase();
    if host.is_empty() {
        bail!("invalid --cors-origin {origin:?}: missing host");
    }
    if !is_host_name(&host) {
        bail!(
            "invalid --cors-origin {origin:?}: {host:?} is not a host name or IP address \
             (wildcards are not allowed)"
        );
    }
    let normalised = match authority.port_u16() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    };
    HeaderValue::from_str(&normalised)
        .map_err(|e| anyhow::anyhow!("invalid --cors-origin {origin:?}: {e}"))
}

/// A DNS name or IPv4 address (letters, digits, `-`, `.`) or a bracketed IPv6
/// literal: `http::Uri` itself lets `*` through.
fn is_host_name(host: &str) -> bool {
    if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return v6.parse::<std::net::Ipv6Addr>().is_ok();
    }
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origins(spec: &str) -> Vec<String> {
        CorsOrigins::parse(spec)
            .unwrap()
            .origins()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn wildcard_and_null_are_refused() {
        let err = CorsOrigins::parse("*").unwrap_err().to_string();
        assert!(err.contains("every website"), "{err}");
        assert!(CorsOrigins::parse("null").is_err());
        assert!(CorsOrigins::parse("NULL").is_err());
        assert!(CorsOrigins::parse("https://example.com, *").is_err());
        assert!(CorsOrigins::parse("").is_err());
        assert!(CorsOrigins::parse(" , ").is_err());
    }

    #[test]
    fn absolute_http_origins_are_normalised() {
        assert_eq!(origins("https://example.com"), ["https://example.com"]);
        assert_eq!(origins("https://example.com/"), ["https://example.com"]);
        assert_eq!(
            origins("HTTPS://Example.COM:8443"),
            ["https://example.com:8443"]
        );
        assert_eq!(origins("http://localhost:5173"), ["http://localhost:5173"]);
        assert_eq!(origins("http://[::1]:5173"), ["http://[::1]:5173"]);
        assert_eq!(
            origins("http://localhost:5173, https://demo.example.com"),
            ["http://localhost:5173", "https://demo.example.com"]
        );
    }

    #[test]
    fn non_origins_are_refused() {
        for bad in [
            "example.com",
            "localhost:5173",
            "ftp://example.com",
            "file:///tmp/index.html",
            "https://example.com/app",
            "https://example.com/?x=1",
            "https://example.com#top",
            "https://user:pw@example.com",
            "https://*.example.com",
            "https://",
        ] {
            let err = CorsOrigins::parse(bad).unwrap_err().to_string();
            assert!(err.contains("cors-origin"), "{bad}: {err}");
        }
    }
}
