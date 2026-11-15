//! Opt-in live check for the real Discord voice gateway and DAVE protocol.
//! It reads the operator's local credentials, skips when absent, and never prints secrets.
//! Run with:
//! `DISCORD_BOT_TOKEN=... DISCORD_GUILD_ID=... DISCORD_CHANNEL_ID=... cargo test -p kairo live_voice -- --ignored --nocapture`

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use voice::connection::Connection;
use voice::ids::{ChannelId, GuildId as VoiceGuildId, UserId};
use voice::{FrameSource, GatewaySocket, SPEAKING_MICROPHONE, VoiceConfig, VoiceTarget};

struct TestTone {
    remaining: usize,
}

impl FrameSource for TestTone {
    fn provide(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        // 3-byte standard Opus silence frame
        let frame = [0xf8, 0xff, 0xfe];
        out[..frame.len()].copy_from_slice(&frame);
        Some(frame.len())
    }

    fn is_active(&self) -> bool {
        self.remaining > 0
    }
}

fn read_config() -> Option<(String, u64, u64)> {
    let token = std::env::var("DISCORD_BOT_TOKEN").ok().or_else(|| {
        let path =
            std::env::var("KAIRO_CONFIG").unwrap_or_else(|_| String::from("config.local.toml"));
        let content = std::fs::read_to_string(&path).ok()?;
        let toml: toml::Table = toml::from_str(&content).ok()?;
        toml.get("discord")?.get("botToken")?.as_str().map(String::from)
    })?;
    let guild = std::env::var("DISCORD_GUILD_ID")
        .ok()
        .or_else(|| {
            let path =
                std::env::var("KAIRO_CONFIG").unwrap_or_else(|_| String::from("config.local.toml"));
            let content = std::fs::read_to_string(&path).ok()?;
            let toml: toml::Table = toml::from_str(&content).ok()?;
            toml.get("discord")?.get("guildId")?.as_str().map(String::from)
        })?
        .parse::<u64>()
        .ok()?;
    let channel = std::env::var("DISCORD_CHANNEL_ID")
        .ok()
        .or_else(|| {
            let path =
                std::env::var("KAIRO_CONFIG").unwrap_or_else(|_| String::from("config.local.toml"));
            let content = std::fs::read_to_string(&path).ok()?;
            let toml: toml::Table = toml::from_str(&content).ok()?;
            toml.get("discord")?.get("channelId")?.as_str().map(String::from)
        })?
        .parse::<u64>()
        .ok()?;

    Some((token, guild, channel))
}

#[tokio::test]
#[ignore = "calls the live Discord voice gateway with the operator's credentials"]
async fn live_discord_voice_and_dave_connection() {
    let Some((bot_token, guild_id, channel_id)) = read_config() else {
        eprintln!("no discord credentials configured, skipping live check");
        return;
    };

    eprintln!("connecting to Discord Gateway to obtain voice server update...");
    let gateway_url = "wss://gateway.discord.gg/?v=10&encoding=json";
    let (mut ws_stream, _) =
        connect_async(gateway_url).await.expect("connect to discord main gateway");

    let mut user_id: Option<u64> = None;
    let mut voice_session_id: Option<String> = None;
    let mut voice_token: Option<String> = None;
    let mut voice_endpoint: Option<String> = None;

    // Handshake on main gateway
    let hello = ws_stream.next().await.expect("hello message").expect("valid message");
    let hello_text = hello.to_text().expect("hello text");
    let hello_json: Value = serde_json::from_str(hello_text).expect("hello json");
    assert_eq!(hello_json["op"], 10, "expected hello op 10");
    let heartbeat_interval =
        hello_json["d"]["heartbeat_interval"].as_u64().expect("heartbeat interval");

    let hb_interval = Duration::from_millis(heartbeat_interval);

    // Send Identify
    let identify = json!({
        "op": 2,
        "d": {
            "token": bot_token,
            "intents": 128, // GUILD_VOICE_STATES
            "properties": {
                "os": "linux",
                "browser": "kairo",
                "device": "kairo"
            }
        }
    });
    ws_stream.send(Message::Text(identify.to_string().into())).await.expect("send identify");

    let timeout_duration = Duration::from_secs(15);
    let receive_info = timeout(timeout_duration, async {
        let mut interval_timer = tokio::time::interval(hb_interval);
        // discard immediate first tick
        interval_timer.tick().await;

        loop {
            tokio::select! {
                _ = interval_timer.tick() => {
                    let hb = json!({ "op": 1, "d": null });
                    ws_stream.send(Message::Text(hb.to_string().into())).await.expect("send heartbeat");
                }
                msg = ws_stream.next() => {
                    let Some(Ok(msg)) = msg else { continue };
                    if !msg.is_text() { continue; }
                    let Ok(val) = serde_json::from_str::<Value>(msg.to_text().unwrap()) else { continue };
                    let op = val["op"].as_u64().unwrap_or(99);
                    let event_type = val["t"].as_str().unwrap_or("");

                    if op == 0 && event_type == "READY" {
                        let uid_str = val["d"]["user"]["id"].as_str().expect("user id");
                        user_id = Some(uid_str.parse::<u64>().expect("parse user id"));
                        eprintln!("Discord Gateway READY. User ID: {}", uid_str);

                        // Send Voice State Update
                        let voice_state_update = json!({
                            "op": 4,
                            "d": {
                                "guild_id": guild_id.to_string(),
                                "channel_id": channel_id.to_string(),
                                "self_mute": false,
                                "self_deaf": false
                            }
                        });
                        ws_stream.send(Message::Text(voice_state_update.to_string().into())).await.expect("send voice state update");
                    } else if op == 0 && event_type == "VOICE_STATE_UPDATE" {
                        let guild = val["d"]["guild_id"].as_str().unwrap_or("");
                        let user = val["d"]["user_id"].as_str().unwrap_or("");
                        if guild == guild_id.to_string() && user_id.map(|u| u.to_string() == user).unwrap_or(false) {
                            let sid = val["d"]["session_id"].as_str().expect("session_id");
                            voice_session_id = Some(sid.to_string());
                            eprintln!("received VOICE_STATE_UPDATE session_id");
                        }
                    } else if op == 0 && event_type == "VOICE_SERVER_UPDATE" {
                        let guild = val["d"]["guild_id"].as_str().unwrap_or("");
                        if guild == guild_id.to_string() {
                            let tok = val["d"]["token"].as_str().expect("token");
                            let ep = val["d"]["endpoint"].as_str().expect("endpoint");
                            voice_token = Some(tok.to_string());
                            voice_endpoint = Some(ep.to_string());
                            eprintln!("received VOICE_SERVER_UPDATE endpoint: {ep}");
                        }
                    }

                    if user_id.is_some() && voice_session_id.is_some() && voice_token.is_some() && voice_endpoint.is_some() {
                        break;
                    }
                }
            }
        }
    }).await;

    receive_info.expect("timeout waiting for voice session and server updates from Discord");

    let user_id = user_id.expect("user id");
    let session_id = voice_session_id.expect("session id");
    let token = voice_token.expect("voice token");
    let endpoint = voice_endpoint.expect("endpoint");

    eprintln!("connecting to live voice gateway at {endpoint}...");
    let target = VoiceTarget {
        endpoint: endpoint.clone(),
        guild: VoiceGuildId(guild_id),
        channel: ChannelId(channel_id),
        user: UserId(user_id),
        session_id,
        token,
    };

    let config =
        VoiceConfig { gateway_connect_timeout: Duration::from_secs(10), ..VoiceConfig::default() };

    let mut connection = Connection::<GatewaySocket>::connect(&target, config)
        .await
        .expect("connect to voice gateway and complete handshake/DAVE negotiation");

    eprintln!(
        "voice connection established! Mode: {}, SSRC: {}, Local UDP: {}, Phase: {:?}",
        connection.mode(),
        connection.ssrc(),
        connection.local(),
        connection.phase()
    );

    let speaking = connection.speaking();
    speaking.set(SPEAKING_MICROPHONE).await.expect("set speaking flag");

    let notifier = connection.frames();
    notifier.notify().await.expect("arm frame pacing");

    let lifecycle = connection.lifecycle();

    // Pump 50 audio frames (1 second of real-time 20ms paced frames)
    let mut audio_source = TestTone { remaining: 50 };
    eprintln!("sending 50 paced frames through the voice connection...");

    let driver_run = async {
        let _ = connection.run_guarded(&mut audio_source).await;
    };

    tokio::select! {
        _ = driver_run => {}
        _ = tokio::time::sleep(Duration::from_millis(1500)) => {}
    }

    let stats = connection.frame_stats();
    eprintln!(
        "voice frame stats: sent={}, skipped={}, final phase: {:?}",
        stats.sent,
        stats.skipped,
        connection.phase()
    );
    assert!(stats.sent > 0, "must have sent audio frames");

    // Clean disconnect: leave voice channel on Discord main gateway
    eprintln!("leaving voice channel and disconnecting cleanly...");
    let leave_msg = json!({
        "op": 4,
        "d": {
            "guild_id": guild_id.to_string(),
            "channel_id": null,
            "self_mute": false,
            "self_deaf": false
        }
    });
    let _ = ws_stream.send(Message::Text(leave_msg.to_string().into())).await;
    let _ = lifecycle.disconnect().await;
    let _ = ws_stream.close(None).await;

    eprintln!("V-live validation successful!");
}
