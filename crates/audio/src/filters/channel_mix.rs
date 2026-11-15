#[cfg(test)]
#[path = "../../tests/filters_channel_mix.rs"]
mod tests;

use kairo_core::filters::ChannelMixConfig;

use crate::CHANNELS;

pub struct ChannelMix {
    config: ChannelMixConfig,
}

impl ChannelMix {
    #[must_use]
    pub fn new(config: ChannelMixConfig) -> Self {
        Self { config }
    }

    pub fn process(&self, samples: &mut [f32]) {
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let left = frame[0];
            let right = frame[1];
            frame[0] = (self.config.left_to_left * left + self.config.right_to_left * right)
                .clamp(-1.0, 1.0);
            frame[1] = (self.config.left_to_right * left + self.config.right_to_right * right)
                .clamp(-1.0, 1.0);
        }
    }
}
