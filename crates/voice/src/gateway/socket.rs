use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, USER_AGENT};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::client::GatewayError;
use crate::config::VoiceConfig;
const GATEWAY_VERSION: u8 = 8;

#[derive(Debug)]
pub enum Inbound {
    Text(String),
    Binary(Vec<u8>),
    Closed(u16, String),
}

pub trait Socket {
    fn send_text(
        &mut self,
        text: String,
    ) -> impl std::future::Future<Output = Result<(), GatewayError>>;
    fn send_binary(
        &mut self,
        bytes: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), GatewayError>>;
    fn recv(&mut self) -> impl std::future::Future<Output = Result<Option<Inbound>, GatewayError>>;
    fn close(&mut self, code: u16, reason: String) -> impl std::future::Future<Output = ()>;
}

impl Socket for WebSocketStream<MaybeTlsStream<TcpStream>> {
    async fn send_text(&mut self, text: String) -> Result<(), GatewayError> {
        SinkExt::send(self, Message::Text(text.into()))
            .await
            .map_err(|err| GatewayError::Transport(err.to_string()))
    }

    async fn send_binary(&mut self, bytes: Vec<u8>) -> Result<(), GatewayError> {
        SinkExt::send(self, Message::Binary(bytes.into()))
            .await
            .map_err(|err| GatewayError::Transport(err.to_string()))
    }

    async fn close(&mut self, code: u16, reason: String) {
        let frame =
            CloseFrame { code: CloseCode::from(code), reason: within_close_limit(reason).into() };
        if SinkExt::send(&mut *self, Message::Close(Some(frame))).await.is_ok() {
            let _ = SinkExt::close(self).await;
        }
    }

    async fn recv(&mut self) -> Result<Option<Inbound>, GatewayError> {
        loop {
            match StreamExt::next(self).await {
                None => return Ok(None),
                Some(Err(err)) => return Err(GatewayError::Transport(err.to_string())),
                Some(Ok(Message::Text(text))) => {
                    return Ok(Some(Inbound::Text(text.as_str().to_owned())));
                }
                Some(Ok(Message::Binary(body))) => {
                    return Ok(Some(Inbound::Binary(body.to_vec())));
                }
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame.map_or((1005, String::new()), |frame| {
                        (u16::from(frame.code), frame.reason.as_str().to_owned())
                    });
                    return Ok(Some(Inbound::Closed(code, reason)));
                }
                Some(Ok(Message::Ping(payload))) => {
                    SinkExt::send(self, Message::Pong(payload))
                        .await
                        .map_err(|err| GatewayError::Transport(err.to_string()))?;
                }
                Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => {}
            }
        }
    }
}

const CLOSE_REASON_LIMIT: usize = 123;

fn within_close_limit(mut reason: String) -> String {
    let mut keep = CLOSE_REASON_LIMIT.min(reason.len());
    while !reason.is_char_boundary(keep) {
        keep -= 1;
    }
    reason.truncate(keep);
    reason
}

pub(crate) async fn open_socket(
    endpoint: &str,
    config: &VoiceConfig,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, GatewayError> {
    let request = gateway_request(endpoint, config.wss_port_override)?;
    let connected = if config.gateway_connect_timeout.is_zero() {
        tokio_tungstenite::connect_async(request).await
    } else {
        tokio::time::timeout(
            config.gateway_connect_timeout,
            tokio_tungstenite::connect_async(request),
        )
        .await
        .map_err(|_| GatewayError::Timeout)?
    };
    let (socket, _) = connected.map_err(|err| GatewayError::Connect(err.to_string()))?;
    Ok(socket)
}

fn gateway_request(endpoint: &str, port_override: bool) -> Result<Request<()>, GatewayError> {
    let mut request = gateway_url(endpoint, port_override)?
        .into_client_request()
        .map_err(|err| GatewayError::Connect(err.to_string()))?;
    let user_agent = HeaderValue::from_str(&user_agent())
        .map_err(|err| GatewayError::Connect(err.to_string()))?;
    request.headers_mut().insert(USER_AGENT, user_agent);
    Ok(request)
}

fn gateway_url(endpoint: &str, port_override: bool) -> Result<String, GatewayError> {
    let endpoint = endpoint.trim();
    let (scheme, host) = endpoint.split_once("://").unwrap_or(("wss", endpoint));
    let host = host.split(['/', '?']).next().unwrap_or_default();
    let host = if port_override { host.strip_suffix(":80").unwrap_or(host) } else { host };
    if host.is_empty() {
        return Err(GatewayError::Connect("empty voice endpoint".into()));
    }
    // Only a build that opted into the test feature turns the explicit scheme into plaintext.
    let scheme =
        if cfg!(feature = "plaintext-gateway") && scheme == "plaintext" { "ws" } else { "wss" };
    Ok(format!("{scheme}://{host}/?v={GATEWAY_VERSION}"))
}

fn user_agent() -> String {
    format!("DiscordBot (https://github.com/appujet/baka-susi, {})", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
#[path = "../../tests/ws.rs"]
mod tests;
