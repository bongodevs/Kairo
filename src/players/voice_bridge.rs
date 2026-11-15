use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::task::JoinHandle;

use player::buffer::FrameConsumer;
use player::playback::Player;
use voice::VoiceConfig;
use voice::connect::{Connection, Frames, Lifecycle};
use voice::media::FrameSource;
use voice::{GatewaySocket, VoiceTarget};

use super::GuildId;
use super::frame_stats::FrameCounter;
use super::messages::player_update_message_for;
use crate::session::SessionStore;

pub(super) struct VoiceConnection {
    frames: Frames,
    lifecycle: Lifecycle,
    task: JoinHandle<FrameConsumer>,
}

pub(super) struct VoiceBridge {
    pub(super) connected: Arc<AtomicBool>,
    pub(super) ping: Arc<AtomicI64>,
    pub(super) sessions: Arc<SessionStore>,
    pub(super) session_id: String,
    pub(super) player: Player,
    pub(super) frame_counter: Arc<FrameCounter>,
}

impl VoiceConnection {
    pub(super) async fn connect(
        target: VoiceTarget,
        frames: FrameConsumer,
        bridge: VoiceBridge,
    ) -> Result<Self, (String, Box<FrameConsumer>)> {
        let config = VoiceConfig {
            gateway_connect_timeout: Duration::from_secs(3),
            ..VoiceConfig::default()
        };
        let mut connection = match Connection::<GatewaySocket>::connect(&target, config).await {
            Ok(connection) => connection,
            Err(error) => return Err((error.to_string(), Box::new(frames))),
        };
        let lifecycle = connection.lifecycle();
        let notifier = connection.frames();
        let task_notifier = notifier.clone();
        let heartbeat_ping = connection.ping_handle();
        let guild_id = GuildId::new(target.guild.0);
        log_voice_connected(&bridge.session_id, guild_id, target.channel.0);
        let VoiceBridge { connected, ping, sessions, session_id, player, frame_counter } = bridge;
        ping.store(duration_millis(connection.ping()), Ordering::Release);
        connected.store(true, Ordering::Release);
        let task = tokio::spawn(async move {
            let mut source = PlayerFrames { frames, counter: frame_counter };
            let _ = task_notifier.notify().await;
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            {
                let mut run = Box::pin(connection.run_guarded(&mut source));
                loop {
                    tokio::select! {
                        result = &mut run => {
                            match result {
                                Ok(disconnect) => {
                                    log_voice_closed(&session_id, guild_id, &disconnect);
                                    if let Some(message) = websocket_closed_message(guild_id, &disconnect) {
                                        sessions.send_lifecycle(&session_id, message).await;
                                    }
                                }
                                Err(error) => tracing::warn!(
                                    session_id = %session_id,
                                    guild_id = %guild_id,
                                    %error,
                                    "voice connection failed"
                                ),
                            }
                            break;
                        }
                        _ = ticker.tick() => {
                            ping.store(
                                i64::try_from(heartbeat_ping.millis()).unwrap_or(i64::MAX),
                                Ordering::Release,
                            );
                        }
                    }
                }
            }
            connected.store(false, Ordering::Release);
            ping.store(-1, Ordering::Release);
            if let Some(message) = player_update_message_for(guild_id, &player, false, -1).await {
                sessions.send_update(&session_id, guild_id, message);
            }
            source.frames
        });
        Ok(Self { frames: notifier, lifecycle, task })
    }

    pub(super) fn frames(&self) -> Frames {
        self.frames.clone()
    }

    pub(super) async fn close(self) -> Option<FrameConsumer> {
        let _ = self.lifecycle.disconnect().await;
        self.task.await.ok()
    }
}

struct PlayerFrames {
    frames: FrameConsumer,
    counter: Arc<FrameCounter>,
}

impl FrameSource for PlayerFrames {
    fn provide(&mut self, output: &mut [u8]) -> Option<usize> {
        let frame = self.frames.provide(output);
        self.counter.record(frame.is_some());
        frame
    }

    fn is_active(&self) -> bool {
        self.frames.is_playing()
    }
}

fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn log_voice_connected(session_id: &str, guild_id: GuildId, channel: u64) {
    tracing::info!(
        session_id,
        guild_id = %guild_id,
        channel,
        "voice connection established"
    );
}

fn log_voice_closed(session_id: &str, guild_id: GuildId, disconnect: &voice::Disconnect) {
    tracing::info!(
        session_id,
        guild_id = %guild_id,
        code = disconnect.code,
        reason = %disconnect.reason,
        by_remote = disconnect.by_remote,
        "voice connection closed"
    );
}

fn websocket_closed_message(guild_id: GuildId, disconnect: &voice::Disconnect) -> Option<String> {
    serde_json::to_string(&json!({
        "op": "event",
        "type": "WebSocketClosedEvent",
        "guildId": guild_id.to_string(),
        "code": disconnect.code,
        "reason": disconnect.reason,
        "byRemote": disconnect.by_remote,
    }))
    .ok()
}

#[cfg(test)]
#[path = "../tests/player_voice_bridge.rs"]
mod tests;
