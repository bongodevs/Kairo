//! The two REST contracts: the disabled planner and a real planner built from config.

use std::sync::Arc;

use super::*;
use crate::config::RateLimitConfig;
use crate::route_planner::RoutePlanner;
use crate::test_support::{HttpResponse, app_state, request, spawn_server};

fn json_response(response: &HttpResponse) -> serde_json::Value {
    serde_json::from_str(&response.body).expect("JSON response")
}

const AUTH: &str = "letmein";

fn enabled_state(blocks: &[&str]) -> AppState {
    let config = RateLimitConfig {
        ip_blocks: blocks.iter().map(|block| (*block).to_owned()).collect(),
        excluded_ips: Vec::new(),
        strategy: String::from("RotateOnBan"),
        retry_limit: -1,
        search_triggers_fail: true,
    };
    let planner = RoutePlanner::build(Some(&config)).expect("config").expect("planner");
    let mut state = app_state(Some(AUTH));
    state.route_planner = Some(Arc::new(planner));
    state
}

#[tokio::test]
async fn status_is_no_content_when_no_planner_exists() {
    assert_eq!(status(State(app_state(Some(AUTH)))).await.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn freeing_addresses_reports_the_disabled_planner() {
    let state = app_state(Some(AUTH));
    let request_body = serde_json::from_value(serde_json::json!({"address": "127.0.0.1"}))
        .expect("free-address body");
    let response = free_address(State(state.clone()), ApiJson(request_body)).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = axum::body::to_bytes(response.into_body(), 1024).await.expect("response body");
    assert_eq!(&body[..], DISABLED.as_bytes());

    assert_eq!(free_all(State(state)).await.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn free_address_requires_the_documented_string_field() {
    assert!(serde_json::from_value::<FreeAddress>(serde_json::json!({})).is_err());
    assert!(serde_json::from_value::<FreeAddress>(serde_json::json!({"address": 1})).is_err());
}

#[tokio::test]
async fn an_enabled_planner_answers_status_and_free_like_the_reference() {
    let address = spawn_server(enabled_state(&["10.0.0.0/30"])).await;

    let response = request(address, "GET", "/v4/routeplanner/status", Some(AUTH), None).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("application/json"));
    let status = json_response(&response);
    assert_eq!(status["class"], "RotatingIpRoutePlanner");
    assert_eq!(status["details"]["ipBlock"]["size"], "4");
    assert_eq!(status["details"]["ipIndex"], "0");
    assert_eq!(status["details"]["currentAddress"], "null");

    let freed = request(
        address,
        "POST",
        "/v4/routeplanner/free/address",
        Some(AUTH),
        Some(r#"{"address":"10.0.0.1"}"#),
    )
    .await;
    assert_eq!(freed.status, 204);
    assert!(freed.body.is_empty());

    let invalid = request(
        address,
        "POST",
        "/v4/routeplanner/free/address",
        Some(AUTH),
        Some(r#"{"address":"this is not an address"}"#),
    )
    .await;
    assert_eq!(invalid.status, 400);
    assert_eq!(json_response(&invalid)["message"], "Invalid address: this is not an address");

    let all = request(address, "POST", "/v4/routeplanner/free/all", Some(AUTH), None).await;
    assert_eq!(all.status, 204);
    assert!(all.body.is_empty());
}
