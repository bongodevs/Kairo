use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::disconnect::Disconnect;
use super::error::ConnectionError;
use super::path::{Path, transport};
use super::reopen::Network;
use super::session::{Phase, Session};
use super::wire::{initial_speaking, select_protocol, video_announcement};
use crate::Ping;
use crate::config::VoiceConfig;
use crate::gateway::{
    Gateway, GatewaySocket, Heartbeat, Intro, Socket, VoiceEvent, VoiceTarget,
    heartbeat_loop_with_pending,
};
use crate::sender::{
    COMMAND_QUEUE, Command, FrameSource, FrameStats, Frames, Lifecycle, Media, NoFrames, Speaking,
};

pub struct Connection<S> {
    pub(super) socket: S,
    pub(super) heartbeat: Heartbeat,
    pub(super) session: Session,
    pub(super) media: Media,
    pub(super) udp: UdpSocket,
    pub(super) local: SocketAddr,
    pub(super) target: VoiceTarget,
    pub(super) commands: mpsc::Sender<Command>,
    pub(super) controls: Option<mpsc::Receiver<Command>>,
    pub(super) pending: VecDeque<VoiceEvent>,
    pub(super) config: VoiceConfig,
}
impl Connection<GatewaySocket> {
    pub async fn connect(
        target: &VoiceTarget,
        config: VoiceConfig,
    ) -> Result<Self, ConnectionError> {
        let mut gateway = Gateway::connect(target, Intro::Identify, &config).await?;
        let ready = gateway
            .ready()
            .cloned()
            .ok_or_else(|| crate::gateway::GatewayError::Protocol("identify was resumed".into()))?;
        let path = gateway.drive_while(transport(&ready, &config)).await?;
        Self::attach(gateway, target, path, config).await
    }

    pub async fn run_guarded<T: FrameSource>(
        &mut self,
        source: &mut T,
    ) -> Result<Disconnect, ConnectionError> {
        let mut network = Network { config: self.config.clone() };
        self.pump_retrying(source, &mut network).await
    }
}

impl<S> Connection<S> {
    pub fn ready_to_send(&self) -> bool {
        self.session.ready_to_send()
    }

    pub fn phase(&self) -> Phase {
        self.session.phase
    }

    pub fn ssrc(&self) -> u32 {
        self.session.ssrc
    }

    pub fn speaking(&self) -> Speaking {
        Speaking::new(self.commands.clone())
    }

    pub fn frames(&self) -> Frames {
        Frames::new(self.commands.clone())
    }

    pub fn lifecycle(&self) -> Lifecycle {
        Lifecycle::new(self.commands.clone())
    }

    pub fn frame_stats(&self) -> FrameStats {
        self.media.stats()
    }

    pub fn ping(&self) -> Duration {
        self.heartbeat.ping
    }

    pub fn ping_handle(&self) -> Ping {
        self.heartbeat.ping_handle()
    }

    pub fn mode(&self) -> &str {
        &self.session.mode
    }

    pub fn local(&self) -> SocketAddr {
        self.local
    }
}

impl<S: Socket> Connection<S> {
    pub(super) async fn attach(
        gateway: Gateway<S>,
        target: &VoiceTarget,
        path: Path,
        config: VoiceConfig,
    ) -> Result<Self, ConnectionError> {
        let (mut socket, heartbeat, pending) = gateway.split();
        socket.send_text(select_protocol(&path)?).await?;
        socket.send_text(initial_speaking(path.ssrc)?).await?;
        socket.send_text(video_announcement(path.ssrc)?).await?;
        let (commands, controls) = mpsc::channel(COMMAND_QUEUE);
        Ok(Self {
            socket,
            heartbeat,
            session: Session::new(target, path.mode, path.ssrc, config.send_speaking_stop),
            media: Media::new(path.ssrc),
            udp: path.udp,
            local: path.local,
            target: target.clone(),
            commands,
            controls: Some(controls),
            pending,
            config,
        })
    }

    pub async fn run(&mut self) -> Result<Disconnect, ConnectionError> {
        self.run_with(&mut NoFrames).await
    }

    pub async fn run_with<T: FrameSource>(
        &mut self,
        source: &mut T,
    ) -> Result<Disconnect, ConnectionError> {
        let close = self.pump(source).await;
        self.controls = None;
        close
    }

    pub(super) async fn pump<T: FrameSource>(
        &mut self,
        source: &mut T,
    ) -> Result<Disconnect, ConnectionError> {
        let Self { socket, heartbeat, session, media, udp, controls, pending, .. } = self;
        let Some(receiver) = controls.as_mut() else {
            return Err(ConnectionError::AlreadyRun);
        };
        heartbeat_loop_with_pending(socket, heartbeat, receiver, pending, |pump| {
            session.on_pump(pump, source, udp, media)
        })
        .await
    }
}
