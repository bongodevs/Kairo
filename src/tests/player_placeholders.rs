use player::source::SourceReference;
use player::track::Track;

use super::Placeholders;

fn track(identifier: &str) -> Track {
    Track::new(SourceReference::new(identifier))
}

#[test]
fn an_armed_successor_does_not_replace_the_playing_placeholder() {
    let mut slots = Placeholders::default();
    let playing = track("first");
    let next = track("second");

    slots.mark_playing(&playing);
    let resume = slots.take_resume().expect("playing retry");
    assert_eq!(resume.reference, *playing.reference());

    slots.arm_next(&next);
    assert!(slots.is_playing(playing.reference()));
    assert!(!slots.is_playing(next.reference()));

    assert!(slots.promote(next.reference()));
    assert!(!slots.is_playing(playing.reference()));
    assert!(slots.is_playing(next.reference()));
    let resume = slots.take_resume().expect("successor retry");
    assert_eq!(resume.reference, *next.reference());
}

#[test]
fn promoting_a_track_that_was_not_armed_changes_nothing() {
    let mut slots = Placeholders::default();
    let playing = track("first");
    slots.mark_playing(&playing);

    assert!(!slots.promote(&SourceReference::new("other")));
    assert!(slots.is_playing(playing.reference()));
    assert!(slots.take_resume().is_some());
}

#[test]
fn clearing_an_armed_successor_keeps_the_playing_retry() {
    let mut slots = Placeholders::default();
    let playing = track("first");
    slots.mark_playing(&playing);
    slots.arm_next(&track("second"));
    slots.clear_next();

    assert!(!slots.promote(&SourceReference::new("second")));
    assert!(slots.is_playing(playing.reference()));
    assert!(slots.take_resume().is_some());
}

#[test]
fn finishing_a_retry_leaves_the_armed_successor_alone() {
    let mut slots = Placeholders::default();
    slots.mark_playing(&track("first"));
    let resume = slots.take_resume().expect("retry");
    let next = track("second");
    slots.arm_next(&next);

    slots.finish(resume.generation);
    assert!(!slots.is_generation(resume.generation));
    assert!(!slots.is_playing(&SourceReference::new("first")));

    assert!(slots.promote(next.reference()));
    assert!(slots.take_resume().is_some());
}

#[test]
fn marking_the_same_track_playing_drops_its_armed_duplicate() {
    let mut slots = Placeholders::default();
    let same = track("same");

    slots.arm_next(&same);
    slots.mark_playing(&same);
    assert!(slots.is_playing(same.reference()));
    assert!(!slots.promote(same.reference()));
    assert!(slots.take_resume().is_some());
}
