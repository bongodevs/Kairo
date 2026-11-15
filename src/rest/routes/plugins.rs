use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::app::AppState;
use crate::plugins::{PluginEntry, ReloadError};
use crate::rest::extract::ApiPath;

pub(crate) async fn list(State(state): State<AppState>) -> Json<Vec<PluginEntry>> {
    Json(state.plugins().listing())
}

pub(crate) async fn reload(
    State(state): State<AppState>,
    ApiPath(name): ApiPath<String>,
) -> Response {
    match state.plugins().reload(&name).await {
        Ok(entry) => Json(entry).into_response(),
        Err(error) => reload_error(error).into_response(),
    }
}

fn reload_error(error: ReloadError) -> Response {
    let status = match error {
        ReloadError::Unknown { .. } => StatusCode::NOT_FOUND,
        ReloadError::Quarantined { .. } | ReloadError::Unavailable { .. } => StatusCode::CONFLICT,
        ReloadError::Failed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}
