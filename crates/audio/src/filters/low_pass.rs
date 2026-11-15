#[cfg(test)]
#[path = "../../tests/filters_low_pass.rs"]
mod tests;

use kairo_core::filters::LowPassConfig;

use crate::CHANNELS;

pub struct LowPass {
    smoothing: f32,
    values: [f32; 2],
    initialized: bool,
}

impl LowPass {
    #[must_use]
    pub fn new(config: LowPassConfig) -> Self {
        Self { smoothing: config.smoothing, values: [0.0; 2], initialized: false }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            if !self.initialized {
                self.values = *frame;
                self.initialized = true;
            }
            for (value, sample) in self.values.iter_mut().zip(frame) {
                *value += (*sample - *value) / self.smoothing;
                *sample = *value;
            }
        }
    }

    pub fn reset(&mut self) {
        self.initialized = false;
    }
}
