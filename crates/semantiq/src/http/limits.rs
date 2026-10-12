//! Request limits of the REST query endpoints: a timeout, a concurrency cap
//! and load shedding.
//!
//! Every handler ends up on the single SQLite connection, so a burst of slow
//! `/search` or `/dead-code` calls during a reindex used to queue every later
//! request behind it, `/health` included (#99): the Docker `HEALTHCHECK` then
//! restarted a server that was merely busy. The limits reject with 503 instead
//! of queueing, cut a request at `REQUEST_TIMEOUT` with 504, and do not apply
//! to `/health` nor to the MCP transport at `/mcp` (whose SSE streams must
//! outlive any timeout).

use axum::error_handling::HandleErrorLayer;
use axum::http::StatusCode;
use axum::{BoxError, Json, Router};
use std::time::Duration;
use tower::ServiceBuilder;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower::load_shed::LoadShedLayer;
use tower::timeout::TimeoutLayer;

use super::types::ErrorResponse;

/// Limits applied to the REST query endpoints.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestLimits {
    /// Requests in flight at once across all query endpoints; the next one is
    /// rejected with 503 instead of waiting for a slot.
    pub(crate) max_concurrent: usize,
    /// A handler still running after this long is dropped and the client gets
    /// 504. The blocking SQLite work it spawned finishes on its own.
    pub(crate) timeout: Duration,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            max_concurrent: 50,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Wrap `router` (the query endpoints) in the limits. The concurrency cap is
/// global: one semaphore shared by all routes, not one per route.
pub(super) fn limit_requests(router: Router, limits: RequestLimits) -> Router {
    router.layer(
        ServiceBuilder::new()
            .layer(HandleErrorLayer::new(limit_error))
            .layer(LoadShedLayer::new())
            .layer(GlobalConcurrencyLimitLayer::new(limits.max_concurrent))
            .layer(TimeoutLayer::new(limits.timeout)),
    )
}

async fn limit_error(err: BoxError) -> (StatusCode, Json<ErrorResponse>) {
    if err.is::<tower::load_shed::error::Overloaded>() {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "Too many requests in flight; retry shortly".to_string(),
                code: "OVERLOADED".to_string(),
            }),
        )
    } else if err.is::<tower::timeout::error::Elapsed>() {
        (
            StatusCode::GATEWAY_TIMEOUT,
            Json(ErrorResponse {
                error: "Request timed out".to_string(),
                code: "TIMEOUT".to_string(),
            }),
        )
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Internal error: {err}"),
                code: "INTERNAL_ERROR".to_string(),
            }),
        )
    }
}
