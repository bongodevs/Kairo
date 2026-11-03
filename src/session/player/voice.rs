//! Discord voice connection handling and audio frame dispatch.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use bytes::Bytes;
use voice::{
    EventDispatcher, OpusFrameProvider, VoiceConnection, VoiceEvent, VoiceEventListener,
    VoiceServerInfo,
};

use super::{AudioLossCounter, AudioPlayer, LavalinkPlayer, SocketContext};
use crate::protocol::message::{EmittedEvent, Message};
use crate::protocol::player::VoiceState;
use crate::utils::locked;

/// An error establishing a Discord voice connection.
#[derive(Debug)]
pub enum VoiceConnectError {
    /// The voice handshake timed out.
    Timeout(Duration),
    /// Establishing the voice connection failed.
    ConnectionFailed(String),
}

impl std::fmt::Display for VoiceConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout(duration) => {
                write!(
                    f,
                    "voice handshake timed out after {} ms",
                    duration.as_millis()
                )
            }
            Self::ConnectionFailed(err) => write!(f, "voice connection failed: {err}"),
        }
    }
}

impl std::error::Error for VoiceConnectError {}

// Feeds the voice send loop from the engine player while recording frame stats.
struct CountingProvider {
    player: Arc<AudioPlayer>,
    loss: Arc<AudioLossCounter>,
}

impl OpusFrameProvider for CountingProvider {
    fn provide(&mut self) -> Option<Bytes> {
        let frame = self.player.try_provide();
        // Only count while a track is playing: idle silence is expected, not loss.
        if self.player.playing_track().is_some() {
            match &frame {
                Some(_) => self.loss.record_success(),
                None => self.loss.record_loss(),
            }
        }
        frame.map(|frame| frame.data)
    }
}

// Discord invalidates the voice session with 4006; a fresh IDENTIFY is the only recovery, and the
// higher layer owns the voice-server info needed to run it.
const REIDENTIFY_CLOSE_CODE: u16 = 4006;
const MAX_REIDENTIFY_ATTEMPTS: u32 = 3;

// Forward voice gateway lifecycle events to the WebSocket.
struct VoiceCloseForwarder {
    guild_id: u64,
    context: Weak<SocketContext>,
}

impl VoiceEventListener for VoiceCloseForwarder {
    fn on_event(&self, event: &VoiceEvent) {
        let Some(context) = self.context.upgrade() else {
            return;
        };
        match event {
            VoiceEvent::GatewayClosed {
                code,
                reason,
                by_remote,
            } => {
                // A 4006 means the gateway forgot this session; resuming with op 7 would fail, so
                // rebuild the connection with a fresh IDENTIFY. Only report the close if that
                // attempt fails or the budget is gone.
                if let Some(player) = context.get_player(self.guild_id) {
                    if let Some(voice) = locked(&player.voice_state).clone() {
                        if player.begin_voice_reidentify(*code) {
                            tracing::warn!(
                                guild_id = self.guild_id,
                                "voice: session invalidated (4006), re-identifying"
                            );
                            let context = Arc::clone(&context);
                            let player = Arc::clone(&player);
                            let guild_id = self.guild_id;
                            let reason = reason.clone();
                            let code = *code;
                            let by_remote = *by_remote;
                            tokio::spawn(async move {
                                match player.connect_voice(voice).await {
                                    Ok(true) => {
                                        tracing::info!(guild_id, "voice: re-identified after 4006")
                                    }
                                    Ok(false) => {
                                        tracing::debug!(guild_id, "voice: re-identify superseded")
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            guild_id,
                                            error = %error,
                                            "voice: re-identify failed"
                                        );
                                        forward_gateway_closed(
                                            &context, guild_id, code, &reason, by_remote,
                                        );
                                    }
                                }
                            });
                            return;
                        }
                    }
                }
                forward_gateway_closed(&context, self.guild_id, *code, reason, *by_remote);
            }
            VoiceEvent::GatewayReady { .. } => {
                // The player may already be gone, with a destroy racing the close.
                if let Some(player) = context.get_player(self.guild_id) {
                    context.send_message(player.player_update_message());
                }
            }
            // Every other voice event is internal to the connection.
            _ => {}
        }
    }
}

fn forward_gateway_closed(
    context: &SocketContext,
    guild_id: u64,
    code: u16,
    reason: &str,
    by_remote: bool,
) {
    context.send_message(Message::event(EmittedEvent::WebSocketClosed {
        guild_id: guild_id.to_string(),
        code: code as i32,
        reason: reason.to_string(),
        by_remote,
    }));
    if let Some(player) = context.get_player(guild_id) {
        context.send_message(player.player_update_message());
    }
}

impl LavalinkPlayer {
    /// Whether `voice` differs from the current state or the connection dropped.
    ///
    /// Cheap synchronous check used before paying for a handshake and again when committing one.
    /// Both checks dedupe repeated `VOICE_SERVER_UPDATE` PATCHes, so a handshake that finished
    /// against a superseded target cannot displace the newer connection.
    pub fn voice_reconnect(&self, voice: &VoiceState) -> bool {
        let current = locked(&self.voice_state);
        match current.as_ref() {
            Some(existing) => {
                existing.token != voice.token
                    || existing.endpoint != voice.endpoint
                    || existing.session_id != voice.session_id
                    || existing.channel_id != voice.channel_id
                    || !self.is_connected()
            }
            None => true,
        }
    }

    // Spend one 4006 re-identify attempt. Other close codes are never re-identified here.
    pub(super) fn begin_voice_reidentify(&self, code: u16) -> bool {
        code == REIDENTIFY_CLOSE_CODE
            && self
                .voice_reidentify_attempts
                .fetch_add(1, Ordering::AcqRel)
                < MAX_REIDENTIFY_ATTEMPTS
    }

    // A fresh handshake succeeded, so the next 4006 gets a full budget again.
    pub(super) fn reset_voice_reidentify(&self) {
        self.voice_reidentify_attempts.store(0, Ordering::Release);
    }

    /// Run the Discord gateway + UDP handshake. No player locks are held across the await.
    async fn handshake_voice(
        &self,
        voice: &VoiceState,
    ) -> Result<VoiceConnection, VoiceConnectError> {
        let channel_id = voice
            .channel_id
            .as_deref()
            .and_then(|id| id.parse::<u64>().ok())
            .unwrap_or(0);
        let info = VoiceServerInfo {
            guild_id: self.guild_id,
            user_id: self.user_id,
            channel_id,
            session_id: voice.session_id.clone(),
            token: voice.token.clone(),
            endpoint: voice.endpoint.clone(),
        };

        let dispatcher = EventDispatcher::new();
        dispatcher.register(Arc::new(VoiceCloseForwarder {
            guild_id: self.guild_id,
            context: self.shared.context.clone(),
        }) as Arc<dyn VoiceEventListener>);

        let provider = CountingProvider {
            player: Arc::clone(&self.player),
            loss: Arc::clone(&self.loss),
        };

        let started = std::time::Instant::now();
        let result = VoiceConnection::connect_with_dispatcher(info, provider, dispatcher).await;
        let elapsed_ms = started.elapsed().as_millis();
        match result {
            Ok(connection) => {
                tracing::debug!(
                    guild_id = self.guild_id,
                    elapsed_ms,
                    "voice handshake succeeded"
                );
                Ok(connection)
            }
            Err(err) => {
                tracing::warn!(
                    guild_id = self.guild_id,
                    elapsed_ms,
                    error = %err,
                    "voice handshake failed"
                );
                Err(VoiceConnectError::ConnectionFailed(err.to_string()))
            }
        }
    }

    /// Store a finished handshake. The caller must hold `lock_patch`.
    ///
    /// Drops `connection` and returns `false` when the guild no longer needs this target, so a
    /// handshake that ran against a superseded voice state never displaces the live connection.
    fn commit_voice(&self, voice: VoiceState, connection: VoiceConnection) -> bool {
        if !self.voice_reconnect(&voice) {
            connection.disconnect();
            return false;
        }
        if let Some(previous) = locked(&self.voice).take() {
            previous.disconnect();
        }
        connection.set_speaking(true);
        *locked(&self.voice) = Some(connection);
        *locked(&self.voice_state) = Some(voice);
        self.reset_voice_reidentify();
        true
    }

    // Outer bound on one voice handshake.
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

    /// Connect voice and store the connection inline on the calling PATCH.
    pub async fn connect_voice(&self, voice: VoiceState) -> Result<bool, VoiceConnectError> {
        let connection =
            match tokio::time::timeout(Self::HANDSHAKE_TIMEOUT, self.handshake_voice(&voice)).await
            {
                Ok(Ok(connection)) => connection,
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(VoiceConnectError::Timeout(Self::HANDSHAKE_TIMEOUT));
                }
            };
        let _serialized = self.lock_patch().await;
        Ok(self.commit_voice(voice, connection))
    }
}
