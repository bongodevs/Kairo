#[cfg(test)]
#[path = "../../tests/filters_distortion.rs"]
mod tests;

use kairo_core::filters::DistortionConfig;

pub struct Distortion {
    config: DistortionConfig,
}

impl Distortion {
    #[must_use]
    pub fn new(config: DistortionConfig) -> Self {
        Self { config }
    }

    pub fn process(&self, samples: &mut [f32]) {
        for sample in samples {
            let sin = self.config.sin_offset + (*sample * self.config.sin_scale).sin();
            let cos = self.config.cos_offset + (*sample * self.config.cos_scale).cos();
            let tan = self.config.tan_offset + (*sample * self.config.tan_scale).tan();
            *sample = (self.config.offset + self.config.scale * sin * cos * tan).clamp(-1.0, 1.0);
        }
    }
}
