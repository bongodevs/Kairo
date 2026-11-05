//! The `/v4/routeplanner/*` endpoints.
//!
//! When `lavalink.server.ratelimit` is configured, an [`IpRoutePlanner`](crate::routeplanner)
//! rotates the outbound source address. These endpoints expose its status and let a client clear
//! failing addresses. Without a route planner, `status` answers `204 No Content` and the `free`
//! endpoints answer `500`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::node::AppState;
use crate::rest::error::{RestError, RestResult};
use crate::rest::json::JsonBody;

/// `GET /v4/routeplanner/status`, the planner status, or `204 No Content` when unconfigured.
pub async fn status(State(state): State<AppState>) -> Response {
    match state.route_planner() {
        Some(planner) => Json(json!(planner.status())).into_response(),
        // `204` rather than a body with a null class, which a client parsing unconditionally would
        // choke on.
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// Body of `POST /v4/routeplanner/free/address`.
#[derive(Debug, Deserialize)]
pub(crate) struct FreeAddressRequest {
    pub(crate) address: String,
}

/// `POST /v4/routeplanner/free/address`, un-mark a single failing address.
pub(crate) async fn free_address(
    State(state): State<AppState>,
    JsonBody(request): JsonBody<FreeAddressRequest>,
) -> RestResult<StatusCode> {
    let planner = state.route_planner().ok_or_else(disabled)?;
    // An unparseable address is a `400`, not a silent no-op reported as success.
    let address = request
        .address
        .trim()
        .parse()
        .map_err(|_| RestError::bad_request(format!("Invalid address: {}", request.address)))?;
    planner.free_address(&address);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v4/routeplanner/free/all`, un-mark every failing address.
pub async fn free_all(State(state): State<AppState>) -> RestResult<StatusCode> {
    let planner = state.route_planner().ok_or_else(disabled)?;
    planner.free_all();
    Ok(StatusCode::NO_CONTENT)
}

fn disabled() -> RestError {
    RestError::internal("Can't access disabled route planner: no route planner is configured")
}
