//! Loopback `Host` guard against DNS rebinding.
//!
//! The REST API binds to loopback and has no authentication: a page served
//! from a hostname that re-resolves to 127.0.0.1 could otherwise call it
//! same-origin (no CORS involved) and read the indexed code. The MCP transport
//! at `/mcp` runs the same check inside rmcp; this middleware extends it to
//! the rest of the router, with the same allowed names (`AllowedHosts` is
//! also handed to rmcp so both checks agree).

use axum::extract::{Request, State};
use axum::http::header::HOST;
use axum::http::uri::Authority;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::IpAddr;
use tracing::warn;

/// Host names a loopback-bound server may be addressed by, as rmcp allows
/// them by default for `/mcp`. Any port is accepted.
const LOOPBACK_NAMES: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// The host names a request may carry: the loopback names plus the bound
/// address itself (`--http-host 127.0.0.2` is loopback too, and its clients
/// send `Host: 127.0.0.2:<port>`).
#[derive(Debug, Clone)]
pub(super) struct AllowedHosts(Vec<String>);

impl AllowedHosts {
    pub(super) fn for_bind_address(host: IpAddr) -> Self {
        let mut names: Vec<String> = LOOPBACK_NAMES.iter().map(|n| n.to_string()).collect();
        let bound = normalize_host(&host.to_string());
        if !names.contains(&bound) {
            names.push(bound);
        }
        Self(names)
    }

    /// The names, for rmcp's `with_allowed_hosts` (host only, any port).
    pub(super) fn names(&self) -> &[String] {
        &self.0
    }

    fn allows(&self, host: &str) -> bool {
        self.0.iter().any(|allowed| allowed == host)
    }
}

/// Reject with 403 any request whose `Host` header (or HTTP/2 `:authority`)
/// is not an allowed name; 400 when neither is present or parseable.
pub(super) async fn require_allowed_host(
    State(allowed): State<AllowedHosts>,
    request: Request,
    next: Next,
) -> Response {
    match request_host(request.uri(), request.headers()) {
        Ok(host) if allowed.allows(&host) => next.run(request).await,
        Ok(host) => {
            warn!(
                host,
                path = request.uri().path(),
                "rejected request with disallowed Host header (possible DNS rebinding attempt)"
            );
            (
                StatusCode::FORBIDDEN,
                "Forbidden: Host header is not allowed",
            )
                .into_response()
        }
        Err(()) => {
            warn!(
                path = request.uri().path(),
                "rejected request with missing or malformed Host header"
            );
            (
                StatusCode::BAD_REQUEST,
                "Bad Request: missing or invalid Host header",
            )
                .into_response()
        }
    }
}

/// The host name the request was addressed to, lowercased, without port or
/// IPv6 brackets: from `Host`, else from the URI authority (HTTP/2).
fn request_host(uri: &Uri, headers: &HeaderMap) -> Result<String, ()> {
    if let Some(value) = headers.get(HOST) {
        let value = value.to_str().map_err(|_| ())?;
        let authority = Authority::try_from(value.trim()).map_err(|_| ())?;
        return Ok(normalize_host(authority.host()));
    }
    uri.authority()
        .map(|authority| normalize_host(authority.host()))
        .ok_or(())
}

fn normalize_host(host: &str) -> String {
    host.trim_matches(['[', ']']).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn host_of(header: Option<&str>, uri: &str) -> Result<String, ()> {
        let mut headers = HeaderMap::new();
        if let Some(header) = header {
            headers.insert(HOST, HeaderValue::from_str(header).unwrap());
        }
        request_host(&uri.parse().unwrap(), &headers)
    }

    #[test]
    fn host_header_is_normalized() {
        assert_eq!(host_of(Some("localhost:3000"), "/"), Ok("localhost".into()));
        assert_eq!(host_of(Some("LOCALHOST"), "/"), Ok("localhost".into()));
        assert_eq!(host_of(Some("127.0.0.1:3000"), "/"), Ok("127.0.0.1".into()));
        assert_eq!(host_of(Some("[::1]:3000"), "/"), Ok("::1".into()));
        assert_eq!(
            host_of(Some("evil.example:3000"), "/"),
            Ok("evil.example".into())
        );
    }

    #[test]
    fn authority_is_the_fallback() {
        assert_eq!(
            host_of(None, "http://localhost:3000/health"),
            Ok("localhost".into())
        );
        assert_eq!(
            host_of(None, "http://evil.example/health"),
            Ok("evil.example".into())
        );
        // The header wins over the URI when both are present.
        assert_eq!(
            host_of(Some("evil.example"), "http://localhost/health"),
            Ok("evil.example".into())
        );
        assert_eq!(host_of(None, "/health"), Err(()));
        assert_eq!(host_of(Some(""), "/health"), Err(()));
        assert_eq!(host_of(Some("not a host"), "/health"), Err(()));
    }

    #[test]
    fn loopback_names_and_the_bound_address_are_allowed() {
        let allowed = AllowedHosts::for_bind_address("127.0.0.1".parse().unwrap());
        assert!(allowed.allows("localhost"));
        assert!(allowed.allows("127.0.0.1"));
        assert!(allowed.allows("::1"));
        assert!(!allowed.allows("evil.example"));
        assert!(!allowed.allows("localhost.evil.example"));
        assert!(!allowed.allows("127.0.0.2"));
        assert!(!allowed.allows("0.0.0.0"));
        assert_eq!(allowed.names().len(), 3, "127.0.0.1 is not listed twice");

        // Another loopback address: its own name is accepted as well.
        let allowed = AllowedHosts::for_bind_address("127.0.0.2".parse().unwrap());
        assert!(allowed.allows("127.0.0.2"));
        assert!(allowed.allows("localhost"));
        assert!(!allowed.allows("127.0.0.3"));
        assert_eq!(
            allowed.names(),
            ["localhost", "127.0.0.1", "::1", "127.0.0.2"]
        );

        let allowed = AllowedHosts::for_bind_address("::1".parse().unwrap());
        assert_eq!(allowed.names().len(), 3);
    }
}
