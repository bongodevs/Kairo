use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinError, JoinHandle};

use crate::config::{DEFAULT_STUCK_THRESHOLD_MS, MIN_STUCK_THRESHOLD_MS, PlayerConfig};
use crate::decode::Millis;
use crate::effects::{CrossfadeOptions, TapeOptions};
use crate::error::PlayerError;
use crate::event::{PlayerEvent, PlayerEvents, TrackEndReason};
use crate::filters::Filters;
use crate::pipeline::{
    FrameConsumer, FrameControl, FrameProducer, Playback, WorkerEnd, frame_buffer,
};
use crate::player::{PlayerOwner, PlayerState};
use crate::source::{OpenedSource, Sources};
use crate::track::Track;
use crate::volume::Volume;

pub(crate) enum Command {
    Play {
        track: Track,
        no_replace: bool,
        reply: oneshot::Sender<Result<bool, PlayerError>>,
    },
    Stop {
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    Pause {
        paused: bool,
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    State {
        reply: oneshot::Sender<Result<PlayerState, PlayerError>>,
    },
    Seek {
        position: Millis,
        reply: oneshot::Sender<Result<bool, PlayerError>>,
    },
    ReplaceMedia {
        playback: Box<OpenedSource>,
        position: Millis,
        reply: oneshot::Sender<Result<bool, PlayerError>>,
    },
    EndTime {
        end_time: Option<Millis>,
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    Volume {
        volume: Volume,
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    Filters {
        filters: Box<Filters>,
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    UserData {
        user_data: Map<String, Value>,
        reply: oneshot::Sender<Result<bool, PlayerError>>,
    },
    Tape {
        options: Option<TapeOptions>,
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    PrepareNext {
        track: Track,
        reply: oneshot::Sender<Result<bool, PlayerError>>,
    },
    ClearNext {
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    Crossfade {
        options: Option<CrossfadeOptions>,
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    ResetCrossfade {
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
    TransitionNow {
        reply: oneshot::Sender<Result<bool, PlayerError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), PlayerError>>,
    },
}

const COMMAND_CAPACITY: usize = 16;
const EVENT_CAPACITY: usize = 16;

#[derive(Clone)]
pub struct Player {
    commands: mpsc::Sender<Command>,
}

impl Player {
    pub async fn play(
        &self,
        track: impl Into<Track>,
        no_replace: bool,
    ) -> Result<bool, PlayerError> {
        self.request(|reply| Command::Play { track: track.into(), no_replace, reply }).await
    }

    pub async fn stop(&self) -> Result<(), PlayerError> {
        self.request(|reply| Command::Stop { reply }).await
    }

    pub async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        self.request(|reply| Command::Pause { paused, reply }).await
    }

    pub async fn state(&self) -> Result<PlayerState, PlayerError> {
        self.request(|reply| Command::State { reply }).await
    }

    pub async fn seek(&self, position: Millis) -> Result<bool, PlayerError> {
        self.request(|reply| Command::Seek { position, reply }).await
    }

    /// Swap the active track's media for an already opened source without restarting the track or
    /// emitting an event. Returns `false` when there is no active track or the source cannot start
    /// at `position`.
    pub async fn replace_media(
        &self,
        playback: OpenedSource,
        position: Millis,
    ) -> Result<bool, PlayerError> {
        self.request(|reply| Command::ReplaceMedia {
            playback: Box::new(playback),
            position,
            reply,
        })
        .await
    }

    pub async fn set_end_time(&self, end_time: Option<Millis>) -> Result<(), PlayerError> {
        self.request(|reply| Command::EndTime { end_time, reply }).await
    }

    pub async fn set_volume(&self, volume: i32) -> Result<(), PlayerError> {
        self.request(|reply| Command::Volume { volume: Volume::new(volume), reply }).await
    }

    pub async fn set_filters(&self, filters: Filters) -> Result<(), PlayerError> {
        self.request(|reply| Command::Filters { filters: Box::new(filters), reply }).await
    }

    pub async fn set_tape(&self, options: Option<TapeOptions>) -> Result<(), PlayerError> {
        self.request(|reply| Command::Tape { options, reply }).await
    }

    /// Pre-buffer the track that should follow the current one. Returns `false` when there is no
    /// active track or no transition configured.
    pub async fn prepare_next(&self, track: impl Into<Track>) -> Result<bool, PlayerError> {
        self.request(|reply| Command::PrepareNext { track: track.into(), reply }).await
    }

    pub async fn clear_next(&self) -> Result<(), PlayerError> {
        self.request(|reply| Command::ClearNext { reply }).await
    }

    /// Replace the per-player transition options. `None` disables transitions for this player.
    pub async fn set_crossfade(
        &self,
        options: Option<CrossfadeOptions>,
    ) -> Result<(), PlayerError> {
        self.request(|reply| Command::Crossfade { options, reply }).await
    }

    /// Restore the transition options from the player configuration.
    pub async fn reset_crossfade(&self) -> Result<(), PlayerError> {
        self.request(|reply| Command::ResetCrossfade { reply }).await
    }

    /// Start the prepared successor now. Returns `false` when none is armed.
    pub async fn transition_now(&self) -> Result<bool, PlayerError> {
        self.request(|reply| Command::TransitionNow { reply }).await
    }

    pub async fn set_user_data(&self, user_data: Map<String, Value>) -> Result<bool, PlayerError> {
        self.request(|reply| Command::UserData { user_data, reply }).await
    }

    async fn shutdown(&self) -> Result<(), PlayerError> {
        self.request(|reply| Command::Shutdown { reply }).await
    }

    async fn request<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, PlayerError>>) -> Command,
    ) -> Result<T, PlayerError> {
        let (reply, response) = oneshot::channel();
        self.commands.send(make(reply)).await.map_err(|_| PlayerError::Stopped)?;
        response.await.map_err(|_| PlayerError::Stopped)?
    }
}

#[must_use = "the guard joins the player task during shutdown"]
pub struct PlayerGuard {
    player: Player,
    join: JoinHandle<()>,
}

impl PlayerGuard {
    pub async fn shutdown(self) -> Result<(), PlayerError> {
        let result = self.player.shutdown().await;
        self.join.await.map_err(|_| PlayerError::WorkerGone)?;
        result
    }
}

pub fn spawn_player(
    sources: Sources,
    config: PlayerConfig,
) -> Result<(Player, FrameConsumer, PlayerEvents, PlayerGuard), PlayerError> {
    config.opus.validate()?;
    let (producer, consumer) = frame_buffer(config.frame_buffer);
    let control = producer.control();
    let stuck_threshold = if config.track_stuck_threshold_ms < MIN_STUCK_THRESHOLD_MS {
        DEFAULT_STUCK_THRESHOLD_MS
    } else {
        config.track_stuck_threshold_ms
    };
    control.set_stuck_threshold(stuck_threshold);
    let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
    let (event_sender, event_receiver) = mpsc::channel(EVENT_CAPACITY);
    let player = Player { commands };
    let join = tokio::spawn(run_player(sources, config, producer, control, receiver, event_sender));
    let guard = PlayerGuard { player: player.clone(), join };
    Ok((player, consumer, PlayerEvents { receiver: event_receiver }, guard))
}

pub(crate) async fn run_player(
    sources: Sources,
    config: PlayerConfig,
    producer: FrameProducer,
    buffer: FrameControl,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<PlayerEvent>,
) {
    let mut owner = PlayerOwner::new(sources, config, producer, buffer, events);
    loop {
        match next_input(&mut owner, &mut commands).await {
            Input::Worker(result) => {
                let _ = owner.worker_completed(*result).await;
            }
            Input::BufferEnd(sequence) => {
                owner.end_seen = sequence;
                let _ = owner.buffer_ended().await;
            }
            Input::Stuck(sequence) => {
                owner.stuck_seen = sequence;
                owner.stuck_detected().await;
            }
            Input::Promotion(sequence) => {
                owner.promotion_seen = sequence;
                owner.process_promotion().await;
            }
            Input::Cleanup => {
                let _ = owner.cleanup_tick().await;
            }
            Input::Command(Some(command)) => {
                let shutdown = matches!(command, Command::Shutdown { .. });
                dispatch(&mut owner, command).await;
                if shutdown {
                    return;
                }
            }
            Input::Command(None) => {
                let _ = owner.stop_with_reason(TrackEndReason::Stopped).await;
                return;
            }
        }
    }
}

enum Input {
    Command(Option<Command>),
    Worker(Box<Result<WorkerEnd, JoinError>>),
    BufferEnd(u64),
    Stuck(u64),
    Promotion(u64),
    Cleanup,
}

async fn next_input(owner: &mut PlayerOwner, commands: &mut mpsc::Receiver<Command>) -> Input {
    let buffer = owner.buffer.clone();
    let seen = owner.end_seen;
    let stuck_seen = owner.stuck_seen;
    let promotion = owner.promotion.clone();
    let promotion_seen = owner.promotion_seen;
    let cleanup_wait = owner.cleanup_wait();
    match &mut owner.playback {
        Playback::Running { join, .. } => {
            tokio::select! {
                biased;
                result = join => Input::Worker(Box::new(result)),
                sequence = buffer.wait_for_end(seen) => Input::BufferEnd(sequence),
                sequence = buffer.wait_for_stuck(stuck_seen) => Input::Stuck(sequence),
                sequence = promotion.wait_for(promotion_seen) => Input::Promotion(sequence),
                () = wait_for_cleanup(cleanup_wait) => Input::Cleanup,
                command = commands.recv() => Input::Command(command),
            }
        }
        _ => {
            tokio::select! {
                biased;
                sequence = buffer.wait_for_end(seen) => Input::BufferEnd(sequence),
                sequence = buffer.wait_for_stuck(stuck_seen) => Input::Stuck(sequence),
                sequence = promotion.wait_for(promotion_seen) => Input::Promotion(sequence),
                () = wait_for_cleanup(cleanup_wait) => Input::Cleanup,
                command = commands.recv() => Input::Command(command),
            }
        }
    }
}

async fn wait_for_cleanup(wait: Option<std::time::Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}

async fn dispatch(owner: &mut PlayerOwner, command: Command) {
    match command {
        Command::Play { track, no_replace, reply } => {
            let _ = reply.send(owner.play(track, no_replace).await);
        }
        Command::Stop { reply } | Command::Shutdown { reply } => {
            let _ = reply.send(owner.stop_with_reason(TrackEndReason::Stopped).await);
        }
        Command::Pause { paused, reply } => {
            let _ = reply.send(owner.pause(paused).await);
        }
        Command::State { reply } => {
            let result = owner.settle().await.map(|()| owner.state());
            let _ = reply.send(result);
        }
        Command::Seek { position, reply } => {
            let _ = reply.send(owner.seek(position).await);
        }
        Command::ReplaceMedia { playback, position, reply } => {
            let _ = reply.send(owner.replace_media(*playback, position).await);
        }
        Command::EndTime { end_time, reply } => {
            let _ = reply.send(owner.set_end_time(end_time));
        }
        Command::Volume { volume, reply } => {
            let _ = reply.send(owner.set_volume(volume).await);
        }
        Command::Filters { filters, reply } => {
            let _ = reply.send(owner.set_filters(*filters).await);
        }
        Command::UserData { user_data, reply } => {
            let updated = owner.set_user_data(user_data);
            let _ = reply.send(Ok(updated));
        }
        Command::Tape { options, reply } => {
            let _ = reply.send(owner.set_tape(options).await);
        }
        Command::PrepareNext { track, reply } => {
            let result = owner.prepare_next(track).await;
            let _ = reply.send(result);
        }
        Command::ClearNext { reply } => {
            let result = owner.clear_next().await;
            let _ = reply.send(result);
        }
        Command::Crossfade { options, reply } => {
            let result = owner.set_crossfade(options).await;
            let _ = reply.send(result);
        }
        Command::ResetCrossfade { reply } => {
            let result = owner.reset_crossfade().await;
            let _ = reply.send(result);
        }
        Command::TransitionNow { reply } => {
            let result = owner.transition_now().await;
            let _ = reply.send(result);
        }
    }
}
