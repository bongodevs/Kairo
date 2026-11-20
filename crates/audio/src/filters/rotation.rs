#[cfg(test)]
#[path = "../../tests/filters_rotation.rs"]
mod tests;

use kairo_core::filters::RotationConfig;

use crate::{CHANNELS, SAMPLE_RATE};

pub struct Rotation {
    phase: f64,
    step: f64,
}

impl Rotation {
    #[must_use]
    pub fn new(config: RotationConfig) -> Self {
        Self {
            phase: 0.0,
            step: config.rotation_hz * std::f64::consts::TAU / f64::from(SAMPLE_RATE),
        }
    }

    #[must_use]
    pub const fn phase(&self) -> f64 {
        self.phase
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let sin = self.phase.sin();
            frame[0] *= ((sin + 1.0) * 0.5) as f32;
            frame[1] *= ((1.0 - sin) * 0.5) as f32;
            self.phase += self.step;
            if self.phase >= std::f64::consts::TAU || self.phase < 0.0 {
                self.phase = self.phase.rem_euclid(std::f64::consts::TAU);
            }
        }
    }
}
