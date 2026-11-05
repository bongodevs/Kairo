use super::*;
use crate::utils::locked;

// SponsorBlock lookups are best-effort and network-bound. One permit keeps them from occupying more
// than one shared blocking slot when a guild races through tracks or the endpoint stops responding.
static SPONSORBLOCK_PERMIT: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(1));

impl AudioEventListener for PlayerShared {
    fn on_event(&self, event: &AudioEvent) {
        // Loss-window bookkeeping first: it has to keep counting even once the socket has gone away.
        self.track_loss_window(event);

        let Some(context) = self.context.upgrade() else {
            return;
        };
        // The v4 protocol has no pause or resume event, so return before allocating the guild-id
        // string; both can be high-frequency.
        if matches!(event, AudioEvent::PlayerPause | AudioEvent::PlayerResume) {
            return;
        }
        let guild_id = self.guild_id.to_string();

        match event {
            AudioEvent::TrackStart(_) => {
                // Phase the periodic update cadence to this track rather than to process start.
                if let Some(player) = context.get_player(self.guild_id) {
                    player.start_update_task();
                }
                if let Some(track) = self.current_track() {
                    // Read what the post-send work needs before the track moves into the event. Only
                    // a normal play seeds lyrics; a transition successor seeds on TrackPromoted.
                    let lyrics_seed = if !self.in_transition.load(Ordering::Acquire) {
                        self.lyrics()
                            .filter(|lyrics| lyrics.is_subscribed())
                            .map(|lyrics| (lyrics, query_from_info(&track.info)))
                    } else {
                        None
                    };

                    let sponsorblock = if track.info.source_name == "youtube" {
                        context
                            .get_sponsorblock_categories(self.guild_id)
                            .map(|categories| {
                                (
                                    track.info.identifier.clone(),
                                    track.info.length as u64,
                                    categories,
                                )
                            })
                    } else {
                        None
                    };

                    context.send_message(Message::event(EmittedEvent::TrackStart {
                        guild_id: guild_id.clone(),
                        track,
                    }));

                    if let Some((lyrics, query)) = lyrics_seed {
                        lyrics.seed(query);
                    }

                    if let Some((video_id, track_duration, categories)) = sponsorblock {
                        let player_weak = self.player.clone();
                        let context_weak = self.context.clone();
                        let gid = self.guild_id;
                        let gid_str = guild_id.clone();

                        // Arming runs on the runtime and the fetch on the blocking pool, never on
                        // the decode thread that is delivering this event.
                        self.runtime.spawn(async move {
                            let Some(context) = context_weak.upgrade() else {
                                return;
                            };
                            if let Some(markables) =
                                context.get_sponsorblock_markables(&video_id, &categories)
                            {
                                Self::set_sponsorblock_marker(
                                    &player_weak,
                                    &context_weak,
                                    gid_str,
                                    &markables,
                                    0,
                                );
                                return;
                            }
                            drop(context);

                            // Serialise lookups: a wedged SponsorBlock endpoint must not occupy
                            // every shared blocking slot that loads and MLS keygen need.
                            let _permit = SPONSORBLOCK_PERMIT.acquire().await;
                            let _ = tokio::task::spawn_blocking(move || {
                                Self::handle_sponsorblock(
                                    player_weak,
                                    context_weak,
                                    gid,
                                    gid_str,
                                    video_id,
                                    track_duration,
                                    categories,
                                );
                            })
                            .await;
                        });
                    }
                }
            }
            AudioEvent::TrackEnd(_, reason) => {
                // Transition events carry the outgoing track separately because the protocol
                // current slot has already moved to the successor at overlap start.
                let transition = matches!(
                    reason,
                    EngineEndReason::Crossfade | EngineEndReason::Gapless
                );
                let track = if transition {
                    let outgoing = locked(&self.transition_outgoing).take();
                    if let Some(incoming) = locked(&self.transition_incoming).take() {
                        let mut slots = locked(&self.tracks);
                        slots.previous = slots.current.take();
                        slots.current = Some(incoming);
                    }
                    outgoing
                } else if matches!(reason, EngineEndReason::Replaced) {
                    self.previous_track().or_else(|| self.current_track())
                } else {
                    self.current_track()
                };

                // An `endTime` marker stops the track but should report as `Finished`.
                let end_reason = if matches!(reason, EngineEndReason::Stopped)
                    && self.end_marker_hit.swap(false, Ordering::AcqRel)
                {
                    TrackEndReason::Finished
                } else {
                    TrackEndReason::from_player(*reason)
                };

                // The client advances its queue only on `trackEnd`, so a swallowed event stalls that
                // guild forever. Fall back through both slots, and say so loudly if both are empty.
                let track = track
                    .or_else(|| self.current_track())
                    .or_else(|| self.previous_track());

                match track {
                    Some(track) => context.send_message(Message::event(EmittedEvent::TrackEnd {
                        guild_id,
                        track,
                        reason: end_reason,
                    })),
                    None => tracing::error!(
                        guild_id = self.guild_id,
                        ?reason,
                        "TrackEnd with no resolvable track; the client queue will stall"
                    ),
                }

                // The current successor remains active after a transition. All other ends clear
                // the current slot unless the event merely reports a replacement.
                if !matches!(
                    reason,
                    EngineEndReason::Replaced
                        | EngineEndReason::Crossfade
                        | EngineEndReason::Gapless
                ) {
                    self.clear_current();
                    // Nothing is playing now, so stop the periodic timer. The excluded reasons all
                    // have a successor whose own `TrackStart` re-arms it.
                    if let Some(player) = context.get_player(self.guild_id) {
                        player.stop_update_task();
                    }
                }

                // Invalidate the outgoing track's lyrics on every end: through a crossfade the old
                // executor keeps firing line markers, so its stale lines would leak out afterwards.
                if !matches!(reason, EngineEndReason::Replaced) {
                    if let Some(lyrics) = self.lyrics() {
                        lyrics.unsubscribe_keep_flag();
                    }
                }
            }
            AudioEvent::TrackException(info, error) => {
                // Attribute the exception to whichever slot actually failed: a prepared gapless
                // successor can fault while the base track is still playing perfectly well.
                let failed = self
                    .current_track()
                    .filter(|track| track.info.identifier == info.identifier)
                    .or_else(|| {
                        locked(&self.transition_incoming)
                            .clone()
                            .filter(|track| track.info.identifier == info.identifier)
                    });
                match failed {
                    Some(track) => {
                        context.send_message(Message::event(EmittedEvent::TrackException {
                            guild_id,
                            track,
                            exception: Exception::from_friendly(error),
                        }));
                    }
                    // No protocol track to attach: emitting against the wrong one is worse than
                    // logging, since the client would skip a healthy track.
                    None => tracing::warn!(
                        guild_id = self.guild_id,
                        identifier = %info.identifier,
                        "TrackException for a track this player does not hold"
                    ),
                }
            }
            AudioEvent::TrackStuck(_, threshold_ms) => {
                if let Some(track) = self.current_track() {
                    context.send_message(Message::event(EmittedEvent::TrackStuck {
                        guild_id,
                        track,
                        threshold_ms: *threshold_ms as i64,
                    }));
                }
                // Follow the event with a player update, so a client sees where playback stalled.
                if let Some(player) = context.get_player(self.guild_id) {
                    player.send_player_update();
                }
            }
            AudioEvent::TrackPromoted(_) => {
                // The successor is the active executor now, so the client re-anchors position off this
                // event; without it, position keeps drifting from the outgoing track.
                if let Some(track) = self.current_track() {
                    context.send_message(Message::event(EmittedEvent::TrackPromoted {
                        guild_id,
                        track,
                    }));
                }

                if let Some(player) = context.get_player(self.guild_id) {
                    if let Some(id) = locked(&player.manual_transition_marker).take() {
                        player.player.remove_marker(id);
                    }
                    player.arm_transition();

                    // A track that transitioned in becomes the active executor here, so seed now.
                    if self.in_transition.swap(false, Ordering::AcqRel) {
                        if let Some(lyrics) = self.lyrics() {
                            if lyrics.is_subscribed() {
                                if let Some(track) = self.current_track() {
                                    lyrics.seed(query_from_info(&track.info));
                                }
                            }
                        }
                    }
                }
            }
            // Returned early above.
            AudioEvent::PlayerPause | AudioEvent::PlayerResume => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SPONSORBLOCK_PERMIT;

    // A wedged endpoint must not be able to take a second shared blocking slot from a lookup queue.
    #[tokio::test]
    async fn sponsorblock_lookups_hold_one_permit_at_a_time() {
        let permit = SPONSORBLOCK_PERMIT.acquire().await.expect("permit");
        assert!(SPONSORBLOCK_PERMIT.try_acquire().is_err());
        drop(permit);
        assert!(SPONSORBLOCK_PERMIT.try_acquire().is_ok());
    }
}
