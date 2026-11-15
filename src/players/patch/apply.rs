use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use player::decode::Millis;
use player::source::{LoadOutcome, SourceReference};
use player::track::Track;
use player::track::codec::{self, DecodeTrack};

use super::super::GuildId;
use super::super::messages::{player_response, player_update_message};
use super::super::registry::PlayerEntry;
use super::super::settings::TapeSettings;
use super::input::Omissible;
use super::input::{PatchQuery, PlayerUpdate};
use crate::app::AppState;
use crate::rest::extract::{ApiJson, ApiPath, ApiQuery};
use crate::tracks::source_name;

pub(crate) async fn patch_player(
    State(state): State<AppState>,
    ApiPath((session_id, guild_id)): ApiPath<(String, u64)>,
    ApiQuery(query): ApiQuery<PatchQuery>,
    ApiJson(update): ApiJson<PlayerUpdate>,
) -> Response {
    let guild_id = GuildId::new(guild_id);
    let PlayerUpdate {
        track,
        encoded_track,
        identifier,
        position,
        end_time,
        volume,
        paused,
        filters,
        voice,
        next_track,
        crossfade,
        tape,
        transition,
    } = update;
    if track.is_present() && (encoded_track.is_present() || identifier.is_present()) {
        return bad_request("Cannot specify both track and encodedTrack/identifier");
    }
    let (encoded, identifier, mut user_data) = match track {
        Omissible::Present(track) => (track.encoded, track.identifier, track.user_data),
        Omissible::Omitted => (encoded_track, identifier, Omissible::Omitted),
    };
    if encoded.is_present() && identifier.is_present() {
        return bad_request("Cannot specify both encodedTrack and identifier");
    }
    let replacing_track = encoded.is_present() || identifier.is_present();
    let transition_now = matches!(transition, Omissible::Present(true));
    if transition_now && replacing_track {
        return bad_request("Cannot trigger a transition while replacing the current track");
    }
    let next_track_present = next_track.is_present();
    let crossfade_present = crossfade.is_present();
    let next_track = match next_track {
        Omissible::Omitted => None,
        Omissible::Present(None) => Some(None),
        Omissible::Present(Some(spec)) => {
            let mut track = match resolve_track(&state, spec.encoded, spec.identifier).await {
                Ok(Some(track)) => track,
                Ok(None) => return bad_request("A nextTrack must identify a track"),
                Err(message) => return bad_request(&message),
            };
            if let Omissible::Present(user_data) = spec.user_data {
                track.set_user_data(user_data);
            }
            Some(Some(track))
        }
    };
    let filters = match filters {
        Omissible::Omitted => None,
        Omissible::Present(filters) => {
            let disabled = filters.disabled_by(state.filter_policy());
            if !disabled.is_empty() {
                return bad_request(&format!(
                    "Following filters are disabled in the config: {}",
                    disabled.join(", ")
                ));
            }
            Some(filters)
        }
    };
    let voice = match voice {
        Omissible::Present(voice)
            if voice.token.trim().is_empty()
                || voice.endpoint.trim().is_empty()
                || voice.session_id.trim().is_empty()
                || voice
                    .channel_id
                    .as_deref()
                    .is_none_or(|channel_id| channel_id.trim().is_empty()) =>
        {
            return bad_request(
                "token, endpoint, sessionId and channelId must be provided in voice state",
            );
        }
        Omissible::Present(voice) => {
            let channel_id = match voice
                .channel_id
                .as_deref()
                .and_then(|channel_id| channel_id.parse::<u64>().ok())
            {
                Some(channel_id) => channel_id,
                None => return bad_request("channelId must be an unsigned integer"),
            };
            Some((voice, channel_id))
        }
        Omissible::Omitted => None,
    };
    if let Omissible::Present(Some(0)) = end_time {
        return bad_request("End time must be greater than 0");
    }
    let filters = match filters {
        Some(filters) => match filters.into_parts() {
            Ok(filters) => Some(filters),
            Err(message) => return bad_request(&message),
        },
        None => None,
    };

    let player = match state.sessions.create_player(
        &session_id,
        guild_id,
        state.sources().clone(),
        state.codec_registry(),
        state.player_config(),
    ) {
        Ok(Some(player)) => player,
        Ok(None) => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Some((voice, channel_id)) = voice {
        let Some(user_id) = state.sessions.user_id(&session_id) else {
            return (StatusCode::NOT_FOUND, "Session not found").into_response();
        };
        match player
            .connect_voice(channel_id, user_id, voice, state.sessions.clone(), session_id.clone())
            .await
        {
            Ok(true) => send_player_update(&state, &session_id, &player).await,
            Ok(false) => {}
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to connect to voice server")
                    .into_response();
            }
        }
    }
    if let Omissible::Present(crossfade) = crossfade {
        match crossfade {
            Some(settings) => {
                let options = settings.to_engine();
                if player.player().set_crossfade(options).await.is_err() {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                player.set_crossfade(options);
            }
            None => {
                if player.player().reset_crossfade().await.is_err() {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                player.set_crossfade(state.player_config().crossfade);
            }
        }
    }
    if let Omissible::Present(tape) = tape {
        let options = tape.and_then(TapeSettings::to_engine);
        if player.player().set_tape(options).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if let Some(next) = next_track {
        match next {
            Some(track) => player.set_next_track(Some(track)),
            None => {
                let _ = player.player().clear_next().await;
                player.set_next_track(None);
            }
        }
    }
    let arm_next = !replacing_track && (next_track_present || crossfade_present);
    if arm_next {
        arm_next_track(&player).await;
    }
    if transition_now {
        match player.player().transition_now().await {
            Ok(true) => {}
            Ok(false) => {
                return bad_request("No active crossfade/gapless transition is available");
            }
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
    if !replacing_track {
        if let Omissible::Present(paused) = paused
            && player.player().set_paused(paused).await.is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        if let Omissible::Present(user_data) = std::mem::take(&mut user_data)
            && player.player().set_user_data(user_data).await.is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if let Omissible::Present(volume) = volume
        && player.player().set_volume(volume).await.is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if !replacing_track {
        if let Omissible::Present(position) = position {
            match player.player().seek(Millis::new(position)).await {
                Ok(true) => {
                    player.set_next_track(None);
                    send_player_update(&state, &session_id, &player).await;
                }
                Ok(false) => {}
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        if let Omissible::Present(end_time) = end_time
            && player.player().set_end_time(end_time.map(Millis::new)).await.is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if let Some((filters, wire)) = filters {
        if player.player().set_filters(filters).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        player.set_filters(wire);
        send_player_update(&state, &session_id, &player).await;
    }
    if replacing_track {
        let current = match player.player().state().await {
            Ok(state) => state,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        if query.no_replace && current.track.is_some() {
            return player_result(&state, &session_id, guild_id, &player).await;
        }
        let pause = match paused {
            Omissible::Present(paused) => paused,
            Omissible::Omitted => false,
        };
        if player.player().set_paused(pause).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let mut track = match resolve_track(&state, encoded, identifier).await {
            Ok(track) => track,
            Err(message) => return bad_request(&message),
        };
        if let Some(track) = &mut track {
            if let Omissible::Present(position) = position {
                track.set_initial_position(Millis::new(position));
            }
            if let Omissible::Present(user_data) = user_data {
                track.set_user_data(user_data);
            }
        }
        if player
            .player()
            .set_end_time(match end_time {
                Omissible::Present(end_time) => end_time.map(Millis::new),
                Omissible::Omitted => None,
            })
            .await
            .is_err()
        {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let started = track.is_some();
        let played_source =
            track.as_ref().map(|track| source_name(track).to_owned()).unwrap_or_default();
        let result = match track {
            Some(track) => player.player().play(track, false).await.map(|_| ()),
            None => player.player().stop().await,
        };
        if result.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        if started {
            if state.metrics_endpoint.is_some() {
                state.search_metrics.record_play(&played_source, &guild_id.to_string());
            }
            send_player_update(&state, &session_id, &player).await;
            arm_next_track(&player).await;
        } else {
            player.set_next_track(None);
        }
    }
    player.notify_voice_frames().await;
    player_result(&state, &session_id, guild_id, &player).await
}

/// Arm the stored successor against the player's current state. The track stays stored so it can be
/// listed and retried; the player only reports `false` when no transition applies right now.
async fn arm_next_track(player: &PlayerEntry) {
    let Some(track) = player.take_next_track() else {
        return;
    };
    let keep = track.make_clone();
    let _ = player.player().prepare_next(track).await;
    player.set_next_track(Some(keep));
}

async fn resolve_track(
    state: &AppState,
    encoded: Omissible<Option<String>>,
    identifier: Omissible<String>,
) -> Result<Option<Track>, String> {
    match (encoded, identifier) {
        (Omissible::Present(Some(encoded)), Omissible::Omitted) => {
            match codec::decode(&encoded, state.codecs()) {
                Ok(DecodeTrack::Track(track)) => Ok(Some(track)),
                Ok(_) | Err(_) => Err(String::from("Invalid encoded track")),
            }
        }
        (Omissible::Present(None), Omissible::Omitted) => Ok(None),
        (Omissible::Omitted, Omissible::Present(identifier)) => {
            match state.sources().load(SourceReference::new(identifier)).await {
                LoadOutcome::Track(track) => Ok(Some(track)),
                LoadOutcome::Empty => Err(String::from("No matches found for identifier")),
                LoadOutcome::Playlist(_) | LoadOutcome::Search(_) => {
                    Err(String::from("Cannot play a playlist or search result"))
                }
                LoadOutcome::Error(error) => Err(error.to_string()),
                _ => Err(String::from("Unknown audio item type")),
            }
        }
        _ => Ok(None),
    }
}

async fn player_result(
    state: &AppState,
    session_id: &str,
    guild_id: GuildId,
    player: &PlayerEntry,
) -> Response {
    if let Ok(snapshot) = player.player().state().await {
        state.sessions.set_player_playing(
            session_id,
            guild_id,
            snapshot.track.is_some() && !snapshot.paused,
        );
    }
    match player_response(player).await {
        Ok(player) => Json(player).into_response(),
        Err(status) => status.into_response(),
    }
}

async fn send_player_update(state: &AppState, session_id: &str, player: &PlayerEntry) {
    if let Some(message) = player_update_message(player).await {
        state.sessions.send_update(session_id, player.guild_id(), message);
    }
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, message.to_owned()).into_response()
}

#[cfg(test)]
#[path = "../../tests/player_filter_patch.rs"]
mod filter_tests;
#[cfg(test)]
#[path = "../../tests/player_patch.rs"]
mod tests;
