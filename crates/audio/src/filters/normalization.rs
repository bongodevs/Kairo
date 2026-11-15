#[cfg(test)]
#[path = "../../tests/filters_normalization.rs"]
mod tests;

use kairo_core::filters::NormalizationConfig;

use crate::CHANNELS;

pub struct Normalization {
    max_amplitude: f32,
    peak: f32,
    adaptive: bool,
}

impl Normalization {
    #[must_use]
    pub fn new(config: NormalizationConfig) -> Self {
        Self {
            max_amplitude: config.max_amplitude.clamp(0.0, 1.0),
            peak: 0.0,
            adaptive: config.adaptive,
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.adaptive {
            self.peak = 0.0;
        }
        for channel in 0..2 {
            for frame in samples.as_chunks::<CHANNELS>().0 {
                self.peak = self.peak.max(frame[channel].abs());
            }
            if self.peak > self.max_amplitude {
                let gain = self.max_amplitude / self.peak;
                for frame in samples.as_chunks_mut::<CHANNELS>().0 {
                    frame[channel] *= gain;
                }
            }
        }
    }
}
