mod audio;
mod dsp;
mod ui;
mod visualizer;

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::bounded;
use crossterm::cursor::Show;
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

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Restores terminal state cleanly if raw mode / alternate screen were entered.
fn restore_terminal() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture, Show);
    }
}

/// Installs a panic hook that exits raw mode and alternate screen before printing panic info.
fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        restore_terminal();
        original_hook(panic_info);
    }));
}

/// RAII guard owning the terminal session and guaranteeing restoration on drop.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);

        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            restore_terminal();
            return Err(err.into());
        }
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
        );

        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(err) => {
                restore_terminal();
                Err(err.into())
            }
        }
    }

    fn restore(&mut self) -> Result<()> {
        if !TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        let raw_res = disable_raw_mode();
        let screen_res = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let cursor_res = self.terminal.show_cursor();
        raw_res?;
        screen_res?;
        cursor_res?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if TERMINAL_ACTIVE.load(Ordering::SeqCst) {
            let _ = self.restore();
        }
    }
}

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

    install_panic_hook();
    let mut guard = TerminalGuard::enter()?;

    let app_result = run_app(
        &mut guard.terminal,
        Arc::clone(&shared),
        cmd_tx,
        snapshot_rx,
    );

    shared.running.store(false, Ordering::Relaxed);
    let _ = fft_handle.join();

    let restore_result = guard.restore();
    if let Some(err) = shared.audio_error() {
        eprintln!("Audio stream error: {err}");
    }
    app_result?;
    restore_result
}

/// Tracks each active note's last event time and whether it has received a repeat event.
struct ActiveKey {
    last_seen: Instant,
    is_held: bool,
}

fn handle_piano_key(
    midi_note: u8,
    kind: KeyEventKind,
    now: Instant,
    active_notes: &mut HashMap<u8, ActiveKey>,
    cmd_tx: &crossbeam_channel::Sender<AudioCommand>,
) {
    match kind {
        KeyEventKind::Press | KeyEventKind::Repeat => {
            if let Some(existing) = active_notes.get_mut(&midi_note) {
                // Standard terminals emit Press for repeats; enhanced terminals emit Repeat.
                // Either way, refresh the watchdog timer without re-triggering NoteOn.
                existing.last_seen = now;
                existing.is_held = true;
            } else if cmd_tx.try_send(AudioCommand::NoteOn(midi_note)).is_ok() {
                active_notes.insert(
                    midi_note,
                    ActiveKey {
                        last_seen: now,
                        is_held: kind == KeyEventKind::Repeat,
                    },
                );
            }
        }
        KeyEventKind::Release => {
            if active_notes.remove(&midi_note).is_some() {
                let _ = cmd_tx.try_send(AudioCommand::NoteOff(midi_note));
            }
        }
    }
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    shared: Arc<SharedSynthState>,
    cmd_tx: crossbeam_channel::Sender<AudioCommand>,
    snapshot_rx: crossbeam_channel::Receiver<VisualizerSnapshot>,
) -> Result<()> {
    let tick_rate = Duration::from_millis(16);
    // Initial tap gate duration (must exceed OS initial key-repeat delay of ~250-300ms
    // if held, or release cleanly after a single quick tap)
    let tap_gate_duration = Duration::from_millis(320);
    // Once a key is actively repeating (held down), release rapidly (~90ms) after repeats stop
    let hold_release_duration = Duration::from_millis(90);

    let mut latest_snapshot = VisualizerSnapshot::default();
    let mut active_notes: HashMap<u8, ActiveKey> = HashMap::with_capacity(16);

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
                            handle_piano_key(
                                midi_note,
                                key.kind,
                                Instant::now(),
                                &mut active_notes,
                                &cmd_tx,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }

        // Always run the release watchdog so notes never get stuck on any terminal
        let now = Instant::now();
        active_notes.retain(|note, state| {
            let timeout = if state.is_held {
                hold_release_duration
            } else {
                tap_gate_duration
            };

            if now.duration_since(state.last_seen) > timeout {
                let _ = cmd_tx.try_send(AudioCommand::NoteOff(*note));
                false
            } else {
                true
            }
        });

        let held_list: Vec<u8> = active_notes.keys().copied().collect();
        terminal.draw(|f| draw_ui(f, &shared, &latest_snapshot, &held_list))?;

        thread::sleep(tick_rate);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_to_midi_maps_octave_c4_to_c5() {
        assert_eq!(char_to_midi('a'), Some(60));
        assert_eq!(char_to_midi('A'), Some(60));
        assert_eq!(char_to_midi('w'), Some(61));
        assert_eq!(char_to_midi('k'), Some(72));
        assert_eq!(char_to_midi('q'), None);
    }

    #[test]
    fn repeat_on_inactive_key_sends_note_on_before_highlighting() {
        let (cmd_tx, cmd_rx) = bounded(16);
        let mut active_notes = HashMap::new();
        let t0 = Instant::now();

        // Repeat when key is NOT active must send NoteOn (never highlight a silent key)
        handle_piano_key(60, KeyEventKind::Repeat, t0, &mut active_notes, &cmd_tx);
        assert!(matches!(cmd_rx.try_recv(), Ok(AudioCommand::NoteOn(60))));
        assert!(active_notes.get(&60).is_some_and(|k| k.is_held));

        // Repeat when key IS already active updates state without re-triggering NoteOn
        let t1 = t0 + Duration::from_millis(30);
        handle_piano_key(60, KeyEventKind::Repeat, t1, &mut active_notes, &cmd_tx);
        assert!(cmd_rx.try_recv().is_err());
        assert_eq!(active_notes[&60].last_seen, t1);

        // Release clears active_notes and emits NoteOff
        handle_piano_key(60, KeyEventKind::Release, t1, &mut active_notes, &cmd_tx);
        assert!(!active_notes.contains_key(&60));
        assert!(matches!(cmd_rx.try_recv(), Ok(AudioCommand::NoteOff(60))));
    }
}
