#[cfg(test)]
#[path = "../../tests/filters_cutoff_low_pass.rs"]
mod tests;

use kairo_core::filters::PassConfig;

use super::biquad::{Biquad, PassKind};

pub struct CutoffLowPass(Biquad);

impl CutoffLowPass {
    #[must_use]
    pub fn new(config: PassConfig) -> Self {
        Self(Biquad::new(config, PassKind::Low))
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        self.0.process(samples);
    }
}
