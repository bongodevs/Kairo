//! The HTTP API: the router, its auth middleware, and one module per group of endpoints.
//!
//! Every route except the Prometheus endpoint requires the configured password in the
//! `Authorization` header. Failures are rendered as the v4 JSON error body by
//! [`error_body_middleware`](error::error_body_middleware).

pub mod error;
pub mod error_detail;
pub mod info;
pub mod json;
pub mod lyrics;
pub mod metrics;
pub mod players;
pub mod request_log;
pub mod routeplanner;
pub mod sessions;
pub mod sponsorblock;
pub mod track_loading;
pub mod ws;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::Router;

use crate::node::AppState;
use crate::rest::error::{error_body_middleware, RestError};

// Shared by the route and by `auth`, which answers a wrong password differently there.
const WEBSOCKET_PATH: &str = "/v4/websocket";

/// Build the REST and WebSocket application.
pub fn app(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/version", get(info::version))
        .route("/", get(ws::legacy_websocket))
        .route("/v3/websocket", get(ws::legacy_websocket))
        .route("/v4/info", get(info::info))
        .route("/v4/stats", get(info::stats))
        .route("/v4/loadtracks", get(track_loading::load_tracks))
        .route("/v4/loadsearch", get(track_loading::load_search))
        .route("/v4/decodetrack", get(track_loading::decode_track))
        .route("/v4/decodetracks", post(track_loading::decode_tracks))
        .route("/v4/sessions/{sessionId}", patch(sessions::patch_session))
        .route(
            "/v4/sessions/{sessionId}/players",
            get(players::get_players),
        )
        .route(
            "/v4/sessions/{sessionId}/players/{guildId}",
            get(players::get_player)
                .patch(players::patch_player)
                .delete(players::delete_player),
        )
        .route("/v4/routeplanner/status", get(routeplanner::status))
        .route(
            "/v4/routeplanner/free/address",
            post(routeplanner::free_address),
        )
        .route("/v4/routeplanner/free/all", post(routeplanner::free_all))
        .route(
            "/v4/sessions/{sessionId}/players/{guildId}/sponsorblock/categories",
            get(sponsorblock::get_categories)
                .put(sponsorblock::set_categories)
                .delete(sponsorblock::delete_categories),
        )
        .route(WEBSOCKET_PATH, get(ws::websocket_handler));

    // Mounted only when lyrics are enabled, so a node without them answers `404` here instead of an
    // auth-gated `503`.
    if state.lyrics_service().is_some() {
        router = router
            .route("/v4/lyrics", get(lyrics::get_lyrics))
            .route(
                "/v4/sessions/{sessionId}/players/{guildId}/track/lyrics",
                get(lyrics::get_player_lyrics),
            )
            .route(
                "/v4/sessions/{sessionId}/players/{guildId}/lyrics/subscribe",
                post(lyrics::subscribe).delete(lyrics::unsubscribe),
            );
    }

    let prometheus = &state.config().metrics.prometheus;
    if prometheus.enabled {
        router = router.route(&prometheus.endpoint, get(metrics::metrics));
    }

    router
        // Innermost is auth, then the error body, so it also formats the `401`, `403`, `404` and
        // `405` the layers below produce, then the version header. Request logging goes outermost so
        // it sees every request, including the ones auth rejects.
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(middleware::from_fn(error_body_middleware))
        .layer(middleware::from_fn(api_version_header))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_log::request_logging,
        ))
        .with_state(state)
}

// Tag every response with the protocol version the client is talking to.
async fn api_version_header(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("Lavalink-Api-Version", HeaderValue::from_static("4"));
    response
}

// Require the configured password on every route: `401` when the header is absent, `403` when it is
// wrong. The WebSocket upgrade answers `401` for both, since a client that cannot authenticate has no
// session to be told about.
async fn auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    // Scraping is anonymous so a metrics collector needs no credentials. Nothing else is exempt,
    // `/version` included.
    let metrics = &state.config().metrics.prometheus;
    if metrics.enabled && path == metrics.endpoint {
        return next.run(request).await;
    }
    let wrong_password_status = if path == WEBSOCKET_PATH {
        StatusCode::UNAUTHORIZED
    } else {
        StatusCode::FORBIDDEN
    };

    let password = &state.config().lavalink.server.password;
    match request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
    {
        None => RestError::unauthorized("Authorization header is missing").into_response(),
        Some(provided) if provided == password => next.run(request).await,
        Some(_) => {
            RestError::new(wrong_password_status, "Authorization header is invalid").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn unauthenticated_request_is_rejected() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v4/info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers().get("Lavalink-Api-Version").unwrap(), "4");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 401);
        assert_eq!(json["error"], "Unauthorized");
    }

    #[tokio::test]
    async fn invalid_password_returns_forbidden() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v4/info")
                    .header("Authorization", "wrongpassword")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 403);
    }

    #[tokio::test]
    async fn authenticated_info_returns_ok() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v4/info")
                    .header("Authorization", "youshallnotpass")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("Lavalink-Api-Version").unwrap(), "4");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["version"]["semver"].is_string());
    }

    #[tokio::test]
    async fn authenticated_stats_returns_ok() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v4/stats")
                    .header("Authorization", "youshallnotpass")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["players"].is_number());
        assert!(json["playingPlayers"].is_number());
        assert!(json["memory"].is_object());
        assert!(json["cpu"].is_object());
    }

    #[tokio::test]
    async fn prometheus_metrics_is_anonymous() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn legacy_websocket_routes_answer_ok_with_an_upgrade_warning() {
        for uri in ["/", "/v3/websocket"] {
            let app = app(AppState::test_state());
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("Authorization", "youshallnotpass")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert!(
                String::from_utf8_lossy(&body).contains("/v4/websocket"),
                "{uri} must point at the v4 endpoint"
            );
        }
    }

    // The reference authenticates the legacy probes too; only metrics is anonymous.
    #[tokio::test]
    async fn the_legacy_websocket_route_still_needs_credentials() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // Axum rejects a typed-but-wrong JSON body with 422; the v4 protocol wants 400 and a message
    // that names the field.
    #[tokio::test]
    async fn a_wrong_typed_json_body_is_a_bad_request() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/v4/sessions/s/players/1")
                    .header("Authorization", "youshallnotpass")
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"volume":"x"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 400);
        assert!(
            json["message"].as_str().unwrap().contains("volume"),
            "the message should name the field: {json}"
        );
    }

    // Spring's `ON_PARAM` treats a valueless `?trace` as present; the reference includes the trace
    // then too.
    #[tokio::test]
    async fn a_valueless_trace_query_includes_the_trace() {
        use axum::middleware;
        use axum::routing::get;

        async fn fail() -> RestError {
            RestError {
                status: StatusCode::BAD_REQUEST,
                message: "bad request".to_string(),
                trace: Some("the cause".to_string()),
            }
        }

        let app = Router::new()
            .route("/fail", get(fail))
            .layer(middleware::from_fn(error_body_middleware));

        for uri in ["/fail?trace", "/fail?trace=true", "/fail?trace=false"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["trace"], "the cause", "{uri}");
        }

        let response = app
            .oneshot(Request::builder().uri("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.get("trace").is_none());
    }

    #[tokio::test]
    async fn missing_route_returns_formatted_not_found() {
        let app = app(AppState::test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v4/nonexistent")
                    .header("Authorization", "youshallnotpass")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 404);
        assert_eq!(json["error"], "Not Found");
    }
}
