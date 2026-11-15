use std::net::IpAddr;
use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::app::AppState;
use crate::rest::extract::ApiJson;

const DISABLED: &str = "Can't access disabled route planner";

#[derive(Debug, Deserialize)]
pub(crate) struct FreeAddress {
    address: String,
}

pub(crate) async fn status(State(state): State<AppState>) -> Response {
    match &state.route_planner {
        Some(planner) => Json(planner.status()).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

pub(crate) async fn free_address(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<FreeAddress>,
) -> Response {
    let Some(planner) = &state.route_planner else {
        return disabled();
    };
    match resolve(&request.address).await {
        Ok(address) => {
            planner.free_address(address);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(()) => (StatusCode::BAD_REQUEST, format!("Invalid address: {}", request.address))
            .into_response(),
    }
}

pub(crate) async fn free_all(State(state): State<AppState>) -> Response {
    match &state.route_planner {
        Some(planner) => {
            planner.free_all();
            StatusCode::NO_CONTENT.into_response()
        }
        None => disabled(),
    }
}

/// `InetAddress.getByName`: an IP literal directly, otherwise the first resolved address.
async fn resolve(text: &str) -> Result<IpAddr, ()> {
    let text = text.trim();
    if let Ok(address) = IpAddr::from_str(text) {
        return Ok(address);
    }
    tokio::net::lookup_host((text, 0))
        .await
        .map_err(|_| ())?
        .next()
        .map(|address| address.ip())
        .ok_or(())
}

fn disabled() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, DISABLED).into_response()
}

#[cfg(test)]
#[path = "../../tests/route_planner_routes.rs"]
mod tests;
