use std::future::Future;

use tokio::sync::mpsc;

use super::disconnect::Disconnect;
use super::error::ConnectionError;
use super::path::{Path, transport};
use super::session::{Phase, Session};
use super::wire::{initial_speaking, select_protocol, video_announcement};
use crate::config::VoiceConfig;
use crate::gateway::{
    Gateway, GatewayError, GatewaySocket, Intro, Opening, Socket, VoiceTarget, acked,
    reconnect_delay,
};
use crate::sender::{Command, FrameSource, Media};

impl<S: Socket> super::Connection<S> {
    pub(super) async fn pump_retrying<T: FrameSource, R: Redial<S>>(
        &mut self,
        source: &mut T,
        redial: &mut R,
    ) -> Result<Disconnect, ConnectionError> {
        let mut closing = match self.pump(source).await {
            Ok(close) => close,
            Err(ConnectionError::AlreadyRun) => return Err(ConnectionError::AlreadyRun),
            Err(err) => closing_after(err),
        };
        loop {
            let seq_ack = acked(self.heartbeat.seq_ack);
            let Some(intro) = self.session.retry.attempt(closing.code, seq_ack) else {
                self.controls = None;
                return Ok(closing);
            };
            let delay = reconnect_delay(self.session.retry.attempts().saturating_sub(1));
            tracing::warn!(
                code = closing.code,
                reason = %closing.reason,
                remote = closing.by_remote,
                ?intro,
                ?delay,
                "voice gateway closed, dialing another one"
            );
            let controls = self.controls.as_mut().ok_or(ConnectionError::AlreadyRun)?;
            if let Waiting::Close(close) = while_controlled(
                tokio::time::sleep(delay),
                controls,
                &mut self.session,
                &mut self.media,
            )
            .await
            {
                self.controls = None;
                return Ok(close);
            }
            let target = self.target.clone();
            let controls = self.controls.as_mut().ok_or(ConnectionError::AlreadyRun)?;
            let reopened = while_controlled(
                redial.redial(&target, intro),
                controls,
                &mut self.session,
                &mut self.media,
            )
            .await;
            closing = match reopened {
                Waiting::Close(close) => {
                    self.controls = None;
                    return Ok(close);
                }
                Waiting::Done(Err(err)) => closing_after(err),
                Waiting::Done(Ok(Reopened::Resumed(gateway))) => match self.resume(gateway).await {
                    Err(err) => closing_after(err),
                    Ok(()) => match self.pump(source).await {
                        Ok(close) => close,
                        Err(err) => closing_after(err),
                    },
                },
                Waiting::Done(Ok(Reopened::Ready(gateway, path))) => {
                    match self.rebind(gateway, path).await {
                        Err(err) => closing_after(err),
                        Ok(()) => match self.pump(source).await {
                            Ok(close) => close,
                            Err(err) => closing_after(err),
                        },
                    }
                }
            };
        }
    }

    pub(super) async fn rebind(
        &mut self,
        gateway: Gateway<S>,
        path: Path,
    ) -> Result<(), ConnectionError> {
        let (mut socket, mut heartbeat, pending) = gateway.split();
        heartbeat.inherit_ping(&self.heartbeat);
        socket.send_text(select_protocol(&path)?).await?;
        socket.send_text(initial_speaking(path.ssrc)?).await?;
        socket.send_text(video_announcement(path.ssrc)?).await?;
        self.socket = socket;
        self.heartbeat = heartbeat;
        self.pending.extend(pending);
        self.session.retry.saw_ready();
        self.session.mode = path.mode;
        self.session.ssrc = path.ssrc;
        self.session.cipher = None;
        self.session.phase = Phase::AwaitingSession;
        self.media.rebind(path.ssrc);
        self.udp = path.udp;
        self.local = path.local;
        Ok(())
    }

    async fn resume(&mut self, gateway: Gateway<S>) -> Result<(), ConnectionError> {
        let (mut socket, mut heartbeat, pending) = gateway.split();
        heartbeat.inherit_ping(&self.heartbeat);
        if let Some(reply) = self.session.resumed_announcement()? {
            reply.send(&mut socket).await?;
        }
        self.socket = socket;
        self.heartbeat = heartbeat;
        self.pending.extend(pending);
        self.media.resume();
        Ok(())
    }
}

enum Waiting<T> {
    Done(T),
    Close(Disconnect),
}

async fn while_controlled<F: Future>(
    future: F,
    controls: &mut mpsc::Receiver<Command>,
    session: &mut Session,
    media: &mut Media,
) -> Waiting<F::Output> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Waiting::Done(result),
            Some(command) = controls.recv() => {
                if let Some(close) = session.while_offline(command, media) {
                    return Waiting::Close(close);
                }
            }
        }
    }
}

pub(super) enum Reopened<S> {
    Resumed(Gateway<S>),
    Ready(Gateway<S>, Path),
}

pub(super) trait Redial<S> {
    fn redial(
        &mut self,
        target: &VoiceTarget,
        intro: Intro,
    ) -> impl std::future::Future<Output = Result<Reopened<S>, ConnectionError>>;
}

pub(super) struct Network {
    pub(super) config: VoiceConfig,
}

impl Redial<GatewaySocket> for Network {
    async fn redial(
        &mut self,
        target: &VoiceTarget,
        intro: Intro,
    ) -> Result<Reopened<GatewaySocket>, ConnectionError> {
        let mut gateway = Gateway::connect(target, intro, &self.config).await?;
        match (intro, gateway.opening().clone()) {
            (Intro::Resume(_), Opening::Resumed) => Ok(Reopened::Resumed(gateway)),
            (_, Opening::Ready(ready)) => {
                let path = gateway.drive_while(transport(&ready, &self.config)).await?;
                Ok(Reopened::Ready(gateway, path))
            }
            (Intro::Identify, Opening::Resumed) => {
                Err(GatewayError::Protocol("identify was acknowledged as a resume".into()).into())
            }
        }
    }
}

fn closing_after(err: ConnectionError) -> Disconnect {
    let err = match err {
        ConnectionError::Gateway(GatewayError::CloseFrame { code, reason }) => {
            return Disconnect::remote(code, reason);
        }
        err => err,
    };
    let connect_failed = matches!(
        err,
        ConnectionError::Gateway(
            GatewayError::Connect(_) | GatewayError::Timeout | GatewayError::Closed
        )
    );
    if connect_failed {
        Disconnect::abnormal(err.to_string())
    } else {
        Disconnect::internal(err.to_string())
    }
}
