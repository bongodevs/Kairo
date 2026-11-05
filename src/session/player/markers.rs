use std::time::{Duration, Instant};

use super::*;

/// Total budget for both SponsorBlock calls, however the time is split between them.
const SPONSORBLOCK_DEADLINE: Duration = Duration::from_secs(10);

impl PlayerShared {
    pub(super) fn handle_sponsorblock(
        player_weak: Weak<AudioPlayer>,
        context_weak: Weak<SocketContext>,
        guild_id: u64,
        guild_id_str: String,
        video_id: String,
        track_duration_ms: u64,
        categories: std::collections::HashSet<String>,
    ) {
        use player::tools::http::HttpInterface;

        // One budget for both calls: a slow segments lookup leaves less time for chapters rather
        // than giving each its own full timeout.
        let deadline = Instant::now() + SPONSORBLOCK_DEADLINE;
        let http = HttpInterface::new().with_timeout(SPONSORBLOCK_DEADLINE);
        let segments = sponsorblock::retrieve_video_segments(&http, &video_id, &categories);
        let chapters = match deadline.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => sponsorblock::retrieve_video_chapters(
                &HttpInterface::new().with_timeout(left),
                &video_id,
                track_duration_ms,
            ),
            _ => Vec::new(),
        };

        let mut markables: Vec<TrackMarkable> = Vec::with_capacity(segments.len() + chapters.len());
        for s in &segments {
            markables.push(TrackMarkable::Segment(s.clone()));
        }
        for c in &chapters {
            markables.push(TrackMarkable::Chapter(c.clone()));
        }
        markables.sort_by_key(|m| m.start());
        let markables = Arc::new(markables);

        let Some(ctx) = context_weak.upgrade() else {
            return;
        };

        // Cache even an empty answer: this video has no markers for this category set, so a replay
        // does not need the network again.
        ctx.cache_sponsorblock_markables(&video_id, &categories, Arc::clone(&markables));

        // A track change while the lookup ran makes the result stale for this player. The cached
        // copy above is still valid for a later replay of the same video. The session slot is the
        // authority here: a crossfade successor is already current when its `TrackStart` fires, but
        // is not the engine's active executor until the overlap ends.
        let current = ctx
            .get_player(guild_id)
            .and_then(|player| player.current_track());
        if current
            .as_ref()
            .is_none_or(|track| track.info.identifier != video_id)
        {
            return;
        }

        if !segments.is_empty() {
            ctx.send_message(Message::event(EmittedEvent::SegmentsLoaded {
                guild_id: guild_id_str.clone(),
                segments: segments.clone(),
            }));
        }
        if !chapters.is_empty() {
            ctx.send_message(Message::event(EmittedEvent::ChaptersLoaded {
                guild_id: guild_id_str.clone(),
                chapters: chapters.clone(),
            }));
        }

        if markables.is_empty() {
            return;
        }

        tracing::info!(
            guild_id,
            count = markables.len(),
            "arming sponsorblock markers"
        );

        Self::set_sponsorblock_marker(&player_weak, &context_weak, guild_id_str, &markables, 0);
    }

    pub(super) fn set_sponsorblock_marker(
        player_weak: &Weak<AudioPlayer>,
        context_weak: &Weak<SocketContext>,
        guild_id_str: String,
        markables: &Arc<Vec<TrackMarkable>>,
        index: usize,
    ) {
        if index >= markables.len() {
            return;
        }

        let start = markables[index].start();
        let handler = make_segment_handler(
            player_weak.clone(),
            context_weak.clone(),
            guild_id_str,
            Arc::clone(markables),
            index,
        );

        if let Some(player) = player_weak.upgrade() {
            player.add_marker(TrackMarker::new(start, handler));
        }
    }
}

fn handle_markable_at(
    player_weak: &Weak<AudioPlayer>,
    context_weak: &Weak<SocketContext>,
    guild_id: &str,
    markables: &Arc<Vec<TrackMarkable>>,
    index: usize,
) {
    match &markables[index] {
        TrackMarkable::Segment(segment) => {
            // Only skip while playback is still short of the segment's end: a seek past the whole
            // segment also fires `Bypassed` or `Late`, and acting on that would yank the listener back.
            let position = player_weak
                .upgrade()
                .and_then(|p| p.position())
                .unwrap_or(0);
            if position >= segment.end {
                arm_next(player_weak, context_weak, guild_id, markables, index);
                return;
            }
            if let Some(player) = player_weak.upgrade() {
                player.set_position(segment.end);
            }
            if let Some(context) = context_weak.upgrade() {
                context.send_message(Message::event(EmittedEvent::SegmentSkipped {
                    guild_id: guild_id.to_string(),
                    segment: segment.clone(),
                }));
            }
        }
        TrackMarkable::Chapter(chapter) => {
            if let Some(context) = context_weak.upgrade() {
                context.send_message(Message::event(EmittedEvent::ChapterStarted {
                    guild_id: guild_id.to_string(),
                    chapter: chapter.clone(),
                }));
            }
        }
    }

    arm_next(player_weak, context_weak, guild_id, markables, index);
}

// Arm the marker for the markable after `index`, if there is one.
fn arm_next(
    player_weak: &Weak<AudioPlayer>,
    context_weak: &Weak<SocketContext>,
    guild_id: &str,
    markables: &Arc<Vec<TrackMarkable>>,
    index: usize,
) {
    let next = index + 1;
    if next < markables.len() {
        let next_start = markables[next].start();
        let handler = make_segment_handler(
            player_weak.clone(),
            context_weak.clone(),
            guild_id.to_string(),
            Arc::clone(markables),
            next,
        );
        if let Some(player) = player_weak.upgrade() {
            player.add_marker(TrackMarker::new(next_start, handler));
        }
    }
}

fn make_segment_handler(
    player_weak: Weak<AudioPlayer>,
    context_weak: Weak<SocketContext>,
    guild_id: String,
    markables: Arc<Vec<TrackMarkable>>,
    index: usize,
) -> player::track::marker::TrackMarkerHandler {
    Box::new(move |state: MarkerState| {
        if !matches!(
            state,
            MarkerState::Reached | MarkerState::Late | MarkerState::Bypassed
        ) {
            return;
        }
        handle_markable_at(&player_weak, &context_weak, &guild_id, &markables, index);
    })
}
