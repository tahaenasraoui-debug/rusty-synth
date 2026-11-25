#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeState {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Debug, Clone, Copy)]
pub struct AdsrParams {
    pub attack_secs: f32,
    pub decay_secs: f32,
    pub sustain_level: f32,
    pub release_secs: f32,
}

impl Default for AdsrParams {
    fn default() -> Self {
        Self {
            attack_secs: 0.015,
            decay_secs: 0.120,
            sustain_level: 0.70,
            release_secs: 0.220,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Envelope {
    state: EnvelopeState,
    level: f32,
    sample_rate: f32,
    params: AdsrParams,
    attack_rate: f32,
    decay_rate: f32,
    release_rate: f32,
}

impl Envelope {
    pub fn new(sample_rate: f32, params: AdsrParams) -> Self {
        let mut env = Self {
            state: EnvelopeState::Idle,
            level: 0.0,
            sample_rate: sample_rate.max(1.0),
            params,
            attack_rate: 0.0,
            decay_rate: 0.0,
            release_rate: 0.0,
        };
        env.recalculate_rates();
        env
    }

    #[inline]
    fn recalculate_rates(&mut self) {
        let attack_samples = (self.params.attack_secs * self.sample_rate).max(1.0);
        let decay_samples = (self.params.decay_secs * self.sample_rate).max(1.0);
        let release_samples = (self.params.release_secs * self.sample_rate).max(1.0);

        self.attack_rate = 1.0 / attack_samples;
        self.decay_rate = (1.0 - self.params.sustain_level).max(0.0001) / decay_samples;
        self.release_rate = self.params.sustain_level.max(0.05) / release_samples;
    }

    #[inline]
    pub fn note_on(&mut self) {
        self.state = EnvelopeState::Attack;
    }

    #[inline]
    pub fn note_off(&mut self) {
        if self.state != EnvelopeState::Idle {
            let release_samples = (self.params.release_secs * self.sample_rate).max(1.0);
            self.release_rate = self.level.max(0.001) / release_samples;
            self.state = EnvelopeState::Release;
        }
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.state != EnvelopeState::Idle
    }

    #[inline]
    pub fn state(&self) -> EnvelopeState {
        self.state
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        match self.state {
            EnvelopeState::Idle => {
                self.level = 0.0;
            }
            EnvelopeState::Attack => {
                self.level += self.attack_rate;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.state = EnvelopeState::Decay;
                }
            }
            EnvelopeState::Decay => {
                self.level -= self.decay_rate;
                if self.level <= self.params.sustain_level {
                    self.level = self.params.sustain_level;
                    self.state = EnvelopeState::Sustain;
                }
            }
            EnvelopeState::Sustain => {
                self.level = self.params.sustain_level;
            }
            EnvelopeState::Release => {
                self.level -= self.release_rate;
                if self.level <= 0.0001 {
                    self.level = 0.0;
                    self.state = EnvelopeState::Idle;
                }
            }
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_transitions_through_full_adsr_lifecycle() {
        let params = AdsrParams {
            attack_secs: 0.01, // 10 samples at 1000 Hz
            decay_secs: 0.01,  // 10 samples at 1000 Hz
            sustain_level: 0.5,
            release_secs: 0.01, // 10 samples at 1000 Hz
        };
        let mut env = Envelope::new(1000.0, params);

        assert_eq!(env.state(), EnvelopeState::Idle);
        assert!(!env.is_active());
        assert_eq!(env.next_sample(), 0.0);

        // note_off while Idle should remain Idle
        env.note_off();
        assert_eq!(env.state(), EnvelopeState::Idle);

        // note_on enters Attack and ramps up to 1.0, transitioning to Decay
        env.note_on();
        assert_eq!(env.state(), EnvelopeState::Attack);
        assert!(env.is_active());

        let mut peak = 0.0_f32;
        for _ in 0..10 {
            peak = env.next_sample();
        }
        assert!((peak - 1.0).abs() < 1e-5);
        assert_eq!(env.state(), EnvelopeState::Decay);

        // Decay ramps down to sustain_level and transitions to Sustain
        let mut decayed = 0.0_f32;
        for _ in 0..10 {
            decayed = env.next_sample();
        }
        assert!((decayed - 0.5).abs() < 1e-5);
        assert_eq!(env.state(), EnvelopeState::Sustain);

        // Sustain holds steady indefinitely
        for _ in 0..20 {
            assert!((env.next_sample() - 0.5).abs() < 1e-5);
            assert_eq!(env.state(), EnvelopeState::Sustain);
        }

        // note_off enters Release and ramps down to Idle
        env.note_off();
        assert_eq!(env.state(), EnvelopeState::Release);

        let mut prev = 0.5;
        for _ in 0..15 {
            let cur = env.next_sample();
            assert!(cur <= prev);
            prev = cur;
        }
        assert_eq!(env.state(), EnvelopeState::Idle);
        assert!(!env.is_active());
        assert_eq!(env.next_sample(), 0.0);
    }

    #[test]
    fn early_release_and_retrigger_transition_smoothly() {
        let params = AdsrParams {
            attack_secs: 0.02, // 20 samples at 1000 Hz
            decay_secs: 0.02,
            sustain_level: 0.7,
            release_secs: 0.02,
        };
        let mut env = Envelope::new(1000.0, params);

        env.note_on();
        let mut level = 0.0;
        for _ in 0..10 {
            level = env.next_sample();
        }
        assert!((level - 0.5).abs() < 1e-5);
        assert_eq!(env.state(), EnvelopeState::Attack);

        // Release mid-attack: should ramp down from current ~0.5 level, not jump
        env.note_off();
        assert_eq!(env.state(), EnvelopeState::Release);
        let first_release = env.next_sample();
        assert!(first_release < level && first_release > 0.4);

        // Retrigger mid-release: returns to Attack without resetting level to 0
        env.note_on();
        assert_eq!(env.state(), EnvelopeState::Attack);
        let retriggered = env.next_sample();
        assert!(retriggered > first_release);
    }
}
