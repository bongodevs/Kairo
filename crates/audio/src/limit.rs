use crate::CHANNELS;

const ACTIVATION_THRESHOLD: f32 = 0.94;
const THRESHOLD: f32 = 0.85;
const CEILING: f32 = 0.90;
const ATTACK: f32 = 0.8;
const RELEASE: f32 = 0.0005;
const SNAP_TO_UNITY: f32 = 0.001;

/// Stateful stereo output safety immediately before Opus encoding.
pub(crate) struct Limiter {
    gain: f32,
}

impl Limiter {
    pub(crate) const fn new() -> Self {
        Self { gain: 1.0 }
    }

    pub(crate) fn reset(&mut self) {
        self.gain = 1.0;
    }

    pub(crate) fn process(&mut self, samples: &mut [f32]) {
        let mut gain = self.gain;
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            for sample in frame.iter_mut() {
                if !sample.is_finite() {
                    *sample = 0.0;
                }
            }
            let peak = frame.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            if gain == 1.0 && peak <= ACTIVATION_THRESHOLD {
                continue;
            }

            let target = if peak > ACTIVATION_THRESHOLD { THRESHOLD / peak } else { 1.0 };
            let speed = if target < gain { ATTACK } else { RELEASE };
            gain += (target - gain) * speed;
            for sample in frame {
                *sample = soft_knee(*sample * gain).clamp(-1.0, 1.0);
            }
        }
        if 1.0 - gain < SNAP_TO_UNITY {
            gain = 1.0;
        }
        self.gain = gain;
    }

    #[cfg(test)]
    pub(crate) const fn gain(&self) -> f32 {
        self.gain
    }
}

#[inline]
fn soft_knee(sample: f32) -> f32 {
    let level = sample.abs();
    if level <= THRESHOLD {
        return sample;
    }
    let width = CEILING - THRESHOLD;
    let over = level - THRESHOLD;
    let limited = THRESHOLD + width * over / (width + over);
    limited.copysign(sample)
}

#[cfg(test)]
#[path = "../tests/limit.rs"]
mod tests;
