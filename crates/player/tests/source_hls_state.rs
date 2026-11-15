use super::*;

fn segment(path: &str) -> HlsSegment {
    HlsSegment {
        url: Url::parse(&format!("https://radio.example/{path}")).expect("segment URL"),
        duration: Some(Duration::from_secs(1)),
    }
}

#[test]
fn segment_selection_advances_without_replaying_the_live_edge() {
    let segments = [segment("one.ts"), segment("two.ts")];
    assert_eq!(choose_segment(&segments, None), Some(&segments[0]));
    assert_eq!(choose_segment(&segments, Some(&segments[0].url)), Some(&segments[1]));
    assert_eq!(choose_segment(&segments, Some(&segments[1].url)), None);
    let missing = Url::parse("https://radio.example/missing.ts").expect("missing URL");
    assert_eq!(choose_segment(&segments, Some(&missing)), Some(&segments[0]));
}
