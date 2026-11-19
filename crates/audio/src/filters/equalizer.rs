use kairo_core::filters::{BAND_COUNT, EqualizerConfig};

use crate::CHANNELS;

const HISTORY_LENGTH: usize = BAND_COUNT * 6;

pub struct Equalizer {
    config: EqualizerConfig,
    history: [[f32; HISTORY_LENGTH]; CHANNELS],
    active_bands: [usize; BAND_COUNT],
    active_count: usize,
    current: usize,
    minus_one: usize,
    minus_two: usize,
}

impl Equalizer {
    #[must_use]
    pub fn new(config: EqualizerConfig) -> Self {
        let mut equalizer = Self {
            config,
            history: [[0.0; HISTORY_LENGTH]; CHANNELS],
            active_bands: [0; BAND_COUNT],
            active_count: 0,
            current: 0,
            minus_one: 2,
            minus_two: 1,
        };
        equalizer.refresh_active_bands();
        equalizer
    }

    pub fn set_config(&mut self, config: EqualizerConfig) {
        self.config = config;
        self.refresh_active_bands();
        self.reset();
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.is_enabled()
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.is_enabled() {
            return;
        }
        let gains = *self.config.gains();
        for frame in samples.as_chunks_mut::<CHANNELS>().0 {
            let input = [frame[0], frame[1]];
            let mut result = [input[0] * 0.25, input[1] * 0.25];
            for &band in &self.active_bands[..self.active_count] {
                let x = band * 6;
                let y = x + 3;
                let coefficient = COEFFICIENTS[band];
                for channel in 0..CHANNELS {
                    let history = &mut self.history[channel];
                    let band_result = coefficient.alpha
                        * (input[channel] - history[x + self.minus_two])
                        + coefficient.gamma * history[y + self.minus_one]
                        - coefficient.beta * history[y + self.minus_two];
                    history[x + self.current] = input[channel];
                    history[y + self.current] = band_result;
                    result[channel] += band_result * gains[band];
                }
            }
            frame[0] = (result[0] * 4.0).clamp(-1.0, 1.0);
            frame[1] = (result[1] * 4.0).clamp(-1.0, 1.0);
            self.advance();
        }
    }

    pub fn reset(&mut self) {
        self.history.fill([0.0; HISTORY_LENGTH]);
        self.current = 0;
        self.minus_one = 2;
        self.minus_two = 1;
    }

    fn refresh_active_bands(&mut self) {
        self.active_count = 0;
        for (band, gain) in self.config.gains().iter().enumerate() {
            if *gain != 0.0 {
                self.active_bands[self.active_count] = band;
                self.active_count += 1;
            }
        }
    }

    fn advance(&mut self) {
        self.current += 1;
        if self.current == 3 {
            self.current = 0;
        }
        self.minus_one += 1;
        if self.minus_one == 3 {
            self.minus_one = 0;
        }
        self.minus_two += 1;
        if self.minus_two == 3 {
            self.minus_two = 0;
        }
    }
}

#[derive(Clone, Copy)]
struct Coefficients {
    beta: f32,
    alpha: f32,
    gamma: f32,
}

#[allow(clippy::excessive_precision)]
const COEFFICIENTS: [Coefficients; BAND_COUNT] = [
    Coefficients { beta: 9.9847546664e-1, alpha: 7.6226668143e-4, gamma: 1.9984647656 },
    Coefficients { beta: 9.9756184654e-1, alpha: 1.2190767289e-3, gamma: 1.9975344645 },
    Coefficients { beta: 9.9616261379e-1, alpha: 1.9186931041e-3, gamma: 1.9960947369 },
    Coefficients { beta: 9.9391578543e-1, alpha: 3.0421072865e-3, gamma: 1.9937449618 },
    Coefficients { beta: 9.9028307215e-1, alpha: 4.8584639242e-3, gamma: 1.9898465702 },
    Coefficients { beta: 9.8485897264e-1, alpha: 7.5705136795e-3, gamma: 1.9837962543 },
    Coefficients { beta: 9.7588512657e-1, alpha: 1.2057436715e-2, gamma: 1.9731772447 },
    Coefficients { beta: 9.6228521814e-1, alpha: 1.8857390928e-2, gamma: 1.9556164694 },
    Coefficients { beta: 9.4080933132e-1, alpha: 2.9595334338e-2, gamma: 1.9242054384 },
    Coefficients { beta: 9.0702059196e-1, alpha: 4.6489704022e-2, gamma: 1.8653476166 },
    Coefficients { beta: 8.5868004289e-1, alpha: 7.0659978553e-2, gamma: 1.7600401337 },
    Coefficients { beta: 7.8409610788e-1, alpha: 1.0795194606e-1, gamma: 1.5450725522 },
    Coefficients { beta: 6.8332861002e-1, alpha: 1.5833569499e-1, gamma: 1.1426447155 },
    Coefficients { beta: 5.5267518228e-1, alpha: 2.2366240886e-1, gamma: 4.0186190803e-1 },
    Coefficients { beta: 4.1811888447e-1, alpha: 2.9094055777e-1, gamma: -7.0905944223e-1 },
];
