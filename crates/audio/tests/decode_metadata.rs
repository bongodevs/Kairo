use std::sync::Arc;

use symphonia::core::meta::{RawTag, RawValue};

use super::*;
fn tag(standard: StandardTag) -> Tag {
    Tag::new_std(RawTag::new("test", RawValue::String(Arc::new(String::from("raw")))), standard)
}

#[test]
fn an_itunes_gapless_tag_supplies_the_presentation_window() {
    let tag = Tag::new_from_parts(
        "com.apple.iTunes:iTunSMPB",
        RawValue::String(Arc::new(String::from(" 00000000 00000400 00000120 00002ee0"))),
        None,
    );
    assert_eq!(parse_itunsmpb_tag(&tag), Some(Gapless { priming: 1_024, total: Some(12_000) }));
}

#[test]
fn direct_artist_wins_over_fallback_and_revisions_upsert() {
    let mut metadata = TrackMetadata::default();
    apply_tag(&mut metadata, &tag(StandardTag::AlbumArtist(Arc::new(String::from("album")))));
    apply_tag(&mut metadata, &tag(StandardTag::Artist(Arc::new(String::from("artist")))));
    apply_tag(&mut metadata, &tag(StandardTag::TrackTitle(Arc::new(String::from("title")))));
    apply_tag(&mut metadata, &tag(StandardTag::IdentIsrc(Arc::new(String::from("US123")))));

    assert_eq!(metadata.title.as_deref(), Some("title"));
    assert_eq!(metadata.artist.as_deref(), Some("artist"));
    assert_eq!(metadata.isrc.as_deref(), Some("US123"));
}
