//! Interactive TUI (`xng tui`): live message browser with detail pane,
//! per-channel statistics, spectrum with channel markers, and waterfall.

use crate::bus::MessageBus;
use crate::outputs::console::{format_message, ConsoleFormat};
use crate::runtime::{self, LiveState, SessionConfig};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use xng_sdr::IqSource;
use xng_types::{Message, StationIdentity};

const MAX_MESSAGES: usize = 2000;
const WATERFALL_ROWS: usize = 120;

struct App {
    messages: VecDeque<Message>,
    selected: Option<usize>,
    follow: bool,
    /// Detail pane shows a human-readable summary; 'v' toggles raw JSON.
    detail_json: bool,
    waterfall: VecDeque<Vec<f32>>,
    started: Instant,
    session_line: String,
    channels_hz: Vec<u64>,
}

pub fn run(mut source: Box<dyn IqSource>, cfg: SessionConfig) -> anyhow::Result<()> {
    let sample_rate = source.sample_rate();
    let capture_center = if cfg.center_hz > 0 { cfg.center_hz } else { source.center_freq_hz() };
    let decoders = runtime::build_decoders(sample_rate, capture_center, &cfg)?;

    let bus = MessageBus::new();
    let mut rx = bus.subscribe();
    let live = LiveState::new();
    let stop = Arc::new(AtomicBool::new(false));
    let station = StationIdentity::new(cfg.station_ident.clone());

    let decode_thread = std::thread::spawn({
        let bus = bus.clone();
        let stop = stop.clone();
        let live = live.clone();
        let sdr = cfg.sdr.clone();
        move || {
            let mut reasm = xng_acars::reasm::Reassembler::new(300.0);
            runtime::decode_loop(
                &mut *source,
                decoders,
                station,
                sdr,
                bus,
                stop,
                Some((live, capture_center, sample_rate)),
                Some(&mut reasm),
            )
        }
    });

    let mut app = App {
        messages: VecDeque::new(),
        selected: None,
        follow: true,
        detail_json: false,
        waterfall: VecDeque::new(),
        started: Instant::now(),
        session_line: format!(
            "{} | {:.3} MHz @ {:.0} kS/s | {} channel(s)",
            cfg.mode,
            capture_center as f64 / 1e6,
            sample_rate / 1e3,
            cfg.channels_hz.len()
        ),
        channels_hz: cfg.channels_hz.clone(),
    };

    let mut terminal = ratatui::init();
    let result = (|| -> anyhow::Result<()> {
        loop {
            // Drain new messages.
            while let Ok(m) = rx.try_recv() {
                app.messages.push_back((*m).clone());
                if app.messages.len() > MAX_MESSAGES {
                    app.messages.pop_front();
                    if let Some(s) = &mut app.selected {
                        *s = s.saturating_sub(1);
                    }
                }
            }
            if app.follow && !app.messages.is_empty() {
                app.selected = Some(app.messages.len() - 1);
            }
            // Latest spectrum into the waterfall history.
            if let Some(frame) = live.spectrum.lock().unwrap().take() {
                app.waterfall.push_front(frame.bins_db);
                if app.waterfall.len() > WATERFALL_ROWS {
                    app.waterfall.pop_back();
                }
            }

            let stats = live.stats.lock().unwrap().clone();
            let samples = live.samples.load(Ordering::Relaxed);
            terminal.draw(|f| draw(f, &mut app, &stats, samples, sample_rate, capture_center))?;

            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.follow = false;
                            app.selected =
                                Some(app.selected.unwrap_or(0).saturating_sub(1));
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.follow = false;
                            let last = app.messages.len().saturating_sub(1);
                            app.selected =
                                Some((app.selected.unwrap_or(0) + 1).min(last));
                        }
                        KeyCode::PageUp => {
                            app.follow = false;
                            app.selected =
                                Some(app.selected.unwrap_or(0).saturating_sub(20));
                        }
                        KeyCode::PageDown => {
                            let last = app.messages.len().saturating_sub(1);
                            app.selected =
                                Some((app.selected.unwrap_or(0) + 20).min(last));
                        }
                        KeyCode::End | KeyCode::Char('G') | KeyCode::Char('f') => {
                            app.follow = true;
                        }
                        KeyCode::Char('v') => {
                            app.detail_json = !app.detail_json;
                        }
                        KeyCode::Char('c') => {
                            app.messages.clear();
                            app.selected = None;
                        }
                        _ => {}
                    }
                }
            }
            if decode_thread.is_finished() && app.messages.is_empty() {
                // Source ended with nothing decoded; keep UI up anyway.
            }
        }
        Ok(())
    })();
    ratatui::restore();
    stop.store(true, Ordering::Relaxed);
    let _ = decode_thread.join();
    result
}

fn draw(
    f: &mut Frame,
    app: &mut App,
    stats: &[(u64, u64, u64, f32)],
    samples: u64,
    sample_rate: f64,
    center_hz: u64,
) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(14),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Header.
    let elapsed = app.started.elapsed().as_secs();
    let header = Line::from(vec![
        Span::styled(" xng ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            " {} | {}:{:02}:{:02} | {} msgs | {:.1} MS captured",
            app.session_line,
            elapsed / 3600,
            (elapsed / 60) % 60,
            elapsed % 60,
            app.messages.len(),
            samples as f64 / 1e6
        )),
    ]);
    f.render_widget(Paragraph::new(header), outer[0]);

    // Main: messages + detail.
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(outer[1]);

    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|m| {
            let line = format_message(m, ConsoleFormat::Pretty);
            let style = if m.decode.crc_ok {
                Style::default()
            } else {
                Style::default().fg(Color::Red)
            };
            ListItem::new(Line::styled(line, style))
        })
        .collect();
    let mut list_state = ListState::default();
    list_state.select(app.selected);
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " xng v{} — messages {} ",
            env!("CARGO_PKG_VERSION"),
            if app.follow { "(follow)" } else { "(paused)" }
        )))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, main[0], &mut list_state);

    let detail = app
        .selected
        .and_then(|i| app.messages.get(i))
        .map(|m| {
            if app.detail_json {
                serde_json::to_string_pretty(m).unwrap_or_default()
            } else {
                detail_pretty(m)
            }
        })
        .unwrap_or_else(|| "no message selected".into());
    let title = if app.detail_json { " detail (json — v: pretty) " } else { " detail (v: json) " };
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title)),
        main[1],
    );

    // Spectrum/waterfall + stats.
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(outer[2]);
    draw_spectrum(f, app, bottom[0], sample_rate, center_hz);
    draw_stats(f, stats, bottom[1]);

    f.render_widget(
        Paragraph::new(" q quit | j/k scroll | f follow | v detail view | c clear ")
            .style(Style::default().fg(Color::DarkGray)),
        outer[3],
    );
}

fn db_color(norm: f32) -> Color {
    let n = norm.clamp(0.0, 1.0);
    let (r, g, b) = if n < 0.25 {
        (0, 0, (n * 4.0 * 180.0) as u8)
    } else if n < 0.5 {
        (0, ((n - 0.25) * 4.0 * 200.0) as u8, 180)
    } else if n < 0.75 {
        (((n - 0.5) * 4.0 * 255.0) as u8, 200, (180.0 * (1.0 - (n - 0.5) * 4.0)) as u8)
    } else {
        (255, (200.0 * (1.0 - (n - 0.75) * 4.0)) as u8, 0)
    };
    Color::Rgb(r, g, b)
}

fn resample_max(bins: &[f32], width: usize) -> Vec<f32> {
    (0..width)
        .map(|c| {
            let a = c * bins.len() / width;
            let b = ((c + 1) * bins.len() / width).max(a + 1);
            bins[a..b.min(bins.len())].iter().cloned().fold(f32::MIN, f32::max)
        })
        .collect()
}

fn draw_spectrum(f: &mut Frame, app: &App, area: Rect, sample_rate: f64, center_hz: u64) {
    let block = Block::default().borders(Borders::ALL).title(format!(
        " spectrum/waterfall ({:.3}-{:.3} MHz) ",
        (center_hz as f64 - sample_rate / 2.0) / 1e6,
        (center_hz as f64 + sample_rate / 2.0) / 1e6
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 3 {
        return;
    }
    let width = inner.width as usize;

    // Channel markers.
    let mut marker = vec![' '; width];
    for &ch in &app.channels_hz {
        let frac = (ch as f64 - center_hz as f64) / sample_rate + 0.5;
        if (0.0..1.0).contains(&frac) {
            marker[(frac * width as f64) as usize] = '▾';
        }
    }
    f.render_widget(
        Paragraph::new(Line::styled(
            marker.into_iter().collect::<String>(),
            Style::default().fg(Color::Yellow),
        )),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );

    // Waterfall: each text row shows two history rows via half blocks.
    let (min_db, max_db) = (-95.0f32, -20.0f32);
    let rows = (inner.height - 1) as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    for r in 0..rows {
        let top = app.waterfall.get(r * 2).map(|b| resample_max(b, width));
        let bot = app.waterfall.get(r * 2 + 1).map(|b| resample_max(b, width));
        let spans: Vec<Span> = (0..width)
            .map(|c| {
                let tn = top
                    .as_ref()
                    .map(|v| (v[c] - min_db) / (max_db - min_db))
                    .unwrap_or(0.0);
                let bn = bot
                    .as_ref()
                    .map(|v| (v[c] - min_db) / (max_db - min_db))
                    .unwrap_or(0.0);
                Span::styled(
                    "▀",
                    Style::default().fg(db_color(tn)).bg(db_color(bn)),
                )
            })
            .collect();
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines),
        Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: inner.height - 1 },
    );
}

fn draw_stats(f: &mut Frame, stats: &[(u64, u64, u64, f32)], area: Rect) {
    let mut lines = vec![Line::from(Span::styled(
        format!("{:<11} {:>7} {:>7} {:>7}", "freq MHz", "frames", "ok", "dBFS"),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    for (freq, frames, ok, level) in stats {
        lines.push(Line::from(format!(
            "{:<11.3} {:>7} {:>7} {:>7.1}",
            *freq as f64 / 1e6,
            frames,
            ok,
            level
        )));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" channels ")),
        area,
    );
}

/// Human-readable detail pane: the message's vitals, the same summary
/// line the console prints, and the body's fields walked out as
/// indented `key: value` lines (raw JSON is one `v` press away).
fn detail_pretty(m: &Message) -> String {
    use xng_types::MessageBody;
    let mut out = String::new();
    out.push_str(&format!(
        "{}  {}  {:.3} MHz\n",
        m.timestamp.format("%H:%M:%S%.3f"),
        m.mode,
        m.frequency_hz as f64 / 1e6
    ));
    if let Some(rssi) = m.signal.rssi_db {
        out.push_str(&format!("signal   {rssi:.1} dBFS\n"));
    }
    let fec = m.decode.fec_corrected.unwrap_or(0);
    out.push_str(&format!(
        "decode   CRC {}{}\n",
        if m.decode.crc_ok { "ok" } else { "FAILED" },
        if fec > 0 { format!(" · {fec} FEC-corrected") } else { String::new() }
    ));
    out.push('\n');
    out.push_str(&format_message(m, ConsoleFormat::Pretty));
    out.push('\n');

    match &m.body {
        MessageBody::Acars(a) => {
            if !a.text.is_empty() {
                out.push('\n');
                out.push_str(&a.text);
                out.push('\n');
            }
        }
        MessageBody::Vdl2 { details, .. }
        | MessageBody::Hfdl { details, .. }
        | MessageBody::Iridium { details, .. }
        | MessageBody::StdC { details, .. } => {
            out.push('\n');
            walk_json(details, 0, &mut out);
        }
        _ => {}
    }

    if let Some(raw) = &m.raw {
        let shown = &raw[..raw.len().min(48)];
        out.push_str(&format!(
            "\nraw      {}{} ({} bytes)\n",
            shown.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            if raw.len() > shown.len() { "…" } else { "" },
            raw.len()
        ));
    }
    out
}

fn walk_json(v: &serde_json::Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                match val {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push_str(&format!("{pad}{k}:\n"));
                        walk_json(val, indent + 1, out);
                    }
                    _ => out.push_str(&format!("{pad}{k}: {}\n", scalar(val))),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                match item {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push_str(&format!("{pad}-\n"));
                        walk_json(item, indent + 1, out);
                    }
                    _ => out.push_str(&format!("{pad}- {}\n", scalar(item))),
                }
            }
        }
        _ => out.push_str(&format!("{pad}{}\n", scalar(v))),
    }
}

fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
