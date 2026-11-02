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
