use axum::http::StatusCode;
use futures_util::future::join_all;
use serde::Serialize;
use serde_json::{Map, Value};

use player::decode::Millis;
use player::playback::Player;

use super::GuildId;
use super::registry::{PlayerEntry, PlayerRegistry, VoiceState};
use super::settings::CrossfadeSettings;
use crate::tracks::{TrackResponse, encode_snapshot};
use crate::utils::time::now_millis;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayerResponse {
    guild_id: String,
    track: Option<TrackResponse>,
    next_track: Option<TrackResponse>,
    volume: u16,
    paused: bool,
    state: PlayerStateResponse,
    voice: VoiceState,
    filters: Map<String, Value>,
    crossfade: Option<CrossfadeSettings>,
}

#[derive(Serialize)]
struct PlayerStateResponse {
    time: u64,
    position: u64,
    connected: bool,
    ping: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlayerUpdateResponse {
    op: &'static str,
    guild_id: String,
    state: PlayerStateResponse,
}

pub(crate) async fn player_response(player: &PlayerEntry) -> Result<PlayerResponse, StatusCode> {
    let state = player.player().state().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let track = match state.track.as_ref() {
        Some(track) => Some(
            encode_snapshot(track, player.codecs())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        ),
        None => None,
    };
    let next_track = match player.next_track() {
        Some(track) => Some(
            encode_snapshot(&track.snapshot(track.initial_position()), player.codecs())
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        ),
        None => None,
    };
    let voice = player.voice();
    let (connected, ping) = player.connection_state();
    Ok(PlayerResponse {
        guild_id: player.guild_id().to_string(),
        track,
        next_track,
        volume: state.volume,
        paused: state.paused,
        state: player_state(state.position, connected, ping),
        voice,
        filters: player.filters(),
        crossfade: player.crossfade().as_ref().map(CrossfadeSettings::from_engine),
    })
}

pub(crate) async fn player_update_message(player: &PlayerEntry) -> Option<String> {
    player_update_frame(player).await.map(|(_, message)| message)
}

pub(crate) async fn player_update_messages(players: &PlayerRegistry) -> Vec<String> {
    collect_player_updates(players, false).await
}

pub(crate) async fn active_player_update_messages(players: &PlayerRegistry) -> Vec<String> {
    collect_player_updates(players, true).await
}

pub(super) async fn player_update_message_for(
    guild_id: GuildId,
    player: &Player,
    connected: bool,
    ping: i64,
) -> Option<String> {
    let state = player.state().await.ok()?;
    encode_player_update(guild_id, state.position, connected, ping)
}

async fn collect_player_updates(players: &PlayerRegistry, active_only: bool) -> Vec<String> {
    let entries = players.list();
    let updates = join_all(entries.iter().map(|p| player_update_frame(p))).await;
    let mut messages = Vec::with_capacity(updates.len());
    for update in updates {
        let Some((active, message)) = update else {
            continue;
        };
        if !active_only || active {
            messages.push(message);
        }
    }
    messages
}

async fn player_update_frame(player: &PlayerEntry) -> Option<(bool, String)> {
    let state = player.player().state().await.ok()?;
    let active = state.track.is_some();
    let (connected, ping) = player.connection_state();
    let message = encode_player_update(player.guild_id(), state.position, connected, ping)?;
    Some((active, message))
}

fn encode_player_update(
    guild_id: GuildId,
    position: Millis,
    connected: bool,
    ping: i64,
) -> Option<String> {
    serde_json::to_string(&PlayerUpdateResponse {
        op: "playerUpdate",
        guild_id: guild_id.to_string(),
        state: player_state(position, connected, ping),
    })
    .ok()
}

fn player_state(position: Millis, connected: bool, ping: i64) -> PlayerStateResponse {
    PlayerStateResponse { time: now_millis(), position: position.get(), connected, ping }
}
