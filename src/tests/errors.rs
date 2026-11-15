use crate::test_support::{HttpResponse, app_state, request, request_with_headers, spawn_server};

fn json(response: &HttpResponse) -> serde_json::Value {
    serde_json::from_str(&response.body).expect("REST error JSON")
}

fn assert_error(response: &HttpResponse, status: u16, reason: &str, path: &str) {
    assert_eq!(response.status, status);
    assert_eq!(response.header("content-type"), Some("application/json"));
    assert_eq!(response.header("lavalink-api-version"), Some("4"));
    let value = json(response);
    assert!(value["timestamp"].is_u64());
    assert_eq!(value["status"], status);
    assert_eq!(value["error"], reason);
    assert!(value["message"].as_str().is_some_and(|message| !message.is_empty()));
    assert_eq!(value["path"], path);
}

#[tokio::test]
async fn auth_fallback_and_method_errors_use_the_v4_shape() {
    let address = spawn_server(app_state(Some("letmein"))).await;

    let unauthorized = request(address, "GET", "/v4/info", None, None).await;
    assert_error(&unauthorized, 401, "Unauthorized", "/v4/info");
    assert!(json(&unauthorized).get("trace").is_none());

    let missing =
        request(address, "GET", "/v4/not-a-route?trace=true", Some("letmein"), None).await;
    assert_error(&missing, 404, "Not Found", "/v4/not-a-route");
    assert!(json(&missing)["trace"].as_str().is_some_and(|trace| !trace.is_empty()));

    let method = request(address, "POST", "/v4/info", Some("letmein"), None).await;
    assert_error(&method, 405, "Method Not Allowed", "/v4/info");
}

#[tokio::test]
async fn extractor_and_handler_errors_use_the_v4_shape() {
    let state = app_state(Some("letmein"));
    state.sessions.reserve_for("session", 1);
    let address = spawn_server(state).await;

    let malformed = request(
        address,
        "PATCH",
        "/v4/sessions/session",
        Some("letmein"),
        Some(r#"{"timeout":"later"}"#),
    )
    .await;
    assert_error(&malformed, 400, "Bad Request", "/v4/sessions/session");

    let disabled =
        request(address, "POST", "/v4/routeplanner/free/all?trace=true", Some("letmein"), None)
            .await;
    assert_error(&disabled, 500, "Internal Server Error", "/v4/routeplanner/free/all");
    let value = json(&disabled);
    assert_eq!(value["message"], "Can't access disabled route planner");
    assert!(value["trace"].is_string());

    let no_content =
        request(address, "GET", "/v4/routeplanner/status", Some("letmein"), None).await;
    assert_eq!(no_content.status, 204);
    assert!(no_content.body.is_empty());
}

#[tokio::test]
async fn framework_rejections_use_canonical_reasons() {
    let state = app_state(Some("letmein"));
    state.sessions.reserve_for("session", 1);
    let address = spawn_server(state).await;

    let cases: &[(&str, &str, Option<&str>, u16, &str)] = &[
        ("PATCH", "/v4/sessions/session", Some(r#"{"timeout":"later"}"#), 400, "Bad Request"),
        ("GET", "/v4/not-a-route", None, 404, "Not Found"),
        ("POST", "/v4/info", None, 405, "Method Not Allowed"),
        ("GET", "/v4/loadtracks", None, 400, "Bad Request"),
        ("GET", "/v4/sessions/session/players/not-a-number", None, 400, "Bad Request"),
    ];

    for (method, path, body, status, reason) in cases {
        let response = request(address, method, path, Some("letmein"), *body).await;
        assert_error(&response, *status, reason, path);
        assert_eq!(json(&response)["message"], *reason, "{method} {path}");
    }

    let wrong_type = request_with_headers(
        address,
        "POST",
        "/v4/decodetracks",
        Some("letmein"),
        Some("[]"),
        &[("Content-Type", "text/plain")],
    )
    .await;
    assert_error(&wrong_type, 415, "Unsupported Media Type", "/v4/decodetracks");
    assert_eq!(json(&wrong_type)["message"], "Unsupported Media Type");
}
