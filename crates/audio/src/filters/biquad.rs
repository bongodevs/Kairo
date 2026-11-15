use kairo_core::filters::PassConfig;

use crate::{CHANNELS, SAMPLE_RATE};

const Q: f64 = 0.707;

pub(super) enum PassKind {
    High,
    Low,
}

pub(super) struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    input: [[f32; 2]; 2],
    output: [[f32; 2]; 2],
    boost: f32,
}

impl Biquad {
    pub(super) fn new(config: PassConfig, kind: PassKind) -> Self {
        let omega = std::f64::consts::TAU * f64::from(config.cutoff_hz) / f64::from(SAMPLE_RATE);
        let cos = omega.cos();
        let alpha = omega.sin() / (2.0 * Q);
        let scale = 1.0 + alpha;
        let edge = match kind {
            PassKind::High => 1.0 + cos,
            PassKind::Low => 1.0 - cos,
        };
        let b1 = match kind {
            PassKind::High => -edge / scale,
            PassKind::Low => edge / scale,
        };
        Self {
            b: [edge * 0.5 / scale, b1, edge * 0.5 / scale],
            a: [-2.0 * cos / scale, (1.0 - alpha) / scale],
            input: [[0.0; 2]; 2],
            output: [[0.0; 2]; 2],
            boost: config.boost,
        }
    }

    pub(super) fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let input = *sample;
                let output = self.b[0] * f64::from(input)
                    + self.b[1] * f64::from(self.input[channel][0])
                    + self.b[2] * f64::from(self.input[channel][1])
                    - self.a[0] * f64::from(self.output[channel][0])
                    - self.a[1] * f64::from(self.output[channel][1]);
                self.input[channel][1] = self.input[channel][0];
                self.input[channel][0] = input;
                self.output[channel][1] = self.output[channel][0];
                self.output[channel][0] = output as f32;
                *sample = (output as f32 * self.boost).clamp(-1.0, 1.0);
            }
        }
    }
}
