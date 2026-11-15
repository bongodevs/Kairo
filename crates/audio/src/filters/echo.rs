#[cfg(test)]
#[path = "../../tests/filters_echo_scale.rs"]
mod scale_tests;
#[cfg(test)]
#[path = "../../tests/filters_echo.rs"]
mod tests;

use kairo_core::filters::{EchoConfig, FilterError, MAX_ECHO_SECONDS};

use crate::{CHANNELS, SAMPLE_RATE};

const MAX_ECHO_FRAMES: usize = (SAMPLE_RATE as f32 * MAX_ECHO_SECONDS) as usize;

pub struct Echo {
    delay: Box<[[f32; CHANNELS]]>,
    position: usize,
    decay: f32,
}

impl Echo {
    pub fn new(config: EchoConfig) -> Result<Self, FilterError> {
        config.validate()?;
        let frames = (SAMPLE_RATE as f32 * config.length_seconds) as usize;
        if frames == 0 {
            return Err(FilterError::InvalidEchoLength);
        }
        debug_assert!(frames <= MAX_ECHO_FRAMES);
        Ok(Self {
            delay: vec![[0.0; CHANNELS]; frames].into_boxed_slice(),
            position: 0,
            decay: config.decay,
        })
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let delayed = self.delay[self.position];
            frame[0] += delayed[0] * self.decay;
            frame[1] += delayed[1] * self.decay;
            self.delay[self.position] = *frame;
            self.position += 1;
            if self.position == self.delay.len() {
                self.position = 0;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn delay_frames(&self) -> usize {
        self.delay.len()
    }
}
