use std::sync::atomic::Ordering;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{BarChart, Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::audio::stream::SharedSynthState;
use crate::dsp::envelope::AdsrParams;
use crate::visualizer::fft::{VisualizerSnapshot, NUM_BINS, SCOPE_SAMPLES};

const BIN_LABELS: [&str; NUM_BINS] = [
    "45", "55", "68", "83", "102", "125", "153", "188", "230", "282", "345", "423", "518", "635",
    "778", "953", "1.1k", "1.4k", "1.7k", "2.1k", "2.6k", "3.2k", "3.9k", "4.8k", "5.9k", "7.2k",
    "8.9k", "11k",
];

pub const PIANO_KEYS: [(char, u8, &str); 13] = [
    ('a', 60, "C4"),
    ('w', 61, "C#4"),
    ('s', 62, "D4"),
    ('e', 63, "D#4"),
    ('d', 64, "E4"),
    ('f', 65, "F4"),
    ('t', 66, "F#4"),
    ('g', 67, "G4"),
    ('y', 68, "G#4"),
    ('h', 69, "A4"),
    ('u', 70, "A#4"),
    ('j', 71, "B4"),
    ('k', 72, "C5"),
];

pub fn draw_ui(
    frame: &mut Frame,
    shared: &SharedSynthState,
    snapshot: &VisualizerSnapshot,
    active_notes: &[u8],
) {
    let root_chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(12),
            Constraint::Length(6),
        ])
        .split(frame.area());

    draw_hud(frame, root_chunks[0], shared, snapshot);

    let mid_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(root_chunks[1]);

    draw_spectrum(frame, mid_chunks[0], snapshot);
    draw_oscilloscope(frame, mid_chunks[1], snapshot);
    draw_keyboard_and_controls(frame, root_chunks[2], active_notes);
}

fn draw_hud(
    frame: &mut Frame,
    area: Rect,
    shared: &SharedSynthState,
    snapshot: &VisualizerSnapshot,
) {
    let hud_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(36),
            Constraint::Percentage(34),
            Constraint::Percentage(30),
        ])
        .split(area);

    let waveform = shared.waveform();
    let sample_rate = shared.sample_rate.load(Ordering::Relaxed);
    let buffer_size = shared.buffer_size.load(Ordering::Relaxed);
    let voices = shared.active_voices.load(Ordering::Relaxed);

    let engine_info = vec![
        Line::from(vec![
            Span::styled(" Oscillator: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("[{}]", waveform.name()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Voices: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}/8", voices),
                Style::default()
                    .fg(if voices > 6 { Color::Yellow } else { Color::Green })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Audio I/O:  ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{} Hz | {} smp buf", sample_rate, buffer_size),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    let engine_block = Paragraph::new(engine_info).block(
        Block::default()
            .title(" RUSTY-SYNTH DSP ENGINE ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(engine_block, hud_chunks[0]);

    let adsr = AdsrParams::default();
    let cutoff = shared.cutoff_hz();
    let gain = shared.master_gain();

    let filter_info = vec![
        Line::from(vec![
            Span::styled(" ADSR:   ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!(
                    "A:{:.0}ms D:{:.0}ms S:{:.0}% R:{:.0}ms",
                    adsr.attack_secs * 1000.0,
                    adsr.decay_secs * 1000.0,
                    adsr.sustain_level * 100.0,
                    adsr.release_secs * 1000.0
                ),
                Style::default().fg(Color::Magenta),
            ),
        ]),
        Line::from(vec![
            Span::styled(" Filter: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("LPF {:.0} Hz", cutoff),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Gain: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{:.0}%", gain * 100.0),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let filter_block = Paragraph::new(filter_info).block(
        Block::default()
            .title(" ENVELOPE & FILTER ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Magenta)),
    );
    frame.render_widget(filter_block, hud_chunks[1]);

    let meter_ratio = ((snapshot.peak_db + 60.0) / 60.0).clamp(0.0, 1.0) as f64;
    let meter_color = if snapshot.peak_db > -6.0 {
        Color::Red
    } else if snapshot.peak_db > -18.0 {
        Color::Yellow
    } else {
        Color::Green
    };

    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(" OUTPUT LEVEL (dBFS) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        )
        .gauge_style(Style::default().fg(meter_color).bg(Color::Black))
        .ratio(meter_ratio)
        .label(format!("{:.1} dB", snapshot.peak_db));
    frame.render_widget(gauge, hud_chunks[2]);
}

fn draw_spectrum(frame: &mut Frame, area: Rect, snapshot: &VisualizerSnapshot) {
    let bars: Vec<(&str, u64)> = BIN_LABELS
        .iter()
        .copied()
        .zip(snapshot.spectrum_bins.iter().copied())
        .collect();

    let barchart = BarChart::default()
        .block(
            Block::default()
                .title(" REAL-TIME FFT SPECTRUM ANALYZER (28 LOG BANDS, -60dB to 0dB) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .data(&bars)
        .bar_width(3)
        .bar_gap(1)
        .max(100)
        .bar_style(Style::default().fg(Color::Cyan))
        .value_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .label_style(Style::default().fg(Color::DarkGray));

    frame.render_widget(barchart, area);
}

fn draw_oscilloscope(frame: &mut Frame, area: Rect, snapshot: &VisualizerSnapshot) {
    let samples = snapshot.oscilloscope;
    let canvas = Canvas::default()
        .block(
            Block::default()
                .title(" WAVEFORM OSCILLOSCOPE ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .marker(symbols::Marker::Braille)
        .x_bounds([0.0, (SCOPE_SAMPLES - 1) as f64])
        .y_bounds([-1.1, 1.1])
        .paint(move |ctx| {
            ctx.draw(&CanvasLine {
                x1: 0.0,
                y1: 0.0,
                x2: (SCOPE_SAMPLES - 1) as f64,
                y2: 0.0,
                color: Color::DarkGray,
            });

            for i in 0..(SCOPE_SAMPLES - 1) {
                ctx.draw(&CanvasLine {
                    x1: i as f64,
                    y1: samples[i] as f64,
                    x2: (i + 1) as f64,
                    y2: samples[i + 1] as f64,
                    color: Color::Yellow,
                });
            }
        });

    frame.render_widget(canvas, area);
}

fn draw_keyboard_and_controls(frame: &mut Frame, area: Rect, active_notes: &[u8]) {
    let mut key_spans = Vec::with_capacity(PIANO_KEYS.len() * 2 + 2);
    key_spans.push(Span::styled(" Keys: ", Style::default().fg(Color::Gray)));

    for (key_char, midi_note, note_name) in PIANO_KEYS {
        let is_pressed = active_notes.contains(&midi_note);
        let style = if is_pressed {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else if note_name.contains('#') {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default().fg(Color::White)
        };

        key_spans.push(Span::styled(
            format!(" {}:{} ", key_char.to_ascii_uppercase(), note_name),
            style,
        ));
        key_spans.push(Span::raw(" "));
    }

    let controls_line_1 = Line::from(key_spans);
    let controls_line_2 = Line::from(vec![
        Span::styled(" Waveforms: ", Style::default().fg(Color::Gray)),
        Span::styled(
            "[1] Sine  [2] Square  [3] Sawtooth  [4] Triangle  [5] White Noise",
            Style::default().fg(Color::Cyan),
        ),
    ]);
    let controls_line_3 = Line::from(vec![
        Span::styled(" Controls:  ", Style::default().fg(Color::Gray)),
        Span::styled("[ / ]", Style::default().fg(Color::Yellow)),
        Span::raw(" Filter Cutoff   "),
        Span::styled("- / =", Style::default().fg(Color::Green)),
        Span::raw(" Master Gain   "),
        Span::styled("Q / Esc", Style::default().fg(Color::Red)),
        Span::raw(" Quit"),
    ]);

    let paragraph = Paragraph::new(vec![controls_line_1, controls_line_2, controls_line_3]).block(
        Block::default()
            .title(" LIVE KEYBOARD & SYNTH CONTROLS ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::White)),
    );

    frame.render_widget(paragraph, area);
}
