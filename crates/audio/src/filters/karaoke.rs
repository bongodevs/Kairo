use kairo_core::filters::KaraokeConfig;

use crate::{CHANNELS, SAMPLE_RATE};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Karaoke {
    config: KaraokeConfig,
    a: f32,
    b: f32,
    c: f32,
    y1: f32,
    y2: f32,
}

impl Karaoke {
    #[must_use]
    pub fn new(config: KaraokeConfig) -> Self {
        let mut karaoke = Self { config, a: 0.0, b: 0.0, c: 0.0, y1: 0.0, y2: 0.0 };
        karaoke.update_coefficients();
        karaoke
    }

    pub fn set_config(&mut self, config: KaraokeConfig) {
        self.config = config;
        self.update_coefficients();
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let left = frame[0];
            let right = frame[1];
            let y = self.a * ((left + right) * 0.5) - self.b * self.y1 - self.c * self.y2;
            self.y2 = self.y1;
            self.y1 = y;
            let mono = y * self.config.mono_level * self.config.level;
            frame[0] = left - right * self.config.level + mono;
            frame[1] = right - left * self.config.level + mono;
        }
    }

    pub fn reset(&mut self) {
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    fn update_coefficients(&mut self) {
        let rate = f64::from(SAMPLE_RATE);
        let width = f64::from(self.config.filter_width);
        let band = f64::from(self.config.filter_band);
        let c = (-2.0 * std::f64::consts::PI * width / rate).exp();
        let b = -4.0 * c / (1.0 + c) * (2.0 * std::f64::consts::PI * band / rate).cos();
        let a = (1.0 - b * b / (4.0 * c)).sqrt() * (1.0 - c);
        self.a = a as f32;
        self.b = b as f32;
        self.c = c as f32;
        self.reset();
    }
}
