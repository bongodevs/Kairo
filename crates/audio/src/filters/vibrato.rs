#[cfg(test)]
#[path = "../../tests/filters_vibrato.rs"]
mod tests;

use kairo_core::filters::{FilterError, VibratoConfig};

use crate::{CHANNELS, SAMPLE_RATE};

const MAX_DELAY_FRAMES: usize = SAMPLE_RATE as usize * 2 / 1_000;
const RING_FRAMES: usize = MAX_DELAY_FRAMES * 2;
const MARGIN: usize = 3;
const STORAGE_FRAMES: usize = RING_FRAMES + MARGIN;

pub struct Vibrato {
    buffers: [[f32; STORAGE_FRAMES]; CHANNELS],
    write: usize,
    phase: f32,
    step: f32,
    depth: f32,
}

impl Vibrato {
    pub fn new(config: VibratoConfig) -> Result<Self, FilterError> {
        config.validate()?;
        Ok(Self {
            buffers: [[0.0; STORAGE_FRAMES]; CHANNELS],
            write: 0,
            phase: 0.0,
            step: std::f32::consts::TAU * config.frequency / SAMPLE_RATE as f32,
            depth: config.depth,
        })
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let lfo = (self.phase.sin() + 1.0) * 0.5;
            let delay = lfo * self.depth * MAX_DELAY_FRAMES as f32 + MARGIN as f32;
            for (channel, sample) in frame.iter_mut().enumerate() {
                let input = *sample;
                *sample = hermite(&self.buffers[channel], self.write, delay);
                write(&mut self.buffers[channel], self.write, input);
            }
            self.write += 1;
            if self.write == RING_FRAMES {
                self.write = 0;
            }
            self.phase = (self.phase + self.step).rem_euclid(std::f32::consts::TAU);
        }
    }

    #[must_use]
    pub fn phase(&self) -> f32 {
        self.phase
    }
}

fn write(buffer: &mut [f32; STORAGE_FRAMES], index: usize, sample: f32) {
    buffer[index] = sample;
    if index < MARGIN {
        buffer[RING_FRAMES + index] = sample;
    }
}

fn hermite(buffer: &[f32; STORAGE_FRAMES], write: usize, delay: f32) -> f32 {
    let mut read = write as f32 - 1.0 - delay;
    if read < 0.0 {
        read += RING_FRAMES as f32;
    }
    if read >= RING_FRAMES as f32 {
        read -= RING_FRAMES as f32;
    }
    let index = read as usize;
    let fraction = read - index as f32;
    let [y0, y1, y2, y3] = buffer[index..index + 4] else {
        return 0.0;
    };
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * fraction + c2) * fraction + c1) * fraction + y1
}
