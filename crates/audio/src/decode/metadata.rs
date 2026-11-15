#[cfg(test)]
#[path = "../../tests/decode_metadata.rs"]
mod tests;

use symphonia::core::formats::FormatReader;
use symphonia::core::meta::{MetadataContainer, MetadataRevision, RawValue, StandardTag, Tag};

use kairo_core::TrackMetadata;

use super::mp4::Gapless;

pub(super) fn refresh(
    format: &mut dyn FormatReader,
    track_id: u32,
    target: &mut TrackMetadata,
    force: bool,
) {
    let mut metadata = format.metadata();
    if !force && metadata.is_latest() {
        return;
    }

    while let Some(revision) = metadata.pop() {
        apply_revision(target, track_id, &revision);
    }
    if let Some(revision) = metadata.current() {
        apply_revision(target, track_id, revision);
    }
}

fn apply_revision(target: &mut TrackMetadata, track_id: u32, revision: &MetadataRevision) {
    apply_container(target, &revision.media);
    for track in &revision.per_track {
        if track.track_id == u64::from(track_id) {
            apply_container(target, &track.metadata);
        }
    }
}

fn apply_container(target: &mut TrackMetadata, container: &MetadataContainer) {
    for tag in &container.tags {
        apply_tag(target, tag);
    }
}

/// Read an `iTunSMPB` freeform tag, when the container carries one.
///
/// The value is four to six space-separated hexadecimal fields: a reserved
/// zero, the encoder delay, the trailing padding, and the original sample
/// count. Counts are already in output PCM frames.
pub(super) fn itunsmpb(format: &mut dyn FormatReader) -> Option<Gapless> {
    let mut metadata = format.metadata();
    let revision = metadata.skip_to_latest()?;
    revision
        .media
        .tags
        .iter()
        .chain(revision.per_track.iter().flat_map(|track| track.metadata.tags.iter()))
        .find_map(parse_itunsmpb_tag)
}

fn parse_itunsmpb_tag(tag: &Tag) -> Option<Gapless> {
    let name = tag.raw.key.rsplit(':').next()?;
    if !name.eq_ignore_ascii_case("iTunSMPB") {
        return None;
    }
    let RawValue::String(value) = &tag.raw.value else {
        return None;
    };
    let mut fields = value.split_whitespace();
    let _reserved = fields.next()?;
    let priming = u64::from_str_radix(fields.next()?, 16).ok()?;
    let _padding = u64::from_str_radix(fields.next()?, 16).ok()?;
    let total = u64::from_str_radix(fields.next()?, 16).ok()?;
    (total > 0).then_some(Gapless { priming, total: Some(total) })
}

fn apply_tag(target: &mut TrackMetadata, tag: &Tag) {
    match tag.std.as_ref() {
        Some(StandardTag::TrackTitle(value)) => target.title = Some(value.as_ref().clone()),
        Some(StandardTag::Artist(value)) => target.artist = Some(value.as_ref().clone()),
        Some(StandardTag::IdentIsrc(value)) => target.isrc = Some(value.as_ref().clone()),
        Some(StandardTag::AlbumArtist(value) | StandardTag::Author(value))
            if target.artist.is_none() =>
        {
            target.artist = Some(value.as_ref().clone());
        }
        _ => {}
    }
}
