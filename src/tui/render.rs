//! All ratatui rendering — one `draw()` entry point, state-dispatched helpers.

use std::time::Instant;

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
    Frame,
};

use crate::core::manifest::{AudioTrack, SessionManifest, VodManifest};
use crate::disc::ProbeResult;
use crate::tui::{App, TuiState};

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// ── Styles ────────────────────────────────────────────────────────────────────

fn style_title() -> Style {
    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
}
fn style_label() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn style_value() -> Style {
    Style::default().fg(Color::White)
}
fn style_dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn style_accent() -> Style {
    Style::default().fg(Color::Cyan)
}
fn style_good() -> Style {
    Style::default().fg(Color::Green)
}
fn style_error() -> Style {
    Style::default().fg(Color::Red)
}
fn style_key() -> Style {
    Style::default().fg(Color::Yellow)
}
fn style_border() -> Style {
    Style::default().fg(Color::DarkGray)
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.size();

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(0),    // content
            Constraint::Length(1), // help bar
        ])
        .split(area);

    draw_header(f, layout[0]);

    match &app.state {
        TuiState::Idle => draw_idle(f, layout[1], app),
        TuiState::Probing { source_name } => draw_probing(f, layout[1], source_name, app.tick),
        TuiState::ScanningKeyframes {
            source_name,
            probe,
            keyframes_found,
            fraction,
            started_at,
        } => draw_scanning(f, layout[1], source_name, probe, *keyframes_found, *fraction, started_at),
        TuiState::ManifestReady { source_name, manifest, probe } => {
            draw_manifest_ready(f, layout[1], source_name, manifest, probe)
        }
        TuiState::Error { message } => draw_error(f, layout[1], message),
    }

    draw_help(f, layout[2], app);
}

// ── Header ────────────────────────────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: ratatui::layout::Rect) {
    let line = Line::from(vec![
        Span::styled(" watch-party ", style_title()),
        Span::styled("─── HOST ", style_dim()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ── Idle ──────────────────────────────────────────────────────────────────────

fn draw_idle(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let block = Block::default()
        .title(Span::styled(" No Disc Loaded ", style_title()))
        .borders(Borders::ALL)
        .border_style(style_border());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = if app.source_path.is_none() {
        vec![
            Line::from(""),
            Line::from(Span::styled("  No disc path provided.", style_dim())),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Run with a path:  ", style_dim()),
                Span::styled("watch-party D:\\", style_accent()),
                Span::styled("   or   ", style_dim()),
                Span::styled("watch-party /dev/sr0", style_accent()),
            ]),
            Line::from(vec![
                Span::styled("                    ", style_dim()),
                Span::styled("watch-party /path/to/movie.iso", style_accent()),
            ]),
        ]
    } else {
        let path = app.source_path.as_deref().and_then(|p| p.to_str()).unwrap_or("");
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Source:  ", style_label()),
                Span::styled(path, style_value()),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Press ", style_dim()),
                Span::styled("a", style_key()),
                Span::styled(" to begin analysis.", style_dim()),
            ]),
        ]
    };

    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

// ── Probing ───────────────────────────────────────────────────────────────────

fn draw_probing(f: &mut Frame, area: ratatui::layout::Rect, source_name: &str, tick: usize) {
    let title = format!(" Analyzing: {} ", source_name);
    let block = Block::default()
        .title(Span::styled(title, style_title()))
        .borders(Borders::ALL)
        .border_style(style_border());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let spinner = SPINNER[tick / 3 % SPINNER.len()];
    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {} ", spinner), style_accent()),
            Span::styled("Probing media with ffprobe…", style_dim()),
        ]),
    ];
    f.render_widget(Paragraph::new(text), inner);
}

// ── Scanning keyframes ────────────────────────────────────────────────────────

fn draw_scanning(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    source_name: &str,
    probe: &ProbeResult,
    keyframes_found: usize,
    fraction: f32,
    started_at: &Instant,
) {
    let title = format!(" Analyzing: {} ", source_name);
    let block = Block::default()
        .title(Span::styled(title, style_title()))
        .borders(Borders::ALL)
        .border_style(style_border());

    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // media summary
            Constraint::Length(1), // spacer
            Constraint::Length(1), // "Scanning keyframes..."
            Constraint::Length(1), // gauge
            Constraint::Length(1), // spacer
            Constraint::Length(1), // elapsed + count
            Constraint::Min(0),
        ])
        .split(inner);

    // Media summary line
    let summary = Line::from(vec![
        Span::styled("  ", style_dim()),
        Span::styled(&probe.video.codec, style_value()),
        Span::styled("  ", style_dim()),
        Span::styled(
            format!("{}×{}", probe.video.width, probe.video.height),
            style_value(),
        ),
        Span::styled("  ", style_dim()),
        Span::styled(format!("{:.3}fps", probe.video.framerate), style_value()),
        if let Some(dur) = probe.duration_secs {
            let s = format!("  Duration: {}", fmt_duration(dur));
            Span::styled(s, style_value())
        } else {
            Span::styled("  Duration: unknown", style_dim())
        },
    ]);
    f.render_widget(Paragraph::new(summary), rows[0]);

    // "Scanning keyframes..."
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("  Scanning keyframes…", style_dim()))),
        rows[2],
    );

    // Progress gauge
    let pct = (fraction * 100.0) as u16;
    let label = format!("{:>3}%  {:>6} keyframes", pct, fmt_count(keyframes_found));
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
        .ratio(fraction as f64)
        .label(label);
    // Inset the gauge slightly so it doesn't touch the block border
    let gauge_area = ratatui::layout::Rect {
        x: rows[3].x + 2,
        width: rows[3].width.saturating_sub(4),
        ..rows[3]
    };
    f.render_widget(gauge, gauge_area);

    // Elapsed time
    let elapsed = started_at.elapsed().as_secs();
    let elapsed_str = format!("  Elapsed: {}:{:02}", elapsed / 60, elapsed % 60);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(elapsed_str, style_dim()))),
        rows[5],
    );
}

// ── Manifest ready ────────────────────────────────────────────────────────────

fn draw_manifest_ready(
    f: &mut Frame,
    area: ratatui::layout::Rect,
    source_name: &str,
    manifest: &SessionManifest,
    probe: &ProbeResult,
) {
    let title = format!(" Ready: {} ", source_name);
    let block = Block::default()
        .title(Span::styled(title, style_good()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // media info
            Constraint::Length(1), // divider space
            Constraint::Min(0),    // peers placeholder
        ])
        .split(inner);

    // Media info block
    let media_block = Block::default()
        .title(Span::styled(" Media ", style_dim()))
        .borders(Borders::ALL)
        .border_style(style_border());
    let media_inner = media_block.inner(sections[0]);
    f.render_widget(media_block, sections[0]);

    let media_lines = build_media_lines(manifest, probe);
    f.render_widget(
        Paragraph::new(media_lines).wrap(Wrap { trim: false }),
        media_inner,
    );

    // Peers placeholder
    let peers_block = Block::default()
        .title(Span::styled(" Peers ", style_dim()))
        .borders(Borders::ALL)
        .border_style(style_border());
    let peers_inner = peers_block.inner(sections[2]);
    f.render_widget(peers_block, sections[2]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  No peers connected.",
            style_dim(),
        ))),
        peers_inner,
    );
}

fn build_media_lines<'a>(manifest: &'a SessionManifest, probe: &'a ProbeResult) -> Vec<Line<'a>> {
    match manifest {
        SessionManifest::Vod(v) => build_vod_lines(v, probe),
        SessionManifest::Live(_) => vec![
            Line::from(Span::styled("  Live stream (unknown duration)", style_dim())),
        ],
    }
}

fn build_vod_lines<'a>(v: &'a VodManifest, probe: &'a ProbeResult) -> Vec<Line<'a>> {
    let snap_pct = if v.keyframe_snap && !v.chunk_map.is_empty() {
        let snapped = v.chunk_map.iter().filter(|c| c.keyframe_snapped).count();
        Some(100 * snapped / v.chunk_map.len())
    } else {
        None
    };

    vec![
        Line::from(vec![
            Span::styled("  Video:    ", style_label()),
            Span::styled(v.video_codec.clone(), style_value()),
            Span::styled("  ", style_dim()),
            Span::styled(format!("{}×{}", v.resolution.0, v.resolution.1), style_value()),
            Span::styled("  ", style_dim()),
            Span::styled(format!("{:.3} fps", v.framerate), style_value()),
        ]),
        Line::from(vec![
            Span::styled("  Duration: ", style_label()),
            Span::styled(fmt_duration(v.duration_secs), style_value()),
            Span::styled("    Bitrate: ", style_label()),
            Span::styled(fmt_kbps(v.avg_bitrate_kbps), style_value()),
        ]),
        Line::from(vec![
            Span::styled("  Audio:    ", style_label()),
            Span::styled(fmt_audio_tracks(&v.audio_tracks), style_value()),
        ]),
        Line::from(vec![
            Span::styled("  Chunks:   ", style_label()),
            Span::styled(fmt_count(v.total_chunks as usize), style_value()),
            Span::styled(
                format!("  ×  {}ms target", v.chunk_duration_ms),
                style_dim(),
            ),
            if let Some(pct) = snap_pct {
                Span::styled(format!("    {}% keyframe-snapped", pct), style_dim())
            } else {
                Span::styled("    time-derived boundaries", style_dim())
            },
        ]),
        // Format name from probe (not in manifest)
        Line::from(vec![
            Span::styled("  Format:   ", style_label()),
            Span::styled(probe.format_name.clone(), style_dim()),
        ]),
    ]
}

// ── Error ─────────────────────────────────────────────────────────────────────

fn draw_error(f: &mut Frame, area: ratatui::layout::Rect, message: &str) {
    let block = Block::default()
        .title(Span::styled(" Error ", style_error()))
        .borders(Borders::ALL)
        .border_style(style_error());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {message}"), style_error())),
    ];
    f.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }),
        inner,
    );
}

// ── Help bar ──────────────────────────────────────────────────────────────────

fn draw_help(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    let mut add = |key: &'static str, desc: &'static str| {
        if !spans.is_empty() {
            spans.push(Span::styled("    ", style_dim()));
        }
        spans.push(Span::styled(key, style_key()));
        spans.push(Span::styled(format!(" {desc}"), style_dim()));
    };

    add("q", "quit");

    match &app.state {
        TuiState::Idle if app.source_path.is_some() => add("a", "analyze"),
        TuiState::ScanningKeyframes { .. } => add("s", "skip keyframe scan"),
        TuiState::ManifestReady { .. } | TuiState::Error { .. } => add("a", "re-analyze"),
        _ => {}
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn fmt_duration(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{}h {:02}m {:02}s", h, m, s)
    } else {
        format!("{}m {:02}s", m, s)
    }
}

fn fmt_kbps(kbps: u32) -> String {
    if kbps >= 1000 {
        format!("{:.1} Mbps", kbps as f32 / 1000.0)
    } else {
        format!("{} kbps", kbps)
    }
}

fn fmt_count(n: usize) -> String {
    // Simple thousands separator
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn fmt_audio_tracks(tracks: &[AudioTrack]) -> String {
    if tracks.is_empty() {
        return "none".into();
    }
    tracks
        .iter()
        .map(|t| {
            let lang = t.language.as_deref().unwrap_or("?");
            format!("{} {}ch ({})", t.codec, t.channels, lang)
        })
        .collect::<Vec<_>>()
        .join("  ·  ")
}
