use std::sync::Arc;

use tokio::runtime::Handle;

use kairo_audio::decode::BlockingDecoder;
use kairo_core::{OpenedSource, StreamInfo};

use crate::source::{ProbeError, SourceProbe};

pub(crate) struct AudioProbe;

impl SourceProbe for AudioProbe {
    fn probe(&self, source: OpenedSource) -> Result<StreamInfo, ProbeError> {
        let runtime = Handle::try_current()
            .map_err(|_| ProbeError::new("audio probe requires a Tokio runtime"))?;
        let decoder = BlockingDecoder::open(source, runtime)
            .map_err(|error| ProbeError::new(error.to_string()))?;
        Ok(decoder.info().clone())
    }
}

pub(crate) fn audio_probe() -> Arc<dyn SourceProbe> {
    Arc::new(AudioProbe)
}
