use std::f32::consts::TAU;

use crate::dsp::envelope::{AdsrParams, Envelope, EnvelopeState};
use crate::dsp::oscillator::{midi_to_freq, Oscillator, Waveform};

pub const MAX_VOICES: usize = 8;

#[derive(Debug, Clone)]
pub struct Voice {
    oscillator: Oscillator,
    envelope: Envelope,
    note: Option<u8>,
    age: u64,
}

impl Voice {
    pub fn new(sample_rate: f32, params: AdsrParams) -> Self {
        Self {
            oscillator: Oscillator::new(sample_rate),
            envelope: Envelope::new(sample_rate, params),
            note: None,
            age: 0,
        }
    }

    #[inline]
    pub fn trigger(&mut self, note: u8, waveform: Waveform, age: u64) {
        self.oscillator.set_frequency(midi_to_freq(note));
        self.oscillator.set_waveform(waveform);
        if !self.envelope.is_active() {
            self.oscillator.reset_phase();
        }
        self.note = Some(note);
        self.age = age;
        self.envelope.note_on();
    }

    #[inline]
    pub fn release(&mut self) {
        self.envelope.note_off();
    }

    #[inline]
    pub fn is_active(&self) -> bool {
        self.envelope.is_active()
    }

    #[inline]
    pub fn note(&self) -> Option<u8> {
        if self.is_active() {
            self.note
        } else {
            None
        }
    }

    #[inline]
    pub fn envelope_state(&self) -> EnvelopeState {
        self.envelope.state()
    }

    #[inline]
    pub fn set_waveform(&mut self, waveform: Waveform) {
        self.oscillator.set_waveform(waveform);
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        if !self.envelope.is_active() {
            self.note = None;
            return 0.0;
        }
        let env = self.envelope.next_sample();
        let osc = self.oscillator.next_sample();
        osc * env
    }
}

#[derive(Debug, Clone)]
pub struct OnePoleLowPass {
    z1: f32,
    a0: f32,
    b1: f32,
    sample_rate: f32,
    last_cutoff_hz: f32,
    #[cfg(test)]
    coeff_recomputations: usize,
}

impl OnePoleLowPass {
    pub fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        let mut filter = Self {
            z1: 0.0,
            a0: 1.0,
            b1: 0.0,
            sample_rate: sample_rate.max(1.0),
            last_cutoff_hz: -1.0,
            #[cfg(test)]
            coeff_recomputations: 0,
        };
        let initial_cutoff = if cutoff_hz.is_finite() {
            cutoff_hz
        } else {
            4500.0
        };
        filter.set_cutoff(initial_cutoff);
        filter
    }

    #[inline]
    pub fn set_cutoff(&mut self, cutoff_hz: f32) {
        if !cutoff_hz.is_finite() {
            return;
        }
        let nyquist = (self.sample_rate * 0.49).max(40.0);
        let clamped = cutoff_hz.clamp(40.0, nyquist);
        // Skip exp() when clamped cutoff matches the cached value.
        if clamped == self.last_cutoff_hz {
            return;
        }
        self.last_cutoff_hz = clamped;
        let fc = clamped / self.sample_rate;
        self.b1 = (-TAU * fc).exp();
        self.a0 = 1.0 - self.b1;
        #[cfg(test)]
        {
            self.coeff_recomputations += 1;
        }
    }

    #[inline]
    pub fn process(&mut self, input: f32) -> f32 {
        self.z1 = input * self.a0 + self.z1 * self.b1;
        self.z1
    }
}

pub struct VoiceManager {
    voices: [Voice; MAX_VOICES],
    generation_counter: u64,
    current_waveform: Waveform,
    filter: OnePoleLowPass,
}

impl VoiceManager {
    pub fn new(sample_rate: f32) -> Self {
        let adsr = AdsrParams::default();
        let voices = std::array::from_fn(|_| Voice::new(sample_rate, adsr));
        Self {
            voices,
            generation_counter: 0,
            current_waveform: Waveform::Sine,
            filter: OnePoleLowPass::new(sample_rate, 6000.0),
        }
    }

    pub fn set_waveform(&mut self, waveform: Waveform) {
        if self.current_waveform != waveform {
            self.current_waveform = waveform;
            for voice in &mut self.voices {
                voice.set_waveform(waveform);
            }
        }
    }

    pub fn note_on(&mut self, note: u8) {
        self.generation_counter = self.generation_counter.wrapping_add(1);
        let gen = self.generation_counter;

        for voice in &mut self.voices {
            if voice.is_active() && voice.note() == Some(note) {
                voice.trigger(note, self.current_waveform, gen);
                return;
            }
        }

        for voice in &mut self.voices {
            if !voice.is_active() {
                voice.trigger(note, self.current_waveform, gen);
                return;
            }
        }

        let oldest_releasing = self
            .voices
            .iter_mut()
            .filter(|v| v.envelope_state() == EnvelopeState::Release)
            .min_by_key(|v| v.age);

        if let Some(victim) = oldest_releasing {
            victim.trigger(note, self.current_waveform, gen);
            return;
        }

        if let Some(victim) = self.voices.iter_mut().min_by_key(|v| v.age) {
            victim.trigger(note, self.current_waveform, gen);
        }
    }

    pub fn note_off(&mut self, note: u8) {
        for voice in &mut self.voices {
            if voice.is_active() && voice.note() == Some(note) {
                voice.release();
            }
        }
    }

    #[inline]
    pub fn next_sample(&mut self, cutoff_hz: f32, master_gain: f32) -> (f32, u8) {
        self.filter.set_cutoff(cutoff_hz);

        let mut mix = 0.0_f32;
        let mut active_count = 0_u8;

        for voice in &mut self.voices {
            if voice.is_active() {
                active_count += 1;
                mix += voice.next_sample();
            }
        }

        let scaled = mix * 0.28;
        let filtered = self.filter.process(scaled);
        let output = (filtered * master_gain).tanh();

        (output, active_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_notes_set(vm: &VoiceManager) -> Vec<u8> {
        let mut notes: Vec<u8> = vm.voices.iter().filter_map(|v| v.note()).collect();
        notes.sort_unstable();
        notes
    }

    #[test]
    fn cutoff_caching_skips_redundant_recomputations() {
        let mut filter = OnePoleLowPass::new(48_000.0, 4500.0);
        assert_eq!(filter.coeff_recomputations, 1);
        let initial_b1 = filter.b1;

        for _ in 0..500 {
            filter.set_cutoff(4500.0);
        }
        assert_eq!(filter.coeff_recomputations, 1);
        assert!((filter.b1 - initial_b1).abs() < 1e-7);

        // Values above Nyquist clamp to the same frequency and also cache-hit
        filter.set_cutoff(30_000.0);
        assert_eq!(filter.coeff_recomputations, 2);
        filter.set_cutoff(40_000.0);
        assert_eq!(filter.coeff_recomputations, 2);

        // Non-finite cutoffs are safely ignored
        filter.set_cutoff(f32::NAN);
        filter.set_cutoff(f32::INFINITY);
        assert_eq!(filter.coeff_recomputations, 2);

        // Distinct cutoff updates coefficients
        filter.set_cutoff(1200.0);
        assert_eq!(filter.coeff_recomputations, 3);
        assert!(filter.b1 > initial_b1);
    }

    #[test]
    fn voice_manager_caches_cutoff_across_samples() {
        let mut vm = VoiceManager::new(48_000.0);
        vm.note_on(60);

        for _ in 0..1000 {
            let _ = vm.next_sample(4500.0, 0.75);
        }
        // 1 from VoiceManager::new(6000.0) + 1 from first next_sample(4500.0)
        assert_eq!(vm.filter.coeff_recomputations, 2);

        let _ = vm.next_sample(2200.0, 0.75);
        assert_eq!(vm.filter.coeff_recomputations, 3);
    }

    #[test]
    fn voice_retrigger_reuses_same_voice() {
        let mut vm = VoiceManager::new(48_000.0);
        vm.note_on(60);
        vm.note_on(60);

        let (_, active_count) = vm.next_sample(4500.0, 0.75);
        assert_eq!(active_count, 1);
        assert_eq!(active_notes_set(&vm), vec![60]);
    }

    #[test]
    fn voice_stealing_prefers_oldest_releasing_voice() {
        let mut vm = VoiceManager::new(48_000.0);
        for note in 60..68 {
            vm.note_on(note);
        }
        assert_eq!(active_notes_set(&vm), (60..68).collect::<Vec<u8>>());

        // Release note 64 (`age = 5`) and note 62 (`age = 3`).
        // Releasing voices take priority over older sustained note 60 (`age = 1`),
        // and among releasing voices the oldest note-on (`62`) is stolen first.
        vm.note_off(64);
        vm.note_off(62);

        vm.note_on(72);
        let active = active_notes_set(&vm);
        assert_eq!(active.len(), MAX_VOICES);
        assert!(!active.contains(&62));
        assert!(active.contains(&60));
        assert!(active.contains(&64));
        assert!(active.contains(&72));

        // Next note steals the remaining releasing voice (64) rather than sustained note 60.
        vm.note_on(74);
        let active = active_notes_set(&vm);
        assert_eq!(active.len(), MAX_VOICES);
        assert!(!active.contains(&64));
        assert!(active.contains(&60));
        assert!(active.contains(&74));
    }

    #[test]
    fn voice_stealing_falls_back_to_oldest_active_voice() {
        let mut vm = VoiceManager::new(48_000.0);
        for note in 60..68 {
            vm.note_on(note);
        }

        // No voices are releasing -> oldest active voice (60) must be stolen
        vm.note_on(72);
        let active = active_notes_set(&vm);
        assert_eq!(active.len(), MAX_VOICES);
        assert!(!active.contains(&60));
        assert!(active.contains(&72));
    }
}
