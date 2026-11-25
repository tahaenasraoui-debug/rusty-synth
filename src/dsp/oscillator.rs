use std::f32::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Waveform {
    Sine = 0,
    Square = 1,
    Sawtooth = 2,
    Triangle = 3,
    WhiteNoise = 4,
}

impl Waveform {
    pub fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::Sine,
            1 => Self::Square,
            2 => Self::Sawtooth,
            3 => Self::Triangle,
            _ => Self::WhiteNoise,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Sine => "Sine",
            Self::Square => "Square",
            Self::Sawtooth => "Sawtooth",
            Self::Triangle => "Triangle",
            Self::WhiteNoise => "White Noise",
        }
    }
}

#[inline]
pub fn midi_to_freq(note: u8) -> f32 {
    440.0 * 2.0_f32.powf((note as f32 - 69.0) / 12.0)
}

#[derive(Debug, Clone)]
pub struct Oscillator {
    phase: f32,
    phase_increment: f32,
    sample_rate: f32,
    waveform: Waveform,
    rng_state: u32,
}

impl Oscillator {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            phase: 0.0,
            phase_increment: 0.0,
            sample_rate: sample_rate.max(1.0),
            waveform: Waveform::Sine,
            rng_state: 0x1234_5678,
        }
    }

    #[inline]
    pub fn set_frequency(&mut self, freq: f32) {
        self.phase_increment = (freq / self.sample_rate).clamp(0.0, 0.5);
    }

    #[inline]
    pub fn set_waveform(&mut self, waveform: Waveform) {
        self.waveform = waveform;
    }

    #[inline]
    pub fn reset_phase(&mut self) {
        self.phase = 0.0;
    }

    #[inline]
    fn next_noise_sample(&mut self) -> f32 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        (x as f32 / (u32::MAX as f32 / 2.0)) - 1.0
    }

    #[inline]
    fn poly_blep(&self, mut t: f32) -> f32 {
        let dt = self.phase_increment;
        if dt <= f32::EPSILON {
            return 0.0;
        }
        if t < dt {
            t /= dt;
            t + t - t * t - 1.0
        } else if t > 1.0 - dt {
            t = (t - 1.0) / dt;
            t * t + t + t + 1.0
        } else {
            0.0
        }
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let t = self.phase;
        let sample = match self.waveform {
            Waveform::Sine => (t * TAU).sin(),
            Waveform::Square => {
                let raw = if t < 0.5 { 1.0 } else { -1.0 };
                raw + self.poly_blep(t) - self.poly_blep((t + 0.5).fract())
            }
            Waveform::Sawtooth => {
                let raw = 2.0 * t - 1.0;
                raw - self.poly_blep(t)
            }
            Waveform::Triangle => 4.0 * (t - (t + 0.5).floor()).abs() - 1.0,
            Waveform::WhiteNoise => self.next_noise_sample() * 0.6,
        };

        self.phase += self.phase_increment;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        sample
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_to_freq_computes_12_tet_pitches() {
        assert!((midi_to_freq(69) - 440.0).abs() < 1e-4);
        assert!((midi_to_freq(81) - 880.0).abs() < 1e-4);
        assert!((midi_to_freq(57) - 220.0).abs() < 1e-4);
        assert!((midi_to_freq(60) - 261.625_58).abs() < 1e-3);
    }

    #[test]
    fn waveform_from_u8_and_names() {
        assert_eq!(Waveform::from_u8(0), Waveform::Sine);
        assert_eq!(Waveform::from_u8(1), Waveform::Square);
        assert_eq!(Waveform::from_u8(2), Waveform::Sawtooth);
        assert_eq!(Waveform::from_u8(3), Waveform::Triangle);
        assert_eq!(Waveform::from_u8(4), Waveform::WhiteNoise);
        assert_eq!(Waveform::from_u8(255), Waveform::WhiteNoise);

        assert_eq!(Waveform::Sine.name(), "Sine");
        assert_eq!(Waveform::Square.name(), "Square");
        assert_eq!(Waveform::Sawtooth.name(), "Sawtooth");
        assert_eq!(Waveform::Triangle.name(), "Triangle");
        assert_eq!(Waveform::WhiteNoise.name(), "White Noise");
    }

    #[test]
    fn sine_waveform_is_bounded_and_zero_mean() {
        let mut osc = Oscillator::new(48_000.0);
        osc.set_waveform(Waveform::Sine);
        osc.set_frequency(480.0); // Exact 100-sample period

        let samples: Vec<f32> = (0..100).map(|_| osc.next_sample()).collect();
        let sum: f32 = samples.iter().sum();
        let max = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let min = samples.iter().copied().fold(f32::INFINITY, f32::min);

        assert!(samples[0].abs() < 1e-6);
        assert!((samples[25] - 1.0).abs() < 1e-5);
        assert!((samples[75] + 1.0).abs() < 1e-5);
        assert!(max <= 1.0 + 1e-6 && min >= -1.0 - 1e-6);
        assert!((sum / 100.0).abs() < 1e-5);
    }

    #[test]
    fn polyblep_square_smooths_edges_and_maintains_levels() {
        let mut osc = Oscillator::new(48_000.0);
        osc.set_waveform(Waveform::Square);
        osc.set_frequency(480.0); // 100 samples/period, dt = 0.01

        let samples: Vec<f32> = (0..200).map(|_| osc.next_sample()).collect();

        // At t = 0.0 (rising transition), PolyBLEP centers the step near 0.0 instead of +1.0
        assert!(samples[0].abs() < 1e-5);
        // Well inside the positive and negative halves, plateau should be +/-1.0
        assert!((samples[20] - 1.0).abs() < 1e-5);
        assert!((samples[70] + 1.0).abs() < 1e-5);

        let dc: f32 = samples[..100].iter().sum::<f32>() / 100.0;
        assert!(dc.abs() < 1e-4);
        assert!(samples.iter().all(|s| s.is_finite() && s.abs() <= 1.05));
    }

    #[test]
    fn polyblep_sawtooth_smooths_wrap_and_follows_ramp() {
        let mut osc = Oscillator::new(48_000.0);
        osc.set_waveform(Waveform::Sawtooth);
        osc.set_frequency(480.0); // 100 samples/period

        let samples: Vec<f32> = (0..100).map(|_| osc.next_sample()).collect();

        // At t = 0.0 (wrap discontinuity), PolyBLEP cancels the -1.0 jump to ~0.0
        assert!(samples[0].abs() < 1e-5);
        // Linear ramp away from edges: t = 0.25 -> -0.5, t = 0.50 -> 0.0, t = 0.75 -> +0.5
        assert!((samples[25] + 0.5).abs() < 1e-5);
        assert!(samples[50].abs() < 1e-5);
        assert!((samples[75] - 0.5).abs() < 1e-5);

        let dc: f32 = samples.iter().sum::<f32>() / 100.0;
        assert!(dc.abs() < 1e-4);
        assert!(samples.iter().all(|s| s.is_finite() && s.abs() <= 1.05));
    }

    #[test]
    fn triangle_and_white_noise_waveforms_behave_correctly() {
        let mut osc = Oscillator::new(48_000.0);
        osc.set_waveform(Waveform::Triangle);
        osc.set_frequency(480.0); // 100 samples/period

        let tri: Vec<f32> = (0..100).map(|_| osc.next_sample()).collect();
        assert!((tri[0] + 1.0).abs() < 1e-5);
        assert!(tri[25].abs() < 1e-5);
        assert!((tri[50] - 1.0).abs() < 1e-5);
        assert!(tri[75].abs() < 1e-5);
        assert!((tri.iter().sum::<f32>() / 100.0).abs() < 1e-5);

        osc.set_waveform(Waveform::WhiteNoise);
        let noise: Vec<f32> = (0..256).map(|_| osc.next_sample()).collect();
        assert!(noise.iter().all(|s| s.is_finite() && s.abs() <= 0.6 + 1e-5));
        assert!(noise.windows(2).any(|w| (w[0] - w[1]).abs() > 1e-4));
    }
}
