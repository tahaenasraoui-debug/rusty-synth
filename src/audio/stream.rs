use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig, SupportedBufferSize};
use crossbeam_channel::{Receiver, Sender};

use crate::dsp::oscillator::Waveform;
use crate::dsp::voice::VoiceManager;

#[derive(Debug, Clone, Copy)]
pub enum AudioCommand {
    NoteOn(u8),
    NoteOff(u8),
}

pub const AUDIO_FRAME_SAMPLES: usize = 256;

#[derive(Clone, Copy)]
pub struct AudioFrame {
    pub samples: [f32; AUDIO_FRAME_SAMPLES],
    pub count: usize,
}

impl Default for AudioFrame {
    fn default() -> Self {
        Self {
            samples: [0.0; AUDIO_FRAME_SAMPLES],
            count: 0,
        }
    }
}

pub struct SharedSynthState {
    pub waveform: AtomicU8,
    pub master_gain_bits: AtomicU32,
    pub cutoff_hz_bits: AtomicU32,
    pub active_voices: AtomicU8,
    pub sample_rate: AtomicU32,
    pub buffer_size: AtomicU32,
    pub running: AtomicBool,
}

impl SharedSynthState {
    pub fn new() -> Self {
        Self {
            waveform: AtomicU8::new(Waveform::Sawtooth as u8),
            master_gain_bits: AtomicU32::new(0.75_f32.to_bits()),
            cutoff_hz_bits: AtomicU32::new(4500.0_f32.to_bits()),
            active_voices: AtomicU8::new(0),
            sample_rate: AtomicU32::new(48_000),
            buffer_size: AtomicU32::new(256),
            running: AtomicBool::new(true),
        }
    }

    #[inline]
    pub fn master_gain(&self) -> f32 {
        f32::from_bits(self.master_gain_bits.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn set_master_gain(&self, gain: f32) {
        let clamped = gain.clamp(0.05, 1.5);
        self.master_gain_bits
            .store(clamped.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn cutoff_hz(&self) -> f32 {
        f32::from_bits(self.cutoff_hz_bits.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn set_cutoff_hz(&self, cutoff: f32) {
        let clamped = cutoff.clamp(120.0, 18_000.0);
        self.cutoff_hz_bits
            .store(clamped.to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn waveform(&self) -> Waveform {
        Waveform::from_u8(self.waveform.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn set_waveform(&self, wf: Waveform) {
        self.waveform.store(wf as u8, Ordering::Relaxed);
    }
}

pub fn start_audio_stream(
    shared: Arc<SharedSynthState>,
    cmd_rx: Receiver<AudioCommand>,
    viz_tx: Sender<AudioFrame>,
) -> Result<Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("No default audio output device detected on system"))?;

    let supported_configs = device
        .supported_output_configs()
        .context("Failed to query output stream configurations")?;

    let mut selected_config = None;
    for range in supported_configs {
        if range.sample_format() == SampleFormat::F32 {
            let min_sr = range.min_sample_rate().0;
            let max_sr = range.max_sample_rate().0;
            if (min_sr..=max_sr).contains(&48_000) {
                selected_config = Some(range.with_sample_rate(SampleRate(48_000)));
                break;
            } else if (min_sr..=max_sr).contains(&44_100) {
                selected_config = Some(range.with_sample_rate(SampleRate(44_100)));
                break;
            }
        }
    }

    let config_to_use = match selected_config {
        Some(cfg) => cfg,
        None => device
            .default_output_config()
            .context("Failed to obtain default output configuration")?,
    };

    let sample_format = config_to_use.sample_format();
    let mut stream_config: StreamConfig = config_to_use.clone().into();

    if let SupportedBufferSize::Range { min, max } = config_to_use.buffer_size() {
        let target = 256_u32.clamp(*min, *max);
        stream_config.buffer_size = cpal::BufferSize::Fixed(target);
        shared.buffer_size.store(target, Ordering::Relaxed);
    } else {
        shared.buffer_size.store(512, Ordering::Relaxed);
    }

    let sample_rate = stream_config.sample_rate.0;
    shared.sample_rate.store(sample_rate, Ordering::Relaxed);

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            &stream_config,
            shared,
            cmd_rx,
            viz_tx,
            |s| s,
        )?,
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            &stream_config,
            shared,
            cmd_rx,
            viz_tx,
            |s| (s * i16::MAX as f32) as i16,
        )?,
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            &stream_config,
            shared,
            cmd_rx,
            viz_tx,
            |s| ((s * 0.5 + 0.5) * u16::MAX as f32) as u16,
        )?,
        _ => return Err(anyhow!("Unsupported sample format")),
    };

    stream.play().context("Failed to start cpal audio stream")?;
    Ok(stream)
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<SharedSynthState>,
    cmd_rx: Receiver<AudioCommand>,
    viz_tx: Sender<AudioFrame>,
    sample_converter: fn(f32) -> T,
) -> Result<Stream>
where
    T: cpal::Sample + cpal::SizedSample + Send + 'static,
{
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;

    let mut voice_manager = VoiceManager::new(sample_rate);
    let mut out_frame = AudioFrame::default();

    let err_fn = move |_err| {};

    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    AudioCommand::NoteOn(note) => voice_manager.note_on(note),
                    AudioCommand::NoteOff(note) => voice_manager.note_off(note),
                }
            }

            let wf = shared.waveform();
            let gain = shared.master_gain();
            let cutoff = shared.cutoff_hz();
            voice_manager.set_waveform(wf);

            let mut max_active = 0_u8;

            for frame in data.chunks_mut(channels.max(1)) {
                let (sample, active_count) = voice_manager.next_sample(cutoff, gain);
                if active_count > max_active {
                    max_active = active_count;
                }

                let converted = sample_converter(sample);
                for ch in frame.iter_mut() {
                    *ch = converted;
                }

                if out_frame.count < AUDIO_FRAME_SAMPLES {
                    out_frame.samples[out_frame.count] = sample;
                    out_frame.count += 1;
                }

                if out_frame.count == AUDIO_FRAME_SAMPLES {
                    let _ = viz_tx.try_send(out_frame);
                    out_frame.count = 0;
                }
            }

            shared.active_voices.store(max_active, Ordering::Relaxed);
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}
