use std::sync::Arc;

/// The payload a source owns between load and playback.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceData {
    #[default]
    None,
    Probe {
        name: String,
        parameters: Option<String>,
    },
    Opaque(Arc<[u8]>),
}

impl SourceData {
    #[must_use]
    pub fn probe(name: impl Into<String>, parameters: Option<String>) -> Self {
        Self::Probe { name: name.into(), parameters }
    }
}
