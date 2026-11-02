mod audio;
mod dsp;
mod ui;
mod visualizer;

use std::collections::HashMap;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::bounded;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::audio::stream::{start_audio_stream, AudioCommand, SharedSynthState};
use crate::dsp::oscillator::Waveform;
use crate::ui::{draw_ui, PIANO_KEYS};
use crate::visualizer::fft::{run_fft_thread, VisualizerSnapshot};

fn char_to_midi(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    PIANO_KEYS
        .iter()
        .find_map(|(k, midi, _)| if *k == lower { Some(*midi) } else { None })
}

fn main() -> Result<()> {
    let shared = Arc::new(SharedSynthState::new());
    let (cmd_tx, cmd_rx) = bounded::<AudioCommand>(128);
    let (viz_frame_tx, viz_frame_rx) = bounded(16);
    let (snapshot_tx, snapshot_rx) = bounded::<VisualizerSnapshot>(4);

    let _audio_stream = start_audio_stream(Arc::clone(&shared), cmd_rx, viz_frame_tx)?;

    let fft_shared = Arc::clone(&shared);
    let fft_handle = thread::Builder::new()
        .name("rusty-synth-fft".into())
        .spawn(move || {
            run_fft_thread(fft_shared, viz_frame_rx, snapshot_tx);
        })?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let supports_key_release = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    )
    .is_ok();

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app_result = run_app(
        &mut terminal,
        Arc::clone(&shared),
        cmd_tx,
        snapshot_rx,
        supports_key_release,
    );

    shared.running.store(false, Ordering::Relaxed);
    let _ = fft_handle.join();

    if supports_key_release {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    app_result
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    shared: Arc<SharedSynthState>,
    cmd_tx: crossbeam_channel::Sender<AudioCommand>,
    snapshot_rx: crossbeam_channel::Receiver<VisualizerSnapshot>,
    supports_key_release: bool,
) -> Result<()> {
    let tick_rate = Duration::from_millis(16);
    let auto_release_timeout = Duration::from_millis(320);

    let mut latest_snapshot = VisualizerSnapshot::default();
    let mut active_notes: HashMap<u8, Instant> = HashMap::with_capacity(16);

    while shared.running.load(Ordering::Relaxed) {
        while let Ok(snap) = snapshot_rx.try_recv() {
            latest_snapshot = snap;
        }

        while event::poll(Duration::from_millis(0))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                        shared.running.store(false, Ordering::Relaxed);
                        break;
                    }
                    KeyCode::Char('1') if key.kind != KeyEventKind::Release => {
                        shared.set_waveform(Waveform::Sine);
                    }
                    KeyCode::Char('2') if key.kind != KeyEventKind::Release => {
                        shared.set_waveform(Waveform::Square);
                    }
                    KeyCode::Char('3') if key.kind != KeyEventKind::Release => {
                        shared.set_waveform(Waveform::Sawtooth);
                    }
                    KeyCode::Char('4') if key.kind != KeyEventKind::Release => {
                        shared.set_waveform(Waveform::Triangle);
                    }
                    KeyCode::Char('5') if key.kind != KeyEventKind::Release => {
                        shared.set_waveform(Waveform::WhiteNoise);
                    }
                    KeyCode::Char('[') if key.kind != KeyEventKind::Release => {
                        let current = shared.cutoff_hz();
                        shared.set_cutoff_hz((current * 0.85).max(120.0));
                    }
                    KeyCode::Char(']') if key.kind != KeyEventKind::Release => {
                        let current = shared.cutoff_hz();
                        shared.set_cutoff_hz((current * 1.18).min(18_000.0));
                    }
                    KeyCode::Char('-') if key.kind != KeyEventKind::Release => {
                        let current = shared.master_gain();
                        shared.set_master_gain(current - 0.05);
                    }
                    KeyCode::Char('=') | KeyCode::Char('+')
                        if key.kind != KeyEventKind::Release =>
                    {
                        let current = shared.master_gain();
                        shared.set_master_gain(current + 0.05);
                    }
                    KeyCode::Char(c) => {
                        if let Some(midi_note) = char_to_midi(c) {
                            match key.kind {
                                KeyEventKind::Press => {
                                    if !active_notes.contains_key(&midi_note) {
                                        let _ = cmd_tx.try_send(AudioCommand::NoteOn(midi_note));
                                    }
                                    active_notes.insert(midi_note, Instant::now());
                                }
                                KeyEventKind::Repeat => {
                                    active_notes.insert(midi_note, Instant::now());
                                }
                                KeyEventKind::Release => {
                                    if active_notes.remove(&midi_note).is_some() {
                                        let _ = cmd_tx.try_send(AudioCommand::NoteOff(midi_note));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if !supports_key_release {
            let now = Instant::now();
            active_notes.retain(|&note, &mut last_seen| {
                if now.duration_since(last_seen) > auto_release_timeout {
                    let _ = cmd_tx.try_send(AudioCommand::NoteOff(note));
                    false
                } else {
                    true
                }
            });
        }

        let held_list: Vec<u8> = active_notes.keys().copied().collect();
        terminal.draw(|f| draw_ui(f, &shared, &latest_snapshot, &held_list))?;

        thread::sleep(tick_rate);
    }

    Ok(())
}
