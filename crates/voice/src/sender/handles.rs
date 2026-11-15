use tokio::sync::mpsc;

use crate::connection::ConnectionError;
use crate::connection::disconnect::{CLOSE_NORMAL, CLOSE_RECONNECT, RECONNECT_REASON};
pub(crate) const COMMAND_QUEUE: usize = 4;

pub const SPEAKING_MICROPHONE: u8 = 1;
pub const SPEAKING_SOUNDSHARE: u8 = 2;
pub const SPEAKING_PRIORITY: u8 = 4;

#[derive(Clone, Copy)]
pub(crate) enum Command {
    Speak(u8),
    Produce,
    Close { code: u16, reason: &'static str },
}

#[derive(Clone)]
pub struct Speaking {
    pub(super) commands: mpsc::Sender<Command>,
}

#[derive(Clone)]
pub struct Frames {
    pub(super) commands: mpsc::Sender<Command>,
}

#[derive(Clone)]
pub struct Lifecycle {
    pub(super) commands: mpsc::Sender<Command>,
}

impl Speaking {
    pub(crate) fn new(commands: mpsc::Sender<Command>) -> Self {
        Self { commands }
    }

    pub async fn set(&self, mask: u8) -> Result<(), ConnectionError> {
        self.commands.send(Command::Speak(mask)).await.map_err(|_| ConnectionError::Stopped)
    }
}

impl Frames {
    pub(crate) fn new(commands: mpsc::Sender<Command>) -> Self {
        Self { commands }
    }

    pub async fn notify(&self) -> Result<(), ConnectionError> {
        self.commands.send(Command::Produce).await.map_err(|_| ConnectionError::Stopped)
    }
}

impl Lifecycle {
    pub(crate) fn new(commands: mpsc::Sender<Command>) -> Self {
        Self { commands }
    }

    pub async fn disconnect(&self) -> Result<(), ConnectionError> {
        self.close(CLOSE_NORMAL, "").await
    }

    pub async fn reconnect(&self) -> Result<(), ConnectionError> {
        self.close(CLOSE_RECONNECT, RECONNECT_REASON).await
    }

    async fn close(&self, code: u16, reason: &'static str) -> Result<(), ConnectionError> {
        self.commands
            .send(Command::Close { code, reason })
            .await
            .map_err(|_| ConnectionError::Stopped)
    }
}
