//! Player state and the player update body.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::filters::Filters;
use crate::protocol::omissible::Omissible;
use crate::protocol::track::Track;

/// A player's full state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    /// The guild id this player belongs to.
    pub guild_id: String,
    /// The currently loaded track, if any.
    pub track: Option<Track>,
    /// The volume, `0..=1000`.
    pub volume: i32,
    /// Whether the player is paused.
    pub paused: bool,
    /// The live player state.
    pub state: PlayerState,
    /// The Discord voice state.
    pub voice: VoiceState,
    /// The active filters.
    pub filters: Filters,
    /// The transition options in effect, or `null` when transitions are off. Always present, so a
    /// client can tell an unconfigured player from an unreported one.
    pub crossfade: Option<CrossfadeSettings>,
}

/// The live player state, broadcast on a timer.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    /// Current server time as a unix millisecond timestamp.
    pub time: i64,
    /// Current track position in milliseconds.
    pub position: i64,
    /// Whether the voice gateway is connected.
    pub connected: bool,
    /// Voice gateway round-trip latency in milliseconds, or `-1` when not known.
    pub ping: i64,
}

/// The Discord voice connection state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceState {
    /// The voice token from `VOICE_SERVER_UPDATE`.
    pub token: String,
    /// The voice endpoint host from `VOICE_SERVER_UPDATE`.
    pub endpoint: String,
    /// The voice session id from `VOICE_STATE_UPDATE`.
    pub session_id: String,
    /// The voice channel id, if connected.
    #[serde(default)]
    pub channel_id: Option<String>,
}

impl VoiceState {
    /// Whether every connection field is set. A partial update is ignored, not half applied.
    pub fn is_complete(&self) -> bool {
        !self.token.is_empty()
            && !self.endpoint.is_empty()
            && !self.session_id.is_empty()
            && self
                .channel_id
                .as_deref()
                .is_some_and(|id| id.parse::<u64>().is_ok())
    }
}

/// The `track` sub-object of a [`PlayerUpdate`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerUpdateTrack {
    /// Base64 encoded track to play, or `null` to stop.
    pub encoded: Omissible<Option<String>>,
    /// An identifier to load and play.
    pub identifier: Omissible<String>,
    /// Arbitrary user data to attach to the track.
    pub user_data: Omissible<Map<String, Value>>,
}

/// Fade easing for a [`CrossfadeSettings`] transition, named as in the `crossfade.*` config block.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CrossfadeCurve {
    /// `f(x) = x`, a straight ramp.
    Linear,
    /// `f(x) = x²`, fast start, slow end.
    Exp,
    /// `f(x) = √x`, slow start, fast end.
    Log,
    /// Raised-cosine S-curve, the default.
    #[default]
    SCurve,
    /// `f(x) = sin(πx/2)` fading in against `cos(πx/2)` fading out, which holds the overlap at
    /// constant power instead of dipping through the middle of it.
    Sinusoidal,
}

/// Per-player transition override, the `crossfade` field of a [`PlayerUpdate`].
///
/// When present it replaces the server-wide `crossfade.*` defaults for that player, and `null`
/// clears the override. `enable` with `durationMs` and `curve` drives a fade overlap, `gapless`
/// alone gives a zero overlap handoff, and with both off a track simply finishes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CrossfadeSettings {
    /// Whether to overlap the outgoing and incoming tracks on a transition.
    pub enable: bool,
    /// Overlap length in milliseconds, used when `enable` is set.
    #[serde(default = "default_duration_ms")]
    pub duration_ms: u64,
    /// Overlap length in milliseconds for an explicit manual skip.
    #[serde(default = "default_manual_duration_ms")]
    pub manual_duration_ms: u64,
    /// Fade easing.
    pub curve: CrossfadeCurve,
    /// Whether to fall back to a zero-gap handoff when crossfade is off or inapplicable.
    pub gapless: bool,
}

impl Default for CrossfadeSettings {
    fn default() -> Self {
        Self {
            enable: false,
            duration_ms: default_duration_ms(),
            manual_duration_ms: default_manual_duration_ms(),
            curve: CrossfadeCurve::default(),
            gapless: false,
        }
    }
}

// Has to match the config default. A zero would send the bare `{"enable": true}` payload down the
// no-overlap path of `to_engine` and switch transitions off instead of on.
fn default_duration_ms() -> u64 {
    6000
}

fn default_manual_duration_ms() -> u64 {
    3500
}

impl CrossfadeSettings {
    /// Convert to engine options, or `None` when no transition would happen.
    pub fn to_engine(&self) -> Option<player::CrossfadeOptions> {
        // A disabled fade can still ask for a gapless handoff, which is a zero length overlap.
        let duration_ms = if self.enable { self.duration_ms } else { 0 };
        if duration_ms == 0 && !self.gapless {
            return None;
        }
        Some(player::CrossfadeOptions {
            duration_ms,
            manual_duration_ms: self.manual_duration_ms,
            curve: match self.curve {
                CrossfadeCurve::Linear => player::CrossfadeCurve::Linear,
                CrossfadeCurve::Exp => player::CrossfadeCurve::Exponential,
                CrossfadeCurve::Log => player::CrossfadeCurve::Logarithmic,
                CrossfadeCurve::SCurve => player::CrossfadeCurve::SCurve,
                CrossfadeCurve::Sinusoidal => player::CrossfadeCurve::Sinusoidal,
            },
            gapless: self.gapless,
        })
    }

    /// The wire view of the options in effect. `enable` follows from a non-zero overlap, since a
    /// zero `duration_ms` only survives in the engine for a gapless handoff.
    pub fn from_engine(options: &player::CrossfadeOptions) -> Self {
        Self {
            enable: options.duration_ms > 0,
            duration_ms: options.duration_ms,
            manual_duration_ms: options.manual_duration_ms,
            curve: match options.curve {
                player::CrossfadeCurve::Linear => CrossfadeCurve::Linear,
                player::CrossfadeCurve::Exponential => CrossfadeCurve::Exp,
                player::CrossfadeCurve::Logarithmic => CrossfadeCurve::Log,
                player::CrossfadeCurve::SCurve => CrossfadeCurve::SCurve,
                player::CrossfadeCurve::Sinusoidal => CrossfadeCurve::Sinusoidal,
            },
            gapless: options.gapless,
        }
    }
}

/// Per-player pause and resume ramp, the `tape` field of a [`PlayerUpdate`].
///
/// There is no node-wide default, so `null` and `{"enable": false}` both switch it off. Omitted
/// fields fall back to a 500 ms sCurve ramp, which makes `{"enable": true}` enough on its own.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct TapeSettings {
    /// Whether the effect is on. With it off, pause and resume are instant.
    pub enable: bool,
    /// Spin-down and spin-up ramp length in milliseconds.
    #[serde(default = "default_tape_duration_ms")]
    pub duration_ms: u64,
    /// Ramp easing.
    pub curve: TapeCurve,
}

/// Ramp easing for [`TapeSettings`].
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TapeCurve {
    /// Constant-rate ramp.
    Linear,
    /// Steep near full speed, slow near the stop.
    Exponential,
    /// Smooth S-curve, the default.
    #[default]
    SCurve,
    /// Quadratic ease.
    Quad,
}

impl Default for TapeSettings {
    fn default() -> Self {
        Self {
            enable: false,
            duration_ms: default_tape_duration_ms(),
            curve: TapeCurve::default(),
        }
    }
}

fn default_tape_duration_ms() -> u64 {
    500
}

// Longest accepted ramp. An unbounded duration drives the per-frame ramp step toward zero, so a
// paused player would never reach silence.
const MAX_TAPE_DURATION_MS: u64 = 60_000;

impl TapeSettings {
    /// Convert to engine options, or `None` when disabled.
    pub fn to_engine(&self) -> Option<player::TapeOptions> {
        if !self.enable {
            return None;
        }
        Some(player::TapeOptions {
            duration_ms: self.duration_ms.min(MAX_TAPE_DURATION_MS),
            curve: match self.curve {
                TapeCurve::Linear => player::TapeCurve::Linear,
                TapeCurve::Exponential => player::TapeCurve::Exponential,
                TapeCurve::SCurve => player::TapeCurve::SCurve,
                TapeCurve::Quad => player::TapeCurve::Quad,
            },
        })
    }
}

/// The player update body. Every field is [`Omissible`], where absent means leave unchanged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerUpdate {
    /// Deprecated alias for `track.encoded`.
    pub encoded_track: Omissible<Option<String>>,
    /// Deprecated alias for `track.identifier`.
    pub identifier: Omissible<String>,
    /// The track to play and its user data.
    pub track: Omissible<PlayerUpdateTrack>,
    /// Seek position in milliseconds.
    pub position: Omissible<i64>,
    /// Millisecond position at which to stop the track, `null` to clear.
    pub end_time: Omissible<Option<i64>>,
    /// Volume, `0..=1000`.
    pub volume: Omissible<i32>,
    /// Whether to pause.
    pub paused: Omissible<bool>,
    /// Filters to apply, replacing the whole chain.
    pub filters: Omissible<Filters>,
    /// Voice connection state to apply.
    pub voice: Omissible<VoiceState>,
    /// The next track to pre-buffer for a transition, `null` to clear. Once the current track's
    /// end marker fires it becomes a held mixer layer and the track ends with reason `crossfade`,
    /// or `gapless` when overlap is off.
    pub next_track: Omissible<Option<PlayerUpdateTrack>>,
    /// Per-player transition override, `null` to fall back to the server-wide `crossfade.*`
    /// defaults.
    pub crossfade: Omissible<Option<CrossfadeSettings>>,
    /// Per-player pause ramp, `null` or `enable: false` to make pause and resume instant. Applied
    /// before `paused`, so one update can set the ramp and pause with it.
    pub tape: Omissible<Option<TapeSettings>>,
    /// Start the held `nextTrack` transition now, for an explicit client skip.
    pub transition: Omissible<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // A channel id is a snowflake; junk must be rejected rather than silently becoming channel 0.
    #[test]
    fn a_voice_state_needs_a_numeric_channel_id() {
        let mut voice = VoiceState {
            token: "t".to_string(),
            endpoint: "e".to_string(),
            session_id: "s".to_string(),
            channel_id: Some("123".to_string()),
        };
        assert!(voice.is_complete());

        voice.channel_id = Some("abc".to_string());
        assert!(!voice.is_complete(), "a non-numeric channel id must fail");
        voice.channel_id = Some(String::new());
        assert!(!voice.is_complete(), "an empty channel id must fail");
        voice.channel_id = None;
        assert!(!voice.is_complete(), "a missing channel id must fail");
    }

    // The wire defaults are a working 500 ms sCurve ramp, not `u64::default()`.
    #[test]
    fn partial_tape_payload_keeps_defaults() {
        let update: PlayerUpdate = serde_json::from_str(r#"{"tape":{"enable":true}}"#).unwrap();
        let settings = match update.tape {
            Omissible::Present(Some(settings)) => settings,
            other => panic!("expected a tape object, got {other:?}"),
        };
        assert_eq!(settings.duration_ms, 500);
        assert_eq!(settings.curve, TapeCurve::SCurve);
        let engine = settings.to_engine().expect("enabled tape has options");
        assert_eq!(engine.duration_ms, 500);
    }

    #[test]
    fn each_curve_name_parses() {
        for (name, expected) in [
            ("linear", TapeCurve::Linear),
            ("exponential", TapeCurve::Exponential),
            ("sCurve", TapeCurve::SCurve),
            ("quad", TapeCurve::Quad),
        ] {
            let json = format!(r#"{{"tape":{{"enable":true,"curve":"{name}"}}}}"#);
            let update: PlayerUpdate = serde_json::from_str(&json).unwrap();
            let Omissible::Present(Some(settings)) = update.tape else {
                panic!("expected a tape object for {name}");
            };
            assert_eq!(settings.curve, expected);
        }
    }

    #[test]
    fn disabled_tape_has_no_engine_options() {
        let settings = TapeSettings {
            enable: false,
            ..Default::default()
        };
        assert!(settings.to_engine().is_none());
    }

    // An unbounded duration would drive the ramp step toward zero, so a paused player would never
    // reach silence.
    #[test]
    fn absurd_tape_duration_is_clamped() {
        let update: PlayerUpdate =
            serde_json::from_str(r#"{"tape":{"enable":true,"durationMs":999999999}}"#).unwrap();
        let Omissible::Present(Some(settings)) = update.tape else {
            panic!("expected a tape object");
        };
        assert_eq!(
            settings.to_engine().unwrap().duration_ms,
            MAX_TAPE_DURATION_MS
        );
    }

    #[test]
    fn null_tape_disables_it_and_absent_leaves_it_alone() {
        let update: PlayerUpdate = serde_json::from_str(r#"{"tape":null}"#).unwrap();
        assert!(matches!(update.tape, Omissible::Present(None)));
        let absent: PlayerUpdate = serde_json::from_str("{}").unwrap();
        assert!(matches!(absent.tape, Omissible::Omitted));
    }

    // `{"enable": true}` means crossfade with the node's defaults. A zero wire default for
    // `durationMs` would flip it into the no-overlap path and switch transitions off.
    #[test]
    fn partial_crossfade_payload_keeps_defaults() {
        let update: PlayerUpdate =
            serde_json::from_str(r#"{"crossfade":{"enable":true}}"#).unwrap();
        let Omissible::Present(Some(settings)) = update.crossfade else {
            panic!("expected a crossfade object");
        };
        assert_eq!(settings.duration_ms, 6000);
        assert_eq!(settings.manual_duration_ms, 3500);
        let engine = settings.to_engine().expect("enabled crossfade has options");
        assert_eq!(engine.duration_ms, 6000);
        assert!(!engine.is_gapless());
    }

    // A client that reads the reported settings back and sends them again has to land on the same
    // engine options, otherwise re-asserting them drifts.
    #[test]
    fn crossfade_echo_round_trips() {
        for json in [
            r#"{"crossfade":{"enable":true}}"#,
            r#"{"crossfade":{"enable":true,"durationMs":8000,"curve":"linear","gapless":true}}"#,
            r#"{"crossfade":{"enable":false,"gapless":true}}"#,
        ] {
            let update: PlayerUpdate = serde_json::from_str(json).unwrap();
            let Omissible::Present(Some(settings)) = update.crossfade else {
                panic!("expected a crossfade object for {json}");
            };
            let engine = settings
                .to_engine()
                .unwrap_or_else(|| panic!("expected a transition for {json}"));
            let echoed = CrossfadeSettings::from_engine(&engine);
            assert_eq!(echoed.to_engine(), Some(engine), "{json}");
            // The client reads these keys off the snapshot, so the echo has to speak camelCase.
            let echoed_json = serde_json::to_string(&echoed).unwrap();
            assert!(echoed_json.contains(r#""durationMs""#), "{echoed_json}");
            assert_eq!(
                serde_json::from_str::<CrossfadeSettings>(&echoed_json).unwrap(),
                echoed
            );
        }
    }

    // No fade and no gapless fallback means no transitions, which the snapshot reports as `null`.
    #[test]
    fn fully_disabled_crossfade_has_no_engine_options() {
        let update: PlayerUpdate =
            serde_json::from_str(r#"{"crossfade":{"enable":false}}"#).unwrap();
        let Omissible::Present(Some(settings)) = update.crossfade else {
            panic!("expected a crossfade object");
        };
        assert!(settings.to_engine().is_none());
    }
}
