use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io;

use kairo_audio::decode::Millis;
use kairo_core::track::{Playlist, SourceData, Track, TrackInfo};

use super::*;
use crate::source::SourceHints;
enum Reply {
    Miss,
    Empty,
    Open(&'static str),
    Redirect(&'static str, &'static str),
    EndlessRedirect,
    Wave(&'static str),
    Playlist(&'static str, bool),
    Fail,
}

struct FakeSource {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    reply: Reply,
}

impl Source for FakeSource {
    fn name(&self) -> &str {
        self.name
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let result = match self.reply {
                Reply::Miss => SourceResult::NotMine,
                Reply::Empty => SourceResult::Empty,
                Reply::Open(identifier) if reference.identifier() == identifier => {
                    SourceResult::Opened(OpenedSource::read_only(
                        self.name,
                        reference.clone(),
                        SourceHints::default(),
                        Some(0),
                        io::empty(),
                    ))
                }
                Reply::Redirect(from, to) if reference.identifier() == from => {
                    SourceResult::Redirect(SourceReference::new(to))
                }
                Reply::EndlessRedirect => {
                    let next = format!("{}x", reference.identifier());
                    SourceResult::Redirect(SourceReference::new(next))
                }
                Reply::Wave(identifier) if reference.identifier() == identifier => {
                    let bytes = crate::test_wav::pcm_wave(48_000, 2, 960);
                    SourceResult::Opened(OpenedSource::seekable(
                        self.name,
                        reference.clone(),
                        SourceHints {
                            mime_type: Some(String::from("audio/wav")),
                            file_extension: Some(String::from("wav")),
                        },
                        Some(bytes.len() as u64),
                        Cursor::new(bytes),
                    ))
                }
                Reply::Playlist(identifier, search) if reference.identifier() == identifier => {
                    let tracks = vec![Track::new(SourceReference::new("result"))];
                    let playlist = if search {
                        Playlist::search("results", tracks)
                    } else {
                        Playlist::new("playlist", tracks)
                            .with_selected_track(0)
                            .expect("selected track")
                    };
                    SourceResult::Playlist(playlist)
                }
                Reply::Fail => {
                    return Err(SourceError::load_failed(
                        "provider rejected the identifier",
                        kairo_core::ExceptionSeverity::Suspicious,
                    ));
                }
                Reply::Open(_) | Reply::Redirect(_, _) | Reply::Wave(_) | Reply::Playlist(_, _) => {
                    SourceResult::NotMine
                }
            };
            Ok(result)
        })
    }
}

impl TrackSource for FakeSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        Box::pin(async move {
            match self.load(track.reference()).await? {
                SourceResult::Opened(opened) => Ok(Some(opened)),
                SourceResult::NotMine | SourceResult::Empty => Ok(None),
                _ => Err(SourceError::InvalidPlaybackResult { name: self.name.to_owned() }),
            }
        })
    }
}

fn fake(name: &'static str, reply: Reply) -> (FakeSource, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    (FakeSource { name, calls: calls.clone(), reply }, calls)
}

struct ProviderSource {
    name: &'static str,
    loads: Arc<AtomicUsize>,
    opens: Arc<AtomicUsize>,
}

impl Source for ProviderSource {
    fn name(&self) -> &str {
        self.name
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async move {
            self.loads.fetch_add(1, Ordering::Relaxed);
            let identifier = format!("{}:track", self.name);
            if reference.identifier() != identifier {
                return Ok(SourceResult::NotMine);
            }
            Ok(SourceResult::Track(Track::from_parts(
                SourceReference::new("track-id"),
                TrackInfo {
                    identifier: String::from("track-id"),
                    is_seekable: true,
                    author: String::from("API Artist"),
                    length: Some(Millis::new(20)),
                    is_stream: false,
                    title: String::from("API Title"),
                    uri: Some(identifier),
                    source_name: self.name.to_owned(),
                    artwork_url: None,
                    isrc: None,
                },
                SourceData::None,
                Millis::default(),
            )))
        })
    }
}

impl TrackSource for ProviderSource {
    fn open<'a>(&'a self, track: &'a Track) -> OpenFuture<'a> {
        Box::pin(async move {
            self.opens.fetch_add(1, Ordering::Relaxed);
            let bytes = crate::test_wav::pcm_wave(48_000, 2, 960);
            Ok(Some(OpenedSource::seekable(
                self.name,
                track.reference().clone(),
                SourceHints {
                    mime_type: Some(String::from("audio/wav")),
                    file_extension: Some(String::from("wav")),
                },
                Some(bytes.len() as u64),
                Cursor::new(bytes),
            )))
        })
    }
}

#[tokio::test]
async fn sources_are_tried_in_registration_order_until_one_opens() {
    let (miss, miss_calls) = fake("miss", Reply::Miss);
    let (winner, winner_calls) = fake("winner", Reply::Open("track"));
    let (unused, unused_calls) = fake("unused", Reply::Open("track"));
    let mut sources = Sources::new();
    sources.register(miss).expect("register miss");
    sources.register(winner).expect("register winner");
    sources.register(unused).expect("register unused");

    let opened =
        sources.open(SourceReference::new("track")).await.expect("resolve").expect("opened");

    assert_eq!(opened.source_name(), "winner");
    assert_eq!(miss_calls.load(Ordering::Relaxed), 1);
    assert_eq!(winner_calls.load(Ordering::Relaxed), 1);
    assert_eq!(unused_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_redirect_restarts_resolution_from_the_first_source() {
    let (redirect, redirect_calls) = fake("redirect", Reply::Redirect("start", "final"));
    let (target, target_calls) = fake("target", Reply::Open("final"));
    let mut sources = Sources::new();
    sources.register(redirect).expect("register redirect");
    sources.register(target).expect("register target");

    let opened =
        sources.open(SourceReference::new("start")).await.expect("resolve").expect("opened");

    assert_eq!(opened.reference().identifier(), "final");
    assert_eq!(redirect_calls.load(Ordering::Relaxed), 2);
    assert_eq!(target_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn redirect_chains_stop_after_five_resolution_steps() {
    let (source, calls) = fake("loop", Reply::EndlessRedirect);
    let mut sources = Sources::new();
    sources.register(source).expect("register loop");

    let result = sources.open(SourceReference::new("start")).await.expect("resolve");

    assert!(result.is_none());
    assert_eq!(calls.load(Ordering::Relaxed), MAX_RESOLUTION_STEPS);
}

#[test]
fn source_names_are_unique() {
    let (first, _) = fake("same", Reply::Miss);
    let (second, _) = fake("same", Reply::Miss);
    let mut sources = Sources::new();
    sources.register(first).expect("register first");

    let error = sources.register(second).expect_err("duplicate rejected");
    assert!(matches!(error, SourceError::DuplicateSource { .. }));
}

#[tokio::test]
async fn loading_an_opened_source_returns_a_resolved_track() {
    let (source, _) = fake("wave", Reply::Wave("track"));
    let mut sources = Sources::new().with_probe(crate::test_audio_probe::audio_probe());
    sources.register(source).expect("register source");

    let LoadOutcome::Track(track) = sources.load(SourceReference::new("track")).await else {
        panic!("expected track outcome");
    };
    let info = track.info().expect("resolved info");
    assert_eq!(info.source_name, "wave");
    assert_eq!(info.identifier, "track");
}

#[tokio::test]
async fn provider_metadata_and_media_opening_are_separate_owned_steps() {
    let loads = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicUsize::new(0));
    let mut sources = Sources::new();
    sources
        .register(ProviderSource { name: "provider", loads: loads.clone(), opens: opens.clone() })
        .expect("provider");

    let LoadOutcome::Track(track) = sources.load(SourceReference::new("provider:track")).await
    else {
        panic!("expected track");
    };
    assert_eq!(track.info().map(|info| info.title.as_str()), Some("API Title"));
    assert_eq!(loads.load(Ordering::Relaxed), 1);
    assert_eq!(opens.load(Ordering::Relaxed), 0);

    let opened = sources.open_track(&track).await.expect("open").expect("media");
    assert_eq!(opened.source_name(), "provider");
    assert_eq!(opened.reference().identifier(), "track-id");
    assert_eq!(loads.load(Ordering::Relaxed), 1);
    assert_eq!(opens.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn open_track_routes_by_the_source_name_in_the_metadata() {
    let first_loads = Arc::new(AtomicUsize::new(0));
    let first_opens = Arc::new(AtomicUsize::new(0));
    let second_loads = Arc::new(AtomicUsize::new(0));
    let second_opens = Arc::new(AtomicUsize::new(0));
    let mut sources = Sources::new();
    sources
        .register(ProviderSource {
            name: "first",
            loads: first_loads.clone(),
            opens: first_opens.clone(),
        })
        .expect("first");
    sources
        .register(ProviderSource {
            name: "second",
            loads: second_loads.clone(),
            opens: second_opens.clone(),
        })
        .expect("second");

    let LoadOutcome::Track(track) = sources.load(SourceReference::new("second:track")).await else {
        panic!("expected track");
    };
    let opened = sources.open_track(&track).await.expect("open").expect("media");

    assert_eq!(opened.source_name(), "second");
    assert_eq!(first_loads.load(Ordering::Relaxed), 1);
    assert_eq!(second_loads.load(Ordering::Relaxed), 1);
    assert_eq!(first_opens.load(Ordering::Relaxed), 0);
    assert_eq!(second_opens.load(Ordering::Relaxed), 1);
}

struct PrefixSource {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

impl Source for PrefixSource {
    fn name(&self) -> &str {
        self.name
    }

    fn search_prefixes(&self) -> &'static [&'static str] {
        &["jssearch:"]
    }

    fn load<'a>(&'a self, reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if !reference.identifier().starts_with("jssearch:") {
                return Ok(SourceResult::NotMine);
            }
            Ok(SourceResult::Playlist(Playlist::search(
                "prefix results",
                vec![Track::new(SourceReference::new("result"))],
            )))
        })
    }
}

impl TrackSource for PrefixSource {
    fn open<'a>(&'a self, _track: &'a Track) -> OpenFuture<'a> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn a_search_prefix_owner_is_the_only_source_tried() {
    let owner_calls = Arc::new(AtomicUsize::new(0));
    let (other, other_calls) = fake("other", Reply::Playlist("jssearch:query", true));
    let owner = PrefixSource { name: "owner", calls: owner_calls.clone() };
    let mut sources = Sources::new();
    sources.register(other).expect("other");
    sources.register(owner).expect("owner");

    let LoadOutcome::Search(tracks) = sources.search("jssearch:query").await else {
        panic!("expected search outcome");
    };

    assert_eq!(tracks.len(), 1);
    assert_eq!(owner_calls.load(Ordering::Relaxed), 1);
    assert_eq!(other_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn playlists_and_searches_keep_distinct_load_outcomes() {
    let (playlist, _) = fake("playlist", Reply::Playlist("list", false));
    let (search, _) = fake("search", Reply::Playlist("query", true));
    let mut sources = Sources::new();
    sources.register(playlist).expect("register playlist");
    sources.register(search).expect("register search");

    let LoadOutcome::Playlist(playlist) = sources.load(SourceReference::new("list")).await else {
        panic!("expected playlist outcome");
    };
    assert_eq!(playlist.selected_track_index(), Some(0));

    let LoadOutcome::Search(tracks) = sources.load(SourceReference::new("query")).await else {
        panic!("expected search outcome");
    };
    assert_eq!(tracks.len(), 1);
}

#[tokio::test]
async fn no_match_and_source_failure_are_in_band_load_outcomes() {
    let (miss, _) = fake("miss", Reply::Miss);
    let mut sources = Sources::new();
    sources.register(miss).expect("register miss");
    assert!(matches!(sources.load(SourceReference::new("missing")).await, LoadOutcome::Empty));

    let (failed, _) = fake("failed", Reply::Fail);
    let mut sources = Sources::new();
    sources.register(failed).expect("register failed");
    let LoadOutcome::Error(error) = sources.load(SourceReference::new("broken")).await else {
        panic!("expected error outcome");
    };
    assert_eq!(error.severity(), kairo_core::ExceptionSeverity::Suspicious);
}

#[tokio::test]
async fn a_claimed_empty_result_does_not_fall_through_to_later_sources() {
    let (empty, empty_calls) = fake("empty", Reply::Empty);
    let (later, later_calls) = fake("later", Reply::Open("claimed"));
    let mut sources = Sources::new();
    sources.register(empty).expect("empty source");
    sources.register(later).expect("later source");

    assert!(matches!(sources.load(SourceReference::new("claimed")).await, LoadOutcome::Empty));
    assert_eq!(empty_calls.load(Ordering::Relaxed), 1);
    assert_eq!(later_calls.load(Ordering::Relaxed), 0);
}
