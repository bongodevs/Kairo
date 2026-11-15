use serde::Deserialize;
use serde_json::{Value, json};

use super::*;
use crate::source::test_support::{
    Reply, Server, assert_load, assert_open, assert_search, assert_track,
};
use crate::source::{Api, LoadOutcome, SourceReference, Sources};
use kairo_core::track::{Track, TrackInfo};

const MEDIA: &[u8] = b"RIFFfake-wave";

fn track(source: &str, title: &str, identifier: &str) -> Track {
    Track::with_info(
        SourceReference::with_title(identifier, title),
        TrackInfo {
            identifier: identifier.to_owned(),
            is_seekable: true,
            author: String::from("Fake Author"),
            length: None,
            is_stream: false,
            title: title.to_owned(),
            uri: Some(identifier.to_owned()),
            source_name: source.to_owned(),
            artwork_url: None,
            isrc: None,
        },
    )
}

fn fake_provider(media_url: String) -> Provider {
    Provider::new("fake")
        .prefix("fake:")
        .item_prefix("fakeid:", Kind::Track)
        .route("music.fake.test", "track/{slug}/{id}", Kind::Track)
        .loader(|request| async move {
            Ok(match request {
                Request::Search(_) => {
                    Load::Search(vec![track("fake", "Search result", "fake:search")])
                }
                Request::Item(Kind::Track, id) => {
                    Load::Track(track("fake", &format!("Track {id}"), &format!("fake:{id}")))
                }
                Request::Item(_, _) => Load::Empty,
            })
        })
        .opener(move |_track| {
            let media_url = media_url.clone();
            async move {
                Ok(Stream::url(media_url)
                    .header("x-source", "fake")
                    .content_type("audio/wav")
                    .length(MEDIA.len() as u64)
                    .seekable(true))
            }
        })
}

fn mirror_provider(mirror: Track) -> Provider {
    Provider::new("mirror")
        .route("mirror.fake.test", "play/{id}", Kind::Track)
        .loader(|request| async move {
            Ok(match request {
                Request::Item(Kind::Track, id) => {
                    Load::Track(track("mirror", &format!("Mirror {id}"), &format!("mirror:{id}")))
                }
                _ => Load::Empty,
            })
        })
        .opener(move |_track| {
            let mirror = mirror.make_clone();
            async move { Ok(Stream::mirror(mirror)) }
        })
}

#[tokio::test]
async fn url_route_search_prefix_and_item_prefix_reach_the_loader() {
    let mut sources = Sources::new();
    let provider = fake_provider(String::from("http://127.0.0.1:1/unused")).enabled(false);
    sources.register(provider).expect("register");

    let outcome =
        sources.load(SourceReference::new("https://music.fake.test/track/hello/id-7")).await;
    assert!(matches!(outcome, LoadOutcome::Empty));
    assert_eq!(sources.metrics("fake").expect("metrics").loads, 0);

    let provider = fake_provider(String::from("http://127.0.0.1:1/unused"));
    let mut sources = Sources::new();
    sources.register(provider).expect("register");

    let track =
        assert_track(assert_load(&sources, "https://music.fake.test/track/hello/id-7").await);
    assert_eq!(track.info().map(|info| info.title.as_str()), Some("Track id-7"));

    let track = assert_track(assert_load(&sources, "fakeid:item-1").await);
    assert_eq!(track.info().map(|info| info.title.as_str()), Some("Track item-1"));

    let tracks = assert_search(sources.search("fake:demo").await);
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].info().map(|info| info.title.as_str()), Some("Search result"));

    let snapshot = sources.metrics("fake").expect("metrics");
    assert_eq!(snapshot.loads, 3);
    assert_eq!(snapshot.load_errors, 0);
}

#[tokio::test]
async fn stream_url_is_opened_relabelled_and_honours_builders() {
    let server = Server::start(vec![Reply::ok("audio/wav", MEDIA)]).await;
    let provider = fake_provider(server.url("media.wav"));
    let mut sources = Sources::new();
    sources.register(provider).expect("register");

    let track =
        assert_track(assert_load(&sources, "https://music.fake.test/track/hello/id-9").await);
    let opened = assert_open(&sources, &track).await;
    assert_eq!(opened.source_name(), "fake");
    assert_eq!(opened.hints().mime_type.as_deref(), Some("audio/wav"));
    assert_eq!(opened.content_length(), Some(MEDIA.len() as u64));
    assert!(opened.is_seekable());
    assert_eq!(opened.metadata().title.as_deref(), Some("Track id-9"));
    assert_eq!(opened.metadata().author.as_deref(), Some("Fake Author"));

    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64];
    let mut reader = opened;
    loop {
        let read = reader.read(&mut buffer).await.expect("read");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    assert_eq!(bytes, MEDIA);

    let requests = server.finish().await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("x-source: fake"), "{}", requests[0]);
    assert!(requests[0].contains("GET /media.wav "), "{}", requests[0]);

    let snapshot = sources.metrics("fake").expect("metrics");
    assert_eq!(snapshot.opens, 1);
    assert_eq!(snapshot.open_errors, 0);
    assert!(snapshot.average_open_micros().is_some());
}

#[tokio::test]
async fn mirror_stream_opens_another_direct_track() {
    let server = Server::start(vec![Reply::ok("audio/wav", MEDIA)]).await;
    let mirror = Track::new(SourceReference::new(server.url("mirror.wav")));
    let mut sources = Sources::new();
    sources.register(mirror_provider(mirror)).expect("register");

    let track = assert_track(assert_load(&sources, "https://mirror.fake.test/play/1").await);
    let opened = assert_open(&sources, &track).await;
    assert_eq!(opened.source_name(), "mirror");
    assert_eq!(opened.metadata().title.as_deref(), Some("Mirror 1"));

    let requests = server.finish().await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("GET /mirror.wav "), "{}", requests[0]);
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FakeConfig {
    #[serde(default)]
    secret_key: String,
    #[serde(default)]
    artist_top_tracks: u32,
}

#[test]
fn config_reads_enabled_and_environment_secrets() {
    let table = json!({
        "enabled": false,
        "secretKey": "file-secret",
        "artistTopTracks": 7,
    });
    let env = vec![(String::from("KAIRO_SOURCES_FAKE_SECRET_KEY"), String::from("env-secret"))];
    let settings = read_with::<FakeConfig, _>("fake", Some(&table), env).expect("config");
    assert!(!settings.enabled);
    assert_eq!(settings.value.secret_key, "env-secret");
    assert_eq!(settings.value.artist_top_tracks, 7);

    let settings =
        read_with::<FakeConfig, _>("fake", None, Vec::<(String, String)>::new()).expect("defaults");
    assert!(settings.enabled);
    assert_eq!(settings.value.secret_key, "");

    let numeric = vec![(String::from("KAIRO_SOURCES_FAKE_SECRET_KEY"), String::from("12345678"))];
    let settings = read_with::<FakeConfig, _>("fake", None, numeric).expect("numeric secret");
    assert_eq!(settings.value.secret_key, "12345678");
}

#[test]
fn config_rejects_unknown_keys_with_the_provider_name() {
    let error = match read_with::<FakeConfig, _>(
        "fake",
        Some(&json!({ "unknown": 1 })),
        Vec::<(String, String)>::new(),
    ) {
        Ok(_) => panic!("unknown key accepted"),
        Err(error) => error,
    };
    let message = error.to_string();
    assert!(message.contains("sources.fake"), "{message}");
    assert!(message.contains("unknown"), "{message}");
}

#[tokio::test]
async fn api_decodes_typed_json_with_query_and_header() {
    let server = Server::start(vec![Reply::json(r#"{"ok":true}"#)]).await;
    let api = Api::new("fake", server.base().clone()).expect("api");
    let answer: Answer =
        api.get("check").query(&[("q", "x")]).header("x-key", "secret").json().await.expect("json");
    assert!(answer.ok);

    let requests = server.finish().await;
    assert!(requests[0].contains("GET /check?q=x "), "{}", requests[0]);
    assert!(requests[0].contains("x-key: secret"), "{}", requests[0]);
}

#[tokio::test]
async fn api_retries_a_server_error_then_decodes() {
    let server =
        Server::start(vec![Reply::status(500), Reply::json(r#"{"value":"second"}"#)]).await;
    let api = Api::new("fake", server.base().clone()).expect("api").with_retries(1);
    let answer: Answer = api.get("retry").json().await.expect("retried json");
    assert_eq!(answer.value, "second");

    let requests = server.finish().await;
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn api_caps_a_large_response() {
    let server = Server::start(vec![Reply::ok("application/json", [b'a'; 128])]).await;
    let api =
        Api::new("fake", server.base().clone()).expect("api").with_retries(0).with_max_bytes(16);
    let error = api.get("big").json::<Value>().await.expect_err("too large");
    assert!(matches!(error, SourceError::ResponseTooLarge { max: 16 }));
}

#[tokio::test]
async fn api_posts_a_json_body() {
    let server = Server::start(vec![Reply::json(r#"{"id":"made"}"#)]).await;
    let api = Api::new("fake", server.base().clone()).expect("api");
    let answer: Answer = api
        .post("create")
        .body(&json!({ "name": "demo" }))
        .expect("body")
        .json()
        .await
        .expect("json");
    assert_eq!(answer.id.as_deref(), Some("made"));

    let requests = server.finish().await;
    assert!(requests[0].contains(r#"{"name":"demo"}"#), "{}", requests[0]);
}

#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    value: String,
    #[serde(default)]
    id: Option<String>,
}
