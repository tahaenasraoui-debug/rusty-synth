use std::f32::consts::PI;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::audio::stream::{AudioFrame, SharedSynthState, AUDIO_FRAME_SAMPLES};

pub const FFT_SIZE: usize = 2048;
pub const NUM_BINS: usize = 28;
pub const SCOPE_SAMPLES: usize = 256;

const MIN_FREQ_HZ: f32 = 45.0;
const MAX_FREQ_HZ: f32 = 12_000.0;

#[derive(Clone, Copy)]
pub struct VisualizerSnapshot {
    pub spectrum_bins: [u64; NUM_BINS],
    pub oscilloscope: [f32; SCOPE_SAMPLES],
    pub peak_db: f32,
}

impl Default for VisualizerSnapshot {
    fn default() -> Self {
        Self {
            spectrum_bins: [0; NUM_BINS],
            oscilloscope: [0.0; SCOPE_SAMPLES],
            peak_db: -60.0,
        }
    }
}

/// Returns the `(low_hz, high_hz)` logarithmic frequency bounds for spectrum band `bin`.
pub fn bin_freq_bounds(bin: usize) -> (f32, f32) {
    let log_ratio = (MAX_FREQ_HZ / MIN_FREQ_HZ).ln();
    let f_low = MIN_FREQ_HZ * (log_ratio * (bin as f32 / NUM_BINS as f32)).exp();
    let f_high = MIN_FREQ_HZ * (log_ratio * ((bin + 1) as f32 / NUM_BINS as f32)).exp();
    (f_low, f_high)
}

/// Maps spectrum band `bin` to half-open FFT bin indices `[idx_low, idx_high)` for `sample_rate`.
pub fn bin_fft_range(bin: usize, sample_rate: f32) -> (usize, usize) {
    let sr = sample_rate.max(44_100.0);
    let nyquist_bins = FFT_SIZE / 2;
    let (f_low, f_high) = bin_freq_bounds(bin);

    let idx_low = ((f_low * FFT_SIZE as f32 / sr).floor() as usize).clamp(1, nyquist_bins - 1);
    let idx_high =
        ((f_high * FFT_SIZE as f32 / sr).ceil() as usize).clamp(idx_low + 1, nyquist_bins);
    (idx_low, idx_high)
}

/// Stateful 2048-point Hann-windowed FFT spectrum analyzer and oscilloscope sampler.
pub struct FftAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    hann_window: Vec<f32>,
    ring_buffer: [f32; FFT_SIZE],
    write_pos: usize,
    fft_buffer: Vec<Complex<f32>>,
    smoothed_bins: [f32; NUM_BINS],
    scope_buffer: [f32; SCOPE_SAMPLES],
}

impl Default for FftAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl FftAnalyzer {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos()))
            .collect();

        Self {
            fft,
            hann_window,
            ring_buffer: [0.0_f32; FFT_SIZE],
            write_pos: 0,
            fft_buffer: vec![
                Complex {
                    re: 0.0_f32,
                    im: 0.0_f32
                };
                FFT_SIZE
            ],
            smoothed_bins: [0.0_f32; NUM_BINS],
            scope_buffer: [0.0_f32; SCOPE_SAMPLES],
        }
    }

    pub fn push_frame(&mut self, frame: &AudioFrame) {
        let count = frame.count.min(AUDIO_FRAME_SAMPLES);
        for &s in &frame.samples[..count] {
            self.ring_buffer[self.write_pos] = s;
            self.write_pos = (self.write_pos + 1) % FFT_SIZE;
        }
        let scope_count = count.min(SCOPE_SAMPLES);
        self.scope_buffer[..scope_count].copy_from_slice(&frame.samples[..scope_count]);
    }

    /// Computes a new FFT snapshot from the current ring buffer and applies ballistics.
    pub fn process(&mut self, sample_rate: f32) -> VisualizerSnapshot {
        for (i, slot) in self.fft_buffer.iter_mut().enumerate() {
            let idx = (self.write_pos + i) % FFT_SIZE;
            slot.re = self.ring_buffer[idx] * self.hann_window[i];
            slot.im = 0.0;
        }

        self.fft.process(&mut self.fft_buffer);

        let mut output_bins = [0_u64; NUM_BINS];
        let mut max_frame_db = -60.0_f32;

        for (b, out_bin) in output_bins.iter_mut().enumerate() {
            let (idx_low, idx_high) = bin_fft_range(b, sample_rate);

            let mut peak_mag = 0.0_f32;
            for c in &self.fft_buffer[idx_low..idx_high] {
                let mag = (c.re * c.re + c.im * c.im).sqrt() / (FFT_SIZE as f32 * 0.25);
                if mag > peak_mag {
                    peak_mag = mag;
                }
            }

            let db = 20.0 * (peak_mag + 1e-6).log10();
            if db > max_frame_db {
                max_frame_db = db;
            }
            let normalized = ((db + 60.0) / 60.0 * 100.0).clamp(0.0, 100.0);

            if normalized > self.smoothed_bins[b] {
                self.smoothed_bins[b] = self.smoothed_bins[b] * 0.30 + normalized * 0.70;
            } else {
                self.smoothed_bins[b] = self.smoothed_bins[b] * 0.78 + normalized * 0.22;
            }

            *out_bin = self.smoothed_bins[b].round() as u64;
        }

        VisualizerSnapshot {
            spectrum_bins: output_bins,
            oscilloscope: self.scope_buffer,
            peak_db: max_frame_db.clamp(-60.0, 0.0),
        }
    }

    /// Decays visualizer state on audio receive timeout without re-evaluating stale ring buffer data.
    pub fn decay(&mut self) -> VisualizerSnapshot {
        let mut output_bins = [0_u64; NUM_BINS];
        let mut max_smoothed = 0.0_f32;

        for (smoothed, out_bin) in self.smoothed_bins.iter_mut().zip(output_bins.iter_mut()) {
            *smoothed *= 0.82;
            if *smoothed < 0.05 {
                *smoothed = 0.0;
            }
            if *smoothed > max_smoothed {
                max_smoothed = *smoothed;
            }
            *out_bin = smoothed.round() as u64;
        }

        self.scope_buffer.fill(0.0);
        let decayed_peak_db = ((max_smoothed / 100.0) * 60.0 - 60.0).clamp(-60.0, 0.0);

        VisualizerSnapshot {
            spectrum_bins: output_bins,
            oscilloscope: self.scope_buffer,
            peak_db: decayed_peak_db,
        }
    }
}

pub fn run_fft_thread(
    shared: Arc<SharedSynthState>,
    audio_rx: Receiver<AudioFrame>,
    snapshot_tx: Sender<VisualizerSnapshot>,
) {
    let mut analyzer = FftAnalyzer::new();

    while shared.running.load(Ordering::Relaxed) {
        let snapshot = match audio_rx.recv_timeout(Duration::from_millis(16)) {
            Ok(frame) => {
                analyzer.push_frame(&frame);
                while let Ok(newer_frame) = audio_rx.try_recv() {
                    analyzer.push_frame(&newer_frame);
                }
                let sample_rate = shared.sample_rate.load(Ordering::Relaxed).max(44_100) as f32;
                analyzer.process(sample_rate)
            }
            Err(RecvTimeoutError::Timeout) => analyzer.decay(),
            Err(RecvTimeoutError::Disconnected) => break,
        };

        let _ = snapshot_tx.try_send(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;

    use crossbeam_channel::bounded;

    use super::*;

    fn push_sine_wave(analyzer: &mut FftAnalyzer, freq_hz: f32, sample_rate: f32) {
        let total_samples = FFT_SIZE;
        let mut sample_idx = 0_usize;
        while sample_idx < total_samples {
            let mut frame = AudioFrame {
                samples: [0.0; AUDIO_FRAME_SAMPLES],
                count: AUDIO_FRAME_SAMPLES,
            };
            for slot in &mut frame.samples {
                let t = sample_idx as f32 / sample_rate;
                *slot = (TAU * freq_hz * t).sin();
                sample_idx += 1;
            }
            analyzer.push_frame(&frame);
        }
    }

    #[test]
    fn logarithmic_bins_cover_45hz_to_12khz_monotonically() {
        let mut prev_high = 0.0_f32;
        for b in 0..NUM_BINS {
            let (low, high) = bin_freq_bounds(b);
            assert!(low < high);
            assert!(low >= prev_high - 1e-4);
            prev_high = high;

            for sr in [44_100.0, 48_000.0] {
                let (idx_low, idx_high) = bin_fft_range(b, sr);
                assert!(idx_low >= 1);
                assert!(idx_low < idx_high);
                assert!(idx_high <= FFT_SIZE / 2);
            }
        }
        let (first_low, _) = bin_freq_bounds(0);
        let (_, last_high) = bin_freq_bounds(NUM_BINS - 1);
        assert!((first_low - 45.0).abs() < 1e-3);
        assert!((last_high - 12_000.0).abs() < 1.0);
    }

    #[test]
    fn fft_maps_440hz_and_4000hz_tones_to_matching_bins() {
        let sample_rate = 48_000.0;

        let mut analyzer_low = FftAnalyzer::new();
        push_sine_wave(&mut analyzer_low, 440.0, sample_rate);
        let snap_low = analyzer_low.process(sample_rate);
        let peak_bin_440 = snap_low
            .spectrum_bins
            .iter()
            .enumerate()
            .max_by_key(|&(_, val)| val)
            .map(|(idx, _)| idx)
            .unwrap();
        let (f_low, f_high) = bin_freq_bounds(peak_bin_440);
        assert!(
            (f_low..=f_high).contains(&440.0),
            "Expected 440 Hz in [{f_low}, {f_high}] (bin {peak_bin_440})"
        );

        let mut analyzer_high = FftAnalyzer::new();
        push_sine_wave(&mut analyzer_high, 4_000.0, sample_rate);
        let snap_high = analyzer_high.process(sample_rate);
        let peak_bin_4k = snap_high
            .spectrum_bins
            .iter()
            .enumerate()
            .max_by_key(|&(_, val)| val)
            .map(|(idx, _)| idx)
            .unwrap();
        let (f_low_h, f_high_h) = bin_freq_bounds(peak_bin_4k);
        assert!(
            (f_low_h..=f_high_h).contains(&4_000.0),
            "Expected 4000 Hz in [{f_low_h}, {f_high_h}] (bin {peak_bin_4k})"
        );
        assert!(peak_bin_4k > peak_bin_440);
    }

    #[test]
    fn timeout_decay_does_not_overwrite_with_stale_ring_buffer() {
        let mut analyzer = FftAnalyzer::new();
        push_sine_wave(&mut analyzer, 440.0, 48_000.0);

        let initial = analyzer.process(48_000.0);
        let peak_idx = initial
            .spectrum_bins
            .iter()
            .enumerate()
            .max_by_key(|&(_, val)| val)
            .map(|(idx, _)| idx)
            .unwrap();
        let initial_peak = initial.spectrum_bins[peak_idx];
        assert!(initial_peak > 40);

        // Consecutive timeout decays must monotonically drop without snapping back up
        let mut prev_val = initial_peak;
        for _ in 0..30 {
            let decayed = analyzer.decay();
            let cur_val = decayed.spectrum_bins[peak_idx];
            assert!(
                cur_val <= prev_val,
                "decayed bin climbed from {prev_val} to {cur_val}"
            );
            assert!(decayed.oscilloscope.iter().all(|&s| s == 0.0));
            prev_val = cur_val;
        }
        assert_eq!(prev_val, 0);
    }

    #[test]
    fn fft_thread_decays_on_recv_timeout_and_exits_on_disconnect() {
        let shared = Arc::new(SharedSynthState::new());
        let (audio_tx, audio_rx) = bounded::<AudioFrame>(8);
        let (snap_tx, snap_rx) = bounded::<VisualizerSnapshot>(16);

        let mut frame = AudioFrame {
            samples: [0.0; AUDIO_FRAME_SAMPLES],
            count: AUDIO_FRAME_SAMPLES,
        };
        for (i, slot) in frame.samples.iter_mut().enumerate() {
            *slot = (TAU * 440.0 * (i as f32 / 48_000.0)).sin();
        }
        audio_tx.send(frame).unwrap();

        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::spawn(move || {
            run_fft_thread(thread_shared, audio_rx, snap_tx);
        });

        let first = snap_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let first_max = *first.spectrum_bins.iter().max().unwrap();
        assert!(first_max > 0);

        // Wait for at least two 16ms timeout decay snapshots while no new audio arrives
        let _second = snap_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let third = snap_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let third_max = *third.spectrum_bins.iter().max().unwrap();
        assert!(third_max < first_max);

        drop(audio_tx);
        shared.running.store(false, Ordering::Relaxed);
        handle.join().unwrap();
    }
}
