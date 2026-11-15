#[cfg(test)]
#[path = "../../tests/filters_high_pass.rs"]
mod tests;

use kairo_core::filters::PassConfig;

use super::biquad::{Biquad, PassKind};

pub struct HighPass(Biquad);

impl HighPass {
    #[must_use]
    pub fn new(config: PassConfig) -> Self {
        Self(Biquad::new(config, PassKind::High))
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        self.0.process(samples);
    }
}
