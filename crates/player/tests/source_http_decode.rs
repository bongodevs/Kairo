use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use kairo_audio::decode::{BlockingDecoder, Millis};

use super::*;
use crate::source::{Source, SourceReference, SourceResult};
use crate::test_wav;

#[tokio::test]
async fn ranged_http_source_supports_decoder_seek() {
    let bytes: Arc<[u8]> = test_wav::pcm_wave(48_000, 2, 480_000).into();
    let (address, stop, requests, server) = media_server(bytes).await;
    let source = HttpSource::new().expect("client");
    let SourceResult::Opened(opened) = source
        .load(&SourceReference::new(format!("http://{address}/track.wav")))
        .await
        .expect("open")
    else {
        panic!("expected opened source");
    };
    assert!(opened.is_seekable());

    let runtime = Handle::current();
    let first_position = tokio::task::spawn_blocking(move || {
        let mut decoder = BlockingDecoder::open(opened, runtime).expect("decoder");
        assert!(decoder.seek(Millis::new(5_000)).expect("seek"));
        decoder.next_block().expect("decode").expect("block").position
    })
    .await
    .expect("decode worker");
    assert_eq!(first_position, Millis::new(5_000));
    assert!(requests.load(Ordering::Relaxed) >= 2);

    let _ = stop.send(());
    server.await.expect("server task");
}

async fn media_server(
    bytes: Arc<[u8]>,
) -> (String, oneshot::Sender<()>, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let requests = Arc::new(AtomicUsize::new(0));
    let request_count = requests.clone();
    let (stop, mut stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = &mut stopped => return,
            };
            let (mut socket, _) = accepted.expect("accept");
            let mut request = [0; 4096];
            let length = socket.read(&mut request).await.expect("request");
            request_count.fetch_add(1, Ordering::Relaxed);
            let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
            let start = request
                .lines()
                .find_map(|line| line.strip_prefix("range: bytes="))
                .and_then(|range| range.split_once('-'))
                .and_then(|(start, _)| start.parse::<usize>().ok())
                .unwrap_or(0)
                .min(bytes.len());
            let body = &bytes[start..];
            let header = if start == 0 {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
            } else {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    start,
                    bytes.len() - 1,
                    bytes.len()
                )
            };
            socket.write_all(header.as_bytes()).await.expect("header");
            socket.write_all(body).await.expect("body");
            socket.shutdown().await.expect("shutdown");
        }
    });
    (address.to_string(), stop, requests, task)
}
