use std::sync::Arc;

use tokio::runtime::Handle;

use crate::source::{ProbeError, SourceProbe};
use kairo_audio::decode::BlockingDecoder;
use kairo_core::{OpenedSource, StreamInfo};

struct AudioProbe;

impl SourceProbe for AudioProbe {
    fn probe(&self, source: OpenedSource) -> Result<StreamInfo, ProbeError> {
        let runtime = Handle::try_current()
            .map_err(|_| ProbeError::new("audio probe requires a Tokio runtime"))?;
        let decoder = BlockingDecoder::open(source, runtime)
            .map_err(|error| ProbeError::new(error.to_string()))?;
        Ok(decoder.info().clone())
    }
}

/// The production probe backed by the audio decoder.
#[must_use]
pub fn audio_probe() -> Arc<dyn SourceProbe> {
    Arc::new(AudioProbe)
}
