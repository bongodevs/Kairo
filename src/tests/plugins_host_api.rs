use std::sync::mpsc;

use kairo_plugin_api::HostCallbacks;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

fn server(response: Vec<u8>) -> (tokio::runtime::Runtime, String, mpsc::Receiver<Vec<u8>>) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).expect("bind");
    let address = listener.local_addr().expect("address");
    let (sent, received) = mpsc::channel();
    runtime.spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = vec![0; 4096];
        let read = stream.read(&mut request).await.expect("read request");
        request.truncate(read);
        sent.send(request).expect("record request");
        stream.write_all(&response).await.expect("write response");
    });
    (runtime, format!("http://{address}/item"), received)
}

fn host() -> PluginHost {
    PluginHost::new("demo".into(), client().expect("client"))
}

#[test]
fn host_http_returns_bounded_utf8_response() {
    let (runtime, url, request) = server(
        b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nX-Demo: yes\r\nConnection: close\r\n\r\n{\"x\":1}"
            .to_vec(),
    );
    let response = runtime
        .block_on(host().http(HttpRequest {
            method: HttpMethod::Post,
            url,
            headers: [("X-Request".into(), "value".into())].into(),
            body: Some("hello".into()),
        }))
        .expect("HTTP response");

    assert_eq!(response.status, 200);
    assert_eq!(response.headers["x-demo"], "yes");
    assert_eq!(response.body, "{\"x\":1}");
    let request = String::from_utf8(request.recv().expect("request")).expect("UTF-8 request");
    assert!(request.starts_with("POST /item HTTP/1.1"));
    assert!(request.to_ascii_lowercase().contains("x-request: value"));
    assert!(request.ends_with("hello"));
}

#[test]
fn host_http_rejects_large_declared_body_before_reading() {
    let length = MAX_HTTP_BODY + 1;
    let response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
    let (runtime, url, _request) = server(response.into_bytes());
    let error = runtime
        .block_on(host().http(HttpRequest {
            method: HttpMethod::Get,
            url,
            headers: Default::default(),
            body: None,
        }))
        .expect_err("large body");

    assert_eq!(error.severity, kairo_plugin_api::Severity::Fault);
    assert!(error.message.contains("exceeds"));
}

#[test]
fn host_http_rejects_non_http_scheme_without_a_request() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let error = runtime
        .block_on(host().http(HttpRequest {
            method: HttpMethod::Get,
            url: "file:///etc/passwd".into(),
            headers: Default::default(),
            body: None,
        }))
        .expect_err("scheme");

    assert_eq!(error.severity, kairo_plugin_api::Severity::Common);
}

#[test]
fn host_http_rejects_redirect_to_another_scheme() {
    let (runtime, url, _request) = server(
        b"HTTP/1.1 302 Found\r\nLocation: https://example.test/item\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    );
    let error = runtime
        .block_on(host().http(HttpRequest {
            method: HttpMethod::Get,
            url,
            headers: Default::default(),
            body: None,
        }))
        .expect_err("scheme-changing redirect");

    assert!(error.unavailable);
}
