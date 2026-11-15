use super::{HlsManifest, PlaylistFormat, PlaylistTarget, detect, parse, parse_hls};
use crate::source::{SourceHints, SourceReference};
fn base() -> reqwest::Url {
    reqwest::Url::parse("https://radio.example/live/master.m3u8").expect("base URL")
}

#[test]
fn m3u_pls_and_plain_signatures_are_detected() {
    assert_eq!(detect(b"#EXTM3U\n", &SourceHints::default()), Some(PlaylistFormat::M3u));
    assert_eq!(detect(b"[playlist]\n", &SourceHints::default()), Some(PlaylistFormat::Pls));
    assert_eq!(
        detect(b"https://radio.example/stream\n", &SourceHints::default()),
        Some(PlaylistFormat::Plain)
    );
}

#[test]
fn direct_playlists_keep_the_first_link_and_title() {
    let pls = "[playlist]\nTitle2=Station\nFile2=https://radio.example/live\n";
    assert_eq!(
        parse(PlaylistFormat::Pls, pls, &base()),
        Some(PlaylistTarget::Redirect(SourceReference::with_title(
            "https://radio.example/live",
            "Station"
        )))
    );
    assert_eq!(
        parse(PlaylistFormat::Plain, "https://radio.example/direct\nignored", &base()),
        Some(PlaylistTarget::Redirect(SourceReference::new("https://radio.example/direct")))
    );
}

#[test]
fn extended_m3u_is_classified_as_hls() {
    let manifest = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nlow/index.m3u8\n";
    assert_eq!(parse(PlaylistFormat::M3u, manifest, &base()), Some(PlaylistTarget::Hls));
    assert_eq!(
        parse_hls(manifest, &base()).expect("HLS master"),
        HlsManifest::Master(
            reqwest::Url::parse("https://radio.example/live/low/index.m3u8").expect("variant URL")
        )
    );
}

#[test]
fn media_manifest_resolves_segments_and_duration() {
    let manifest = "#EXTM3U\n#EXTINF:2.5,one\nseg-1.ts\n#EXTINF:2,two\n/seg-2.ts\n#EXT-X-ENDLIST\n";
    let HlsManifest::Media { segments, ended } =
        parse_hls(manifest, &base()).expect("media manifest")
    else {
        panic!("expected media manifest");
    };
    assert!(ended);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].duration, Some(std::time::Duration::from_millis(2_500)));
    assert_eq!(segments[0].url.as_str(), "https://radio.example/live/seg-1.ts");
    assert_eq!(segments[1].url.as_str(), "https://radio.example/seg-2.ts");
}

#[test]
fn encrypted_or_fragmented_hls_is_rejected_explicitly() {
    assert!(parse_hls("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128\nseg.ts", &base()).is_err());
    assert!(parse_hls("#EXTM3U\n#EXT-X-MAP:URI=init.mp4\nseg.m4s", &base()).is_err());
}
