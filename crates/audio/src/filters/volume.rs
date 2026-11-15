use super::FilterError;

const UNITY: f32 = 1.0;
const BYPASS_DISTANCE: f32 = 0.02;
const CURVE_SPLIT: f32 = 1.5;

/// Floating-point volume from the aggregate filter set.
pub struct FilterVolume {
    multiplier: Option<f32>,
}

impl FilterVolume {
    pub fn new(volume: f32) -> Result<Self, FilterError> {
        validate(volume)?;
        let multiplier = if (UNITY - volume).abs() < BYPASS_DISTANCE {
            None
        } else if volume <= CURVE_SPLIT {
            Some((f64::from(volume * 0.79).tan() * 10_000.0) as f32)
        } else {
            Some(24_612.0 * (volume * 100.0) / 150.0)
        };
        Ok(Self { multiplier })
    }

    pub fn process(&self, samples: &mut [f32]) {
        let Some(multiplier) = self.multiplier else {
            return;
        };
        for sample in samples {
            *sample = (*sample * multiplier / 10_000.0).clamp(-1.0, 1.0);
        }
    }
}

pub(super) fn validate(volume: f32) -> Result<(), FilterError> {
    if !volume.is_finite() || !(0.0..=5.0).contains(&volume) {
        return Err(FilterError::InvalidFilterVolume);
    }
    Ok(())
}
