use std::sync::{Arc, Weak};
use std::time::Duration;

use player::decode::Millis;
use player::source::SourceReference;
use player::track::Track;

use super::registry::PlayerEntry;

/// Generated silence that started, or that is armed as the successor.
pub(super) struct Placeholder {
    generation: u64,
    reference: SourceReference,
    resume: Option<Track>,
}

/// The two placeholder slots of a player. Keeping the successor in its own slot means arming it
/// cannot disturb the retry that belongs to the track already playing.
#[derive(Default)]
pub(super) struct Placeholders {
    playing: Option<Placeholder>,
    next: Option<Placeholder>,
    sequence: u64,
}

impl Placeholders {
    fn new_placeholder(&mut self, track: &Track) -> Placeholder {
        self.sequence += 1;
        Placeholder {
            generation: self.sequence,
            reference: track.reference().clone(),
            resume: Some(track.make_clone()),
        }
    }

    pub(super) fn mark_playing(&mut self, track: &Track) {
        if self.next.as_ref().is_some_and(|next| next.reference == *track.reference()) {
            self.next = None;
        }
        let placeholder = self.new_placeholder(track);
        self.playing = Some(placeholder);
    }

    pub(super) fn arm_next(&mut self, track: &Track) {
        let placeholder = self.new_placeholder(track);
        self.next = Some(placeholder);
    }

    /// The armed successor started, so its events now belong to generated silence.
    pub(super) fn promote(&mut self, reference: &SourceReference) -> bool {
        if !self.next.as_ref().is_some_and(|next| &next.reference == reference) {
            return false;
        }
        self.playing = self.next.take();
        true
    }

    pub(super) fn take_resume(&mut self) -> Option<Resume> {
        let placeholder = self.playing.as_mut()?;
        let track = placeholder.resume.take()?;
        Some(Resume {
            generation: placeholder.generation,
            reference: placeholder.reference.clone(),
            track,
        })
    }

    pub(super) fn is_playing(&self, reference: &SourceReference) -> bool {
        self.playing.as_ref().is_some_and(|placeholder| &placeholder.reference == reference)
    }

    pub(super) fn is_generation(&self, generation: u64) -> bool {
        self.playing.as_ref().is_some_and(|placeholder| placeholder.generation == generation)
    }

    pub(super) fn finish(&mut self, generation: u64) {
        if self.is_generation(generation) {
            self.playing = None;
        }
    }

    pub(super) fn clear_playing(&mut self, reference: &SourceReference) {
        if self.is_playing(reference) {
            self.playing = None;
        }
    }

    pub(super) fn clear_next(&mut self) {
        self.next = None;
    }
}

/// A claimed retry. The generation keeps a late attempt from touching a newer placeholder.
pub(super) struct Resume {
    pub(super) generation: u64,
    pub(super) reference: SourceReference,
    pub(super) track: Track,
}

/// Delays before each attempt while the placeholder plays.
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(3),
    Duration::from_secs(6),
    Duration::from_secs(12),
    Duration::from_secs(24),
    Duration::from_secs(30),
];

/// Claim the one pending retry and run it in the background.
pub(crate) fn start(player: &Arc<PlayerEntry>) {
    let Some(resume) = player.take_resume() else {
        return;
    };
    let player = Arc::downgrade(player);
    tokio::spawn(run(player, resume));
}

async fn run(player: Weak<PlayerEntry>, resume: Resume) {
    let Resume { generation, reference, track } = resume;
    for delay in RETRY_DELAYS {
        tokio::time::sleep(delay).await;
        let Some(entry) = player.upgrade() else {
            return;
        };
        if !entry.resume_current(generation) {
            return;
        }
        if position_of(&entry, &reference).await.is_none() {
            return;
        }
        let sources = entry.sources().clone();
        let opened = player::source::without_fallback(sources.open_track(&track)).await;
        let Some(entry) = player.upgrade() else {
            return;
        };
        if !entry.resume_current(generation) {
            return;
        }
        match opened {
            Ok(Some(opened)) => {
                let Some(position) = position_of(&entry, &reference).await else {
                    return;
                };
                swap(&entry, &reference, generation, opened, position).await;
                return;
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(
                guild_id = %entry.guild_id(),
                track = reference.identifier(),
                %error,
                "resume attempt failed; keeping generated silence"
            ),
        }
    }
}

/// Swap real media in. Every outcome ends the retry loop: success removes the placeholder, while a
/// refusal leaves the silence to finish normally.
async fn swap(
    entry: &PlayerEntry,
    reference: &SourceReference,
    generation: u64,
    opened: player::source::OpenedSource,
    position: Millis,
) {
    match entry.player().replace_media(opened, position).await {
        Ok(true) => {
            entry.finish_resume(generation);
            tracing::info!(
                guild_id = %entry.guild_id(),
                track = reference.identifier(),
                "resumed generated silence with real media"
            );
        }
        Ok(false) => tracing::warn!(
            guild_id = %entry.guild_id(),
            track = reference.identifier(),
            "resume media cannot seek to the elapsed position"
        ),
        Err(error) => tracing::warn!(
            guild_id = %entry.guild_id(),
            track = reference.identifier(),
            %error,
            "replacing generated silence failed"
        ),
    }
}

/// The current position, but only while the placeholder track is still the active one.
async fn position_of(entry: &PlayerEntry, reference: &SourceReference) -> Option<Millis> {
    let state = entry.player().state().await.ok()?;
    let track = state.track?;
    (track.reference() == reference).then_some(state.position)
}

#[cfg(test)]
#[path = "../tests/player_placeholders.rs"]
mod tests;
