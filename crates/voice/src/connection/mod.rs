pub(crate) mod disconnect;
mod driver;
mod error;
mod path;
mod reopen;
mod session;
mod wire;

pub use crate::sender::{Frames, Lifecycle, Speaking};
pub use crate::sender::{SPEAKING_MICROPHONE, SPEAKING_PRIORITY, SPEAKING_SOUNDSHARE};
pub use disconnect::Disconnect;
pub use driver::Connection;
pub use error::ConnectionError;
pub use session::Phase;
pub(crate) use session::Session;

#[cfg(test)]
#[path = "../../tests/connect.rs"]
mod tests;
