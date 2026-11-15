use axum::Json;
use axum::extract::State;

use crate::app::AppState;
use crate::stats::{Stats, snapshot};

pub(crate) async fn stats(State(state): State<AppState>) -> Json<Stats> {
    Json(snapshot(&state).await)
}
