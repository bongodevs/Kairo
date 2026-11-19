use std::fmt;

/// Milliseconds on a track timeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Millis(u64);

impl Millis {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Native PCM layout before resampling or channel conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: usize,
}

impl AudioFormat {
    #[must_use]
    pub const fn new(sample_rate: u32, channels: usize) -> Option<Self> {
        if sample_rate > 0 && channels > 0 { Some(Self { sample_rate, channels }) } else { None }
    }
}

/// Metadata accumulated from container revisions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artwork_url: Option<String>,
    pub isrc: Option<String>,
}

/// Information known after probing and updated when a chained stream changes format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub source_name: String,
    pub identifier: String,
    pub codec: &'static str,
    pub container: &'static str,
    pub native_format: Option<AudioFormat>,
    pub duration: Option<Millis>,
    pub seekable: bool,
    pub metadata: TrackMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionSeverity {
    Common,
    Suspicious,
    Fault,
}

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0)
            }
        }
    };
}

id_newtype!(GuildId);
id_newtype!(UserId);
id_newtype!(ChannelId);
