use std::io::SeekFrom;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;
use crate::source::{OpenedSource, Source, SourceError, SourceReference, SourceResult, Sources};
async fn server(responses: Vec<&'static str>) -> (String, JoinHandle<Vec<Vec<u8>>>) {
    server_owned(responses.into_iter().map(str::to_owned).collect()).await
}

async fn server_owned(responses: Vec<String>) -> (String, JoinHandle<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local address");
    let task = tokio::spawn(async move {
        let mut requests = Vec::with_capacity(responses.len());
        for response in responses {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0u8; 2048];
            let length = socket.read(&mut request).await.expect("read request");
            assert!(request[..length].windows(4).any(|part| part == b"\r\n\r\n"));
            requests.push(request[..length].to_vec());
            socket.write_all(response.as_bytes()).await.expect("write response");
            socket.shutdown().await.expect("shutdown response");
        }
        requests
    });
    (format!("{address}"), task)
}

async fn read_exact(source: &mut OpenedSource, output: &mut [u8]) {
    let mut offset = 0;
    while offset < output.len() {
        let read = source.read(&mut output[offset..]).await.expect("read body");
        assert!(read > 0, "body ended before the expected bytes");
        offset += read;
    }
}

async fn read_all(mut source: OpenedSource) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 3];
    loop {
        let length = source.read(&mut buffer).await.expect("read body");
        if length == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..length]);
    }
}

#[tokio::test]
async fn http_stream_is_pull_driven_and_keeps_response_hints() {
    let (address, server) = server(vec![
        "HTTP/1.1 200 OK\r\nContent-Type: audio/ogg\r\nContent-Length: 7\r\nIcy-Description: header title\r\nIcy-Name: station\r\nConnection: close\r\n\r\npayload",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let reference = SourceReference::with_title(format!("icy://{address}/audio"), "radio");

    let SourceResult::Opened(opened) = source.load(&reference).await.expect("open") else {
        panic!("expected opened source");
    };

    assert_eq!(opened.reference().identifier(), format!("http://{address}/audio"));
    assert_eq!(opened.reference().title(), Some("radio"));
    assert_eq!(opened.hints().mime_type.as_deref(), Some("audio/ogg"));
    assert_eq!(opened.metadata().title.as_deref(), Some("radio"));
    assert_eq!(opened.metadata().author.as_deref(), Some("station"));
    assert_eq!(opened.content_length(), Some(7));
    assert!(opened.is_seekable());
    assert_eq!(read_all(opened).await, b"payload");
    let _ = server.await.expect("server task");
}

#[tokio::test]
async fn relative_redirects_restart_the_registry() {
    let (address, server) = server(vec![
        "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone",
    ])
    .await;
    let mut sources = Sources::new();
    sources.register(HttpSource::new().expect("client")).expect("register");

    let opened = sources
        .open(SourceReference::new(format!("http://{address}/start")))
        .await
        .expect("resolve")
        .expect("opened");

    assert_eq!(opened.reference().identifier(), format!("http://{address}/final"));
    assert_eq!(read_all(opened).await, b"done");
    let _ = server.await.expect("server task");
}

#[tokio::test]
async fn direct_open_follows_redirects_and_reuses_headers() {
    let (address, server) = server(vec![
        "HTTP/1.1 308 Permanent Redirect\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let opened = source
        .open_with_headers(
            SourceReference::new(format!("http://{address}/start")),
            &[(String::from("X-Plugin"), String::from("demo"))],
        )
        .await
        .expect("redirect")
        .expect("opened");

    assert_eq!(opened.reference().identifier(), format!("http://{address}/final"));
    assert_eq!(read_all(opened).await, b"done");
    let requests = server.await.expect("server task");
    for request in requests {
        let request = String::from_utf8(request).expect("request UTF-8").to_ascii_lowercase();
        assert!(request.contains("x-plugin: demo"));
    }
}

#[tokio::test]
async fn not_found_is_no_match_but_other_empty_success_is_an_error() {
    let (address, server) = server(vec![
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
    ])
    .await;
    let source = HttpSource::new().expect("client");

    let missing = source
        .load(&SourceReference::new(format!("http://{address}/missing")))
        .await
        .expect("404 result");
    assert!(matches!(missing, SourceResult::NotMine));

    let error = source
        .load(&SourceReference::new(format!("http://{address}/empty")))
        .await
        .expect_err("204 rejected");
    assert!(matches!(error, SourceError::UnplayableHttp { status: 204, .. }));
    let _ = server.await.expect("server task");
}

#[tokio::test]
async fn unrelated_identifiers_make_no_request() {
    let source = HttpSource::new().expect("client");
    let result = source.load(&SourceReference::new("relative/file.ogg")).await.expect("result");
    assert!(matches!(result, SourceResult::NotMine));
}

#[tokio::test]
async fn unknown_length_http_remains_forward_only() {
    let (address, server) = server(vec![
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\npayload\r\n0\r\n\r\n",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(opened) =
        source.load(&SourceReference::new(format!("http://{address}/live"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    assert!(!opened.is_seekable());
    assert_eq!(read_all(opened).await, b"payload");
    let _ = server.await.expect("server task");
}

#[tokio::test]
async fn short_forward_seek_discards_from_the_current_body() {
    let total = 2_000;
    let body: String = (0..total).map(|index| char::from(b'0' + (index % 10) as u8)).collect();
    let (address, server) = server_owned(vec![format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n{body}"
    )])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    let mut first = [0; 2];
    read_exact(&mut opened, &mut first).await;
    assert_eq!(&first, b"01");
    assert_eq!(opened.seek(SeekFrom::Start(1_500)).await.expect("seek"), 1_500);
    let mut after = [0; 3];
    read_exact(&mut opened, &mut after).await;
    assert_eq!(&after, b"012");

    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 1, "short seek should not reopen HTTP");
}

#[tokio::test]
async fn backward_seek_within_cached_bytes_does_not_reopen() {
    let (address, server) = server(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    let mut first = [0; 4];
    read_exact(&mut opened, &mut first).await;
    assert_eq!(opened.seek(SeekFrom::Start(1)).await.expect("seek"), 1);
    let mut after = [0; 3];
    read_exact(&mut opened, &mut after).await;
    assert_eq!(&after, b"123");

    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 1, "cached backward seek should not reopen HTTP");
}

#[tokio::test]
async fn re_reading_a_late_range_uses_the_cache_instead_of_the_body() {
    let total = 1_200_000;
    let range_start = 900_000;
    let (address, server) = server_owned(vec![
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n{}",
            "a".repeat(2_000)
        ),
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 1000\r\nContent-Range: bytes {range_start}-{}/{total}\r\nConnection: close\r\n\r\n{}",
            range_start + 999,
            "m".repeat(1000)
        ),
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };

    assert_eq!(opened.seek(SeekFrom::Start(range_start)).await.expect("seek"), range_start);
    let mut late = vec![0; 1_000];
    read_exact(&mut opened, &mut late).await;
    assert_eq!(late, vec![b'm'; 1_000]);

    assert_eq!(opened.seek(SeekFrom::Start(0)).await.expect("seek"), 0);
    let mut head = vec![0; 1_000];
    read_exact(&mut opened, &mut head).await;
    assert_eq!(head, vec![b'a'; 1_000]);

    assert_eq!(opened.seek(SeekFrom::Start(range_start)).await.expect("seek"), range_start);
    read_exact(&mut opened, &mut late).await;
    assert_eq!(late, vec![b'm'; 1_000]);

    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 2, "the second range read should come from the cache");
}

#[tokio::test]
async fn backward_seek_outside_the_cache_reopens_with_a_range_request() {
    let total = super::stream::RANGE_CACHE_BYTES + 4_096;
    let body = "x".repeat(total);
    let (address, server) = server_owned(vec![
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n{body}"
        ),
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 1-3/{total}\r\nConnection: close\r\n\r\nabc"
        ),
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    let mut chunk = vec![0; super::stream::RANGE_CACHE_BYTES + 2_048];
    read_exact(&mut opened, &mut chunk).await;
    assert_eq!(opened.seek(SeekFrom::Start(1)).await.expect("seek"), 1);
    let mut after = [0; 3];
    read_exact(&mut opened, &mut after).await;
    assert_eq!(&after, b"abc");

    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 2);
    assert!(
        String::from_utf8_lossy(&requests[1]).contains("range: bytes=1-"),
        "{}",
        String::from_utf8_lossy(&requests[1])
    );
}

#[tokio::test]
async fn large_forward_seek_reopens_instead_of_discarding() {
    let target = 512 * 1024 + 1;
    let ranged = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes {target}-{target}/600000\r\nConnection: close\r\n\r\nx"
    );
    let (address, server) = server_owned(vec![
        String::from("HTTP/1.1 200 OK\r\nContent-Length: 600000\r\nConnection: close\r\n\r\na"),
        ranged,
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    assert_eq!(opened.seek(SeekFrom::Start(target)).await.expect("seek"), target);
    let mut byte = [0];
    read_exact(&mut opened, &mut byte).await;
    assert_eq!(&byte, b"x");

    let requests = server.await.expect("server task");
    assert!(String::from_utf8_lossy(&requests[1]).contains(&format!("range: bytes={target}-")));
}

#[tokio::test]
async fn interrupted_body_reconnects_at_the_current_position() {
    let (address, server) = server(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc",
        "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/6\r\nConnection: close\r\n\r\ndef",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    assert_eq!(read_all(opened).await, b"abcdef");
    let requests = server.await.expect("server task");
    assert!(String::from_utf8_lossy(&requests[1]).contains("range: bytes=3-"));
}

#[tokio::test]
async fn server_error_during_reopen_is_retried_once() {
    let total = 600_000;
    let target = 512 * 1024 + 100;
    let (address, server) = server_owned(vec![
        format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\nabc"),
        String::from("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
        format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes {target}-{}/{total}\r\nConnection: close\r\n\r\nxyz",
            target + 2
        ),
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    let mut first = [0; 3];
    read_exact(&mut opened, &mut first).await;
    assert_eq!(opened.seek(SeekFrom::Start(target)).await.expect("seek"), target);
    let mut after = [0; 3];
    read_exact(&mut opened, &mut after).await;
    assert_eq!(&after, b"xyz");

    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 3);
}

#[tokio::test]
async fn consecutive_dropped_bodies_reconnect_within_the_budget() {
    let (address, server) = server(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabc",
        "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 3-5/9\r\nConnection: close\r\n\r\ndef",
        "HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 6-8/9\r\nConnection: close\r\n\r\nghi",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    assert_eq!(read_all(opened).await, b"abcdefghi");
    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 3);
    assert!(String::from_utf8_lossy(&requests[1]).contains("range: bytes=3-"));
    assert!(String::from_utf8_lossy(&requests[2]).contains("range: bytes=6-"));
}

#[tokio::test]
async fn reconnect_budget_ends_the_stream_with_a_typed_error() {
    let partial = "HTTP/1.1 206 Partial Content\r\nContent-Length: 0\r\nContent-Range: bytes 3-8/9\r\nConnection: close\r\n\r\n";
    let mut responses =
        vec![String::from("HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabc")];
    responses.extend(std::iter::repeat_n(partial.to_owned(), 4));
    let (address, server) = server_owned(responses).await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };

    let mut output = Vec::new();
    let error = loop {
        let mut buffer = [0; 3];
        match opened.read(&mut buffer).await {
            Ok(0) => panic!("stream ended without an error"),
            Ok(length) => output.extend_from_slice(&buffer[..length]),
            Err(error) => break error,
        }
    };

    assert_eq!(output, b"abc");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 5);
}

#[tokio::test]
async fn client_error_during_reopen_is_not_retried() {
    let (address, server) = server(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabc",
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ])
    .await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(mut opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    let mut buffer = [0; 3];
    let error = loop {
        match opened.read(&mut buffer).await {
            Ok(0) => panic!("stream ended without an error"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert!(error.to_string().contains("404"));
    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn reconnect_budget_resets_after_forward_progress() {
    const CHUNK: usize = 8 * 1024 + 1;
    const TOTAL: usize = 3 + CHUNK * 5 + 10;
    let mut responses =
        vec![format!("HTTP/1.1 200 OK\r\nContent-Length: {TOTAL}\r\nConnection: close\r\n\r\nabc")];
    let mut start = 3;
    for _ in 0..5 {
        let end = start + CHUNK - 1;
        responses.push(format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {CHUNK}\r\nContent-Range: bytes {start}-{end}/{TOTAL}\r\nConnection: close\r\n\r\n{}",
            "x".repeat(CHUNK)
        ));
        start += CHUNK;
    }
    responses.push(format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: 10\r\nContent-Range: bytes {start}-{}/{TOTAL}\r\nConnection: close\r\n\r\n0123456789",
        TOTAL - 1
    ));

    let (address, server) = server_owned(responses).await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(opened) =
        source.load(&SourceReference::new(format!("http://{address}/audio"))).await.expect("open")
    else {
        panic!("expected opened source");
    };
    let bytes = read_all(opened).await;
    assert_eq!(bytes.len(), TOTAL);
    assert_eq!(&bytes[..3], b"abc");
    assert_eq!(&bytes[TOTAL - 10..], b"0123456789");
    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 7);
}

#[tokio::test]
async fn text_playlist_formats_redirect_to_their_first_track() {
    let m3u = "#EXTM3U\nhttps://radio.example/m3u\n";
    let pls = "[playlist]\nTitle1=PLS Station\nFile1=https://radio.example/pls\n";
    let plain = "https://radio.example/plain\n";
    let response = |content_type: &str, body: &str| {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let (address, server) = server_owned(vec![
        response("audio/x-mpegurl", m3u),
        response("audio/x-scpls", pls),
        response("text/plain", plain),
    ])
    .await;
    let source = HttpSource::new().expect("client");

    let redirects = [
        ("m3u", "https://radio.example/m3u", None),
        ("pls", "https://radio.example/pls", Some("PLS Station")),
        ("plain", "https://radio.example/plain", None),
    ];
    for (path, expected, title) in redirects {
        let SourceResult::Redirect(reference) = source
            .load(&SourceReference::new(format!("http://{address}/{path}")))
            .await
            .expect("playlist")
        else {
            panic!("expected playlist redirect");
        };
        assert_eq!(reference.identifier(), expected);
        assert_eq!(reference.title(), title);
    }
    let requests = server.await.expect("server task");
    assert_eq!(requests.len(), 3);
}
