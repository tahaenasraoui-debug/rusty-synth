use std::f32::consts::PI;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use num_complex::Complex;
use rustfft::FftPlanner;

use crate::audio::stream::{AudioFrame, SharedSynthState};

pub const FFT_SIZE: usize = 2048;
pub const NUM_BINS: usize = 28;
pub const SCOPE_SAMPLES: usize = 256;

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

pub fn run_fft_thread(
    shared: Arc<SharedSynthState>,
    audio_rx: Receiver<AudioFrame>,
    snapshot_tx: Sender<VisualizerSnapshot>,
) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);

    let hann_window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos()))
        .collect();

    let mut ring_buffer = [0.0_f32; FFT_SIZE];
    let mut write_pos = 0_usize;
    let mut fft_buffer = vec![Complex { re: 0.0_f32, im: 0.0_f32 }; FFT_SIZE];
    let mut smoothed_bins = [0.0_f32; NUM_BINS];
    let mut scope_buffer = [0.0_f32; SCOPE_SAMPLES];

    while shared.running.load(Ordering::Relaxed) {
        match audio_rx.recv_timeout(Duration::from_millis(16)) {
            Ok(frame) => {
                for i in 0..frame.count {
                    let s = frame.samples[i];
                    ring_buffer[write_pos] = s;
                    write_pos = (write_pos + 1) % FFT_SIZE;
                    if i < SCOPE_SAMPLES {
                        scope_buffer[i] = s;
                    }
                }

                while let Ok(newer_frame) = audio_rx.try_recv() {
                    for i in 0..newer_frame.count {
                        let s = newer_frame.samples[i];
                        ring_buffer[write_pos] = s;
                        write_pos = (write_pos + 1) % FFT_SIZE;
                        if i < SCOPE_SAMPLES {
                            scope_buffer[i] = s;
                        }
                    }
                }
            }
            Err(_) => {
                for bin in &mut smoothed_bins {
                    *bin *= 0.82;
                }
                scope_buffer.fill(0.0);
            }
        }

        for (i, slot) in fft_buffer.iter_mut().enumerate() {
            let idx = (write_pos + i) % FFT_SIZE;
            slot.re = ring_buffer[idx] * hann_window[i];
            slot.im = 0.0;
        }

        fft.process(&mut fft_buffer);

        let sample_rate = shared.sample_rate.load(Ordering::Relaxed).max(44_100) as f32;
        let nyquist_bins = FFT_SIZE / 2;
        let min_freq = 45.0_f32;
        let max_freq = 12_000.0_f32;
        let log_ratio = (max_freq / min_freq).ln();

        let mut output_bins = [0_u64; NUM_BINS];
        let mut max_frame_db = -60.0_f32;

        for b in 0..NUM_BINS {
            let f_low = min_freq * (log_ratio * (b as f32 / NUM_BINS as f32)).exp();
            let f_high = min_freq * (log_ratio * ((b + 1) as f32 / NUM_BINS as f32)).exp();

            let idx_low = ((f_low * FFT_SIZE as f32 / sample_rate).floor() as usize)
                .clamp(1, nyquist_bins - 1);
            let idx_high = ((f_high * FFT_SIZE as f32 / sample_rate).ceil() as usize)
                .clamp(idx_low + 1, nyquist_bins);

            let mut peak_mag = 0.0_f32;
            for c in &fft_buffer[idx_low..idx_high] {
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

            if normalized > smoothed_bins[b] {
                smoothed_bins[b] = smoothed_bins[b] * 0.30 + normalized * 0.70;
            } else {
                smoothed_bins[b] = smoothed_bins[b] * 0.78 + normalized * 0.22;
            }

            output_bins[b] = smoothed_bins[b].round() as u64;
        }

        let snapshot = VisualizerSnapshot {
            spectrum_bins: output_bins,
            oscilloscope: scope_buffer,
            peak_db: max_frame_db.clamp(-60.0, 0.0),
        };

        let _ = snapshot_tx.try_send(snapshot);
    }
}
