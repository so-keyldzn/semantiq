//! Request limits of the REST query endpoints: a concurrency cap with load
//! shedding and a timeout.
//!
//! Every handler ends up on the single SQLite connection, so a burst of slow
//! `/search` or `/dead-code` calls during a reindex used to queue every later
//! request behind it, `/health` included (#99): the Docker `HEALTHCHECK` then
//! restarted a server that was merely busy. The limits reject with 503 instead
//! of queueing, answer 504 after `RequestLimits::timeout`, and do not apply
//! to `/health` nor to the MCP transport at `/mcp` (whose SSE streams must
//! outlive any timeout).
//!
//! A timed-out handler is not cancelled: its SQLite work runs on the blocking
//! pool and cannot be interrupted, so the handler keeps running on its own
//! task and holds its slot until it finishes. The cap therefore bounds the
//! blocking jobs in flight, not only the clients still waiting.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::warn;

use super::types::ErrorResponse;

/// Limits applied to the REST query endpoints.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RequestLimits {
    /// Requests in flight at once across all query endpoints; the next one is
    /// rejected with 503 instead of waiting for a slot.
    pub(crate) max_concurrent: usize,
    /// A handler still running after this long gets its client a 504; the
    /// handler itself runs to completion and keeps its slot until then.
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

#[derive(Clone)]
struct LimitState {
    slots: Arc<Semaphore>,
    timeout: Duration,
}

/// Wrap `router` (the query endpoints) in the limits. The concurrency cap is
/// global: one semaphore shared by all routes, not one per route.
pub(super) fn limit_requests(router: Router, limits: RequestLimits) -> Router {
    let state = LimitState {
        slots: Arc::new(Semaphore::new(limits.max_concurrent)),
        timeout: limits.timeout,
    };
    router.layer(middleware::from_fn_with_state(state, limit_request))
}

async fn limit_request(State(state): State<LimitState>, request: Request, next: Next) -> Response {
    let Ok(permit) = Arc::clone(&state.slots).try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OVERLOADED",
            "Too many requests in flight; retry shortly",
        );
    };
    let method = request.method().clone();
    let path = request.uri().path().to_owned();

    // The handler owns the slot: dropping this task's `JoinHandle` on timeout
    // does not cancel it, so the permit is released only when the work ends.
    let handler = tokio::spawn(async move {
        let response = next.run(request).await;
        drop(permit);
        response
    });

    match tokio::time::timeout(state.timeout, handler).await {
        Ok(Ok(response)) => response,
        Ok(Err(join_error)) => {
            warn!(%method, path, "query handler failed: {join_error}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Internal error",
            )
        }
        Err(_elapsed) => {
            warn!(
                %method,
                path,
                "query handler still running after {:?}: client answered 504, slot kept until it ends",
                state.timeout
            );
            error(StatusCode::GATEWAY_TIMEOUT, "TIMEOUT", "Request timed out")
        }
    }
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.to_string(),
            code: code.to_string(),
        }),
    )
        .into_response()
}
