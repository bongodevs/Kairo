use kairo_core::track::Track;

/// Where the bytes for one track come from.
#[derive(Debug)]
pub(crate) enum Target {
    Url(String),
    Mirror(Track),
}

/// A declarative answer to "how do I play this track".
///
/// Named providers return this instead of an already opened stream; the framework resolves it
/// through the direct HTTP transport and re-labels the result with the provider's name.
#[derive(Debug)]
pub struct Stream {
    pub(crate) target: Target,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) seekable: Option<bool>,
    pub(crate) length: Option<u64>,
    pub(crate) content_type: Option<String>,
}

impl Stream {
    /// Play a direct URL.
    #[must_use]
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            target: Target::Url(url.into()),
            headers: Vec::new(),
            seekable: None,
            length: None,
            content_type: None,
        }
    }

    /// Play another track through the direct transport.
    #[must_use]
    pub fn mirror(track: Track) -> Self {
        Self {
            target: Target::Mirror(track),
            headers: Vec::new(),
            seekable: None,
            length: None,
            content_type: None,
        }
    }

    /// Add one request header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Override whether the opened stream can seek.
    #[must_use]
    pub const fn seekable(mut self, seekable: bool) -> Self {
        self.seekable = Some(seekable);
        self
    }

    /// Override the byte length the player should expect.
    #[must_use]
    pub const fn length(mut self, length: u64) -> Self {
        self.length = Some(length);
        self
    }

    /// Override the media type the player should probe with.
    #[must_use]
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }
}
