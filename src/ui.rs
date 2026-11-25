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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpectrumLayout {
    bar_width: u16,
    bar_gap: u16,
    num_bars: usize,
}

/// Computes bar geometry and visible band count so the spectrum fits inside `inner_width` without clipping.
fn compute_spectrum_layout(inner_width: u16) -> SpectrumLayout {
    let w = inner_width as usize;
    if w == 0 {
        return SpectrumLayout {
            bar_width: 1,
            bar_gap: 0,
            num_bars: 0,
        };
    }

    // Prefer 3-column bars (matching 3-char Hz labels and 0..100 values) down to narrow panes.
    let (bar_width, bar_gap) = if w >= 35 {
        (3, 1)
    } else if w >= 18 {
        (2, 1)
    } else if w >= 5 {
        (1, 1)
    } else {
        (1, 0)
    };

    let num_bars = ((w + bar_gap) / (bar_width + bar_gap)).clamp(1, NUM_BINS);
    SpectrumLayout {
        bar_width: bar_width as u16,
        bar_gap: bar_gap as u16,
        num_bars,
    }
}

/// Pools the 28 logarithmic FFT bins into `num_bars` bands spanning the full 45 Hz–11 kHz range.
fn aggregate_spectrum_bars(bins: &[u64; NUM_BINS], num_bars: usize) -> Vec<(&'static str, u64)> {
    if num_bars == 0 {
        return Vec::new();
    }
    if num_bars >= NUM_BINS {
        return BIN_LABELS
            .iter()
            .copied()
            .zip(bins.iter().copied())
            .collect();
    }

    let mut bars = Vec::with_capacity(num_bars);
    for k in 0..num_bars {
        let start = k * NUM_BINS / num_bars;
        let end = ((k + 1) * NUM_BINS / num_bars).max(start + 1).min(NUM_BINS);
        let peak = bins[start..end].iter().copied().max().unwrap_or(0);

        let label_idx = if k == 0 {
            0
        } else if k + 1 == num_bars {
            NUM_BINS - 1
        } else {
            (start + end - 1) / 2
        };
        bars.push((BIN_LABELS[label_idx], peak));
    }
    bars
}

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

    let engine_inner_width = hud_chunks[0].width.saturating_sub(2);
    let voice_style = Style::default()
        .fg(if voices > 6 {
            Color::Yellow
        } else {
            Color::Green
        })
        .add_modifier(Modifier::BOLD);

    let mut engine_info = if engine_inner_width < 38 {
        vec![
            Line::from(vec![
                Span::styled(" Osc: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("[{}]", waveform.name()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  V:", Style::default().fg(Color::Gray)),
                Span::styled(format!("{}/8", voices), voice_style),
            ]),
            Line::from(vec![
                Span::styled(" I/O: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{} Hz | {} smp", sample_rate, buffer_size),
                    Style::default().fg(Color::White),
                ),
            ]),
        ]
    } else {
        vec![
            Line::from(vec![
                Span::styled(" Oscillator: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("[{}]", waveform.name()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  Voices: ", Style::default().fg(Color::Gray)),
                Span::styled(format!("{}/8", voices), voice_style),
            ]),
            Line::from(vec![
                Span::styled(" Audio I/O:  ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{} Hz | {} smp buf", sample_rate, buffer_size),
                    Style::default().fg(Color::White),
                ),
            ]),
        ]
    };

    if let Some(err) = shared.audio_error() {
        let label = if engine_inner_width < 38 {
            " Err: "
        } else {
            " Audio Err:  "
        };
        engine_info.push(Line::from(vec![
            Span::styled(
                label,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err, Style::default().fg(Color::Red)),
        ]));
    }

    let engine_title = if engine_inner_width < 26 {
        " DSP ENGINE "
    } else {
        " RUSTY-SYNTH DSP ENGINE "
    };
    let engine_block = Paragraph::new(engine_info).block(
        Block::default()
            .title(engine_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(engine_block, hud_chunks[0]);

    let adsr = AdsrParams::default();
    let cutoff = shared.cutoff_hz();
    let gain = shared.master_gain();

    let filter_inner_width = hud_chunks[1].width.saturating_sub(2);
    let filter_info = if filter_inner_width < 38 {
        vec![
            Line::from(vec![
                Span::styled(" ADSR: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!(
                        "{:.0}/{:.0}/{:.0}%/{:.0}",
                        adsr.attack_secs * 1000.0,
                        adsr.decay_secs * 1000.0,
                        adsr.sustain_level * 100.0,
                        adsr.release_secs * 1000.0
                    ),
                    Style::default().fg(Color::Magenta),
                ),
            ]),
            Line::from(vec![
                Span::styled(" LPF:", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{:.0}Hz", cutoff),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Vol:", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{:.0}%", gain * 100.0),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ]
    } else {
        vec![
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
        ]
    };

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

    let gauge_title = if hud_chunks[2].width.saturating_sub(2) < 23 {
        " LEVEL (dBFS) "
    } else {
        " OUTPUT LEVEL (dBFS) "
    };
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(gauge_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        )
        .gauge_style(Style::default().fg(meter_color).bg(Color::Black))
        .ratio(meter_ratio)
        .label(format!("{:.1} dB", snapshot.peak_db));
    frame.render_widget(gauge, hud_chunks[2]);
}

fn draw_spectrum(frame: &mut Frame, area: Rect, snapshot: &VisualizerSnapshot) {
    let inner_width = area.width.saturating_sub(2);
    let layout = compute_spectrum_layout(inner_width);
    let bars = aggregate_spectrum_bars(&snapshot.spectrum_bins, layout.num_bars);

    let title = if inner_width < 64 {
        format!(" FFT SPECTRUM ({} BANDS, -60..0dB) ", bars.len())
    } else {
        format!(
            " REAL-TIME FFT SPECTRUM ANALYZER ({} LOG BANDS, -60dB to 0dB) ",
            bars.len()
        )
    };

    let barchart = BarChart::default()
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .data(&bars)
        .bar_width(layout.bar_width)
        .bar_gap(layout.bar_gap)
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
    let inner_width = area.width.saturating_sub(2);
    let compact = inner_width < 98;

    let mut key_spans = Vec::with_capacity(PIANO_KEYS.len() * 2 + 2);
    key_spans.push(Span::styled(
        if compact { "Keys:" } else { " Keys: " },
        Style::default().fg(Color::Gray),
    ));

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

        if compact {
            key_spans.push(Span::raw(" "));
            key_spans.push(Span::styled(
                format!("{}:{}", key_char.to_ascii_uppercase(), note_name),
                style,
            ));
        } else {
            key_spans.push(Span::styled(
                format!(" {}:{} ", key_char.to_ascii_uppercase(), note_name),
                style,
            ));
            key_spans.push(Span::raw(" "));
        }
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

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn spectrum_layout_fits_within_available_width_without_clipping() {
        for inner_width in 0..=150_u16 {
            let layout = compute_spectrum_layout(inner_width);
            if inner_width == 0 {
                assert_eq!(layout.num_bars, 0);
                continue;
            }
            assert!(layout.num_bars >= 1 && layout.num_bars <= NUM_BINS);
            let used = layout.num_bars * layout.bar_width as usize
                + layout.num_bars.saturating_sub(1) * layout.bar_gap as usize;
            assert!(
                used <= inner_width as usize,
                "layout {layout:?} used {used} > inner_width {inner_width}"
            );
        }

        // Standard 80-col terminal (~46 inner cols) and 120-col terminal (~71 inner cols)
        let l80 = compute_spectrum_layout(46);
        assert_eq!(l80.bar_width, 3);
        assert_eq!(l80.num_bars, 11);

        let l120 = compute_spectrum_layout(71);
        assert_eq!(l120.bar_width, 3);
        assert_eq!(l120.num_bars, 18);

        let l_wide = compute_spectrum_layout(120);
        assert_eq!(l_wide.bar_width, 3);
        assert_eq!(l_wide.num_bars, NUM_BINS);
    }

    #[test]
    fn aggregate_spectrum_bars_preserves_high_frequency_energy_when_downsampled() {
        let mut bins = [0_u64; NUM_BINS];
        bins[0] = 42;
        bins[NUM_BINS - 1] = 88; // 11 kHz bin

        let bars = aggregate_spectrum_bars(&bins, 11);
        assert_eq!(bars.len(), 11);
        assert_eq!(bars[0], ("45", 42));
        assert_eq!(bars[10], ("11k", 88));
    }

    #[test]
    fn draw_ui_renders_cleanly_at_80_and_120_columns_and_shows_errors() {
        let shared = SharedSynthState::new();
        shared.set_audio_error("ALSA xrun");
        let mut snapshot = VisualizerSnapshot::default();
        snapshot.spectrum_bins[0] = 65;
        snapshot.spectrum_bins[NUM_BINS - 1] = 90;
        snapshot.peak_db = -12.0;

        for (width, height) in [(80, 24), (100, 30), (120, 35), (190, 40), (20, 10)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| draw_ui(f, &shared, &snapshot, &[60, 64, 67]))
                .unwrap();

            let buf = terminal.backend().buffer();
            let content: String = buf.content().iter().map(|c| c.symbol()).collect();
            if width >= 80 {
                // Both ends of the spectrum (45 Hz and 11k) and audio error surface cleanly
                assert!(
                    content.contains("45"),
                    "missing 45Hz label at {width}x{height}"
                );
                assert!(
                    content.contains("11k"),
                    "missing 11k label at {width}x{height}"
                );
                assert!(
                    content.contains("ALSA"),
                    "missing error text at {width}x{height}"
                );
            }
        }
    }
}
