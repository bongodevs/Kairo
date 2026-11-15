#[cfg(test)]
#[path = "../../tests/filters_tremolo.rs"]
mod tests;

use kairo_core::filters::{FilterError, TremoloConfig};

use crate::{CHANNELS, SAMPLE_RATE};

pub struct Tremolo {
    phase: f32,
    step: f32,
    offset: f32,
    depth: f32,
}

impl Tremolo {
    pub fn new(config: TremoloConfig) -> Result<Self, FilterError> {
        config.validate()?;
        let depth = config.depth * 0.5;
        Ok(Self {
            phase: 0.0,
            step: std::f32::consts::TAU * config.frequency / SAMPLE_RATE as f32,
            offset: 1.0 - depth,
            depth,
        })
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let gain = self.offset + self.depth * self.phase.sin();
            frame[0] *= gain;
            frame[1] *= gain;
            self.phase += self.step;
        }
    }
}
