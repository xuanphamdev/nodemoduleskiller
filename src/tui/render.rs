//! ratatui draw functions. Stateless: take `AppState`, write to a Frame.
//!
//! Visual goals: give the TUI personality without depending on Nerd Font or
//! wide emoji that breaks alignment. We use BMP-range Unicode glyphs that
//! all monospace fonts ship — box-drawing, geometric shapes, braille
//! patterns for the spinner.
//!
//! Layout (top to bottom):
//! ```text
//! ▓▓ nmk  ⟶  /home/me/projects                        [dry-run]
//!   ◆ 12 found   ▼ 1.20 GB total   ⠋ scanning   sort: ▼ size
//! ──────────────────────────────────────────────────────────────
//! ▶ ⚠  /home/me/proj-a/node_modules           1.20 GB   2d
//!   ·  /home/me/proj-b/node_modules           250 MB    1w
//!   ✗  /home/me/proj-c/node_modules           120 MB    —
//! ──────────────────────────────────────────────────────────────
//!  ↑↓ nav  d delete  s/n/m sort  r rescan  q quit
//! ```
//!
//! When the user requests a delete, a double-bordered modal overlays.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState,
};

use crate::core::types::{FolderResult, SortBy};
use crate::tui::app::{AppState, Mode};

/// Braille-pattern spinner frames (smoother than ASCII /-\|).
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame<'_>, state: &AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    draw_header(frame, chunks[0], state);
    draw_table(frame, chunks[1], state);
    draw_status(frame, chunks[2], state);

    if let Mode::Confirm(idx) = &state.mode {
        let row = state.results.get(*idx);
        draw_confirm_modal(frame, area, state, row);
    }
}

// ─── Header ──────────────────────────────────────────────────────────────────

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let logo =
        Span::styled(" ▓▓ nmk", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let arrow = Span::styled("  ⟶  ", Style::default().fg(Color::DarkGray));
    let path = Span::styled(state.root.display().to_string(), Style::default().fg(Color::White));
    let mut title_spans = vec![logo, arrow, path];
    if state.dry_run {
        title_spans.push(Span::styled(
            "   [dry-run]",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ));
    }
    let title = Line::from(title_spans);

    let (state_icon, state_label, state_style) = if state.scan_finished {
        ("✓", "done", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
    } else {
        (spinner_frame(), "scanning", Style::default().fg(Color::Yellow))
    };
    let sort_label = match state.sort {
        SortBy::Path => "name",
        SortBy::Size => "size",
        SortBy::Age => "last-used",
    };
    let dim = Style::default().fg(Color::DarkGray);
    let stats = Line::from(vec![
        Span::raw("  "),
        Span::styled("◆ ", Style::default().fg(Color::Cyan)),
        Span::styled(
            format!("{} found", state.results.len()),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled("    ", dim),
        Span::styled("▼ ", Style::default().fg(Color::Cyan)),
        Span::styled(
            human_bytes(state.total_size()),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" total", dim),
        Span::styled("    ", dim),
        Span::styled(format!("{state_icon} "), state_style),
        Span::styled(state_label, state_style),
        Span::styled("    ", dim),
        Span::styled("sort ", dim),
        Span::styled(
            format!("{} {sort_label}", state.sort_direction.indicator()),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ]);

    let p = Paragraph::new(vec![title, stats]).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(p, area);
}

// ─── Results table ───────────────────────────────────────────────────────────

fn draw_table(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let header_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from(Span::styled("PATH", header_style)),
        Cell::from(Span::styled("SIZE", header_style)),
        Cell::from(Span::styled("AGE", header_style)),
    ])
    .height(1);

    let rows: Vec<Row> = state
        .results
        .iter()
        .map(|r| {
            let risk_marker = if r.deleted {
                Span::styled("✗", Style::default().fg(Color::DarkGray))
            } else {
                match &r.risk {
                    Some(a) if a.is_sensitive => Span::styled(
                        "⚠",
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    _ => Span::styled("·", Style::default().fg(Color::DarkGray)),
                }
            };

            let path_style = if r.deleted {
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT)
            } else {
                Style::default().fg(Color::White)
            };
            let path_span = Span::styled(r.path.display().to_string(), path_style);

            let (size_text, size_style_v) = match r.size_bytes {
                Some(b) => (human_bytes(b), size_style(b)),
                None => ("…".to_string(), Style::default().fg(Color::DarkGray)),
            };

            let age_text = human_age(r.last_modified);
            let age_style = if r.last_modified.is_some() {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            Row::new(vec![
                Cell::from(risk_marker),
                Cell::from(path_span),
                Cell::from(Span::styled(size_text, size_style_v)),
                Cell::from(Span::styled(age_text, age_style)),
            ])
        })
        .collect();

    let widths =
        [Constraint::Length(2), Constraint::Min(30), Constraint::Length(10), Constraint::Length(6)];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .block(Block::default().borders(Borders::NONE))
        .row_highlight_style(
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut t_state = TableState::default();
    if !state.results.is_empty() {
        t_state.select(Some(state.cursor.min(state.results.len() - 1)));
    }

    frame.render_stateful_widget(table, area, &mut t_state);
}

// ─── Status bar ──────────────────────────────────────────────────────────────

fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut spans = match state.mode {
        Mode::Browse => vec![
            key_span(" ↑↓ "),
            label_span("nav  "),
            key_span("d "),
            label_span("delete  "),
            key_span("s/n/m "),
            label_span("sort  "),
            key_span("r "),
            label_span("rescan  "),
            key_span("q "),
            label_span("quit"),
        ],
        Mode::Confirm(_) => {
            vec![key_span(" y "), label_span("delete  "), key_span("n/Esc "), label_span("cancel")]
        }
    };
    if let Some(msg) = &state.last_message {
        spans.push(Span::raw("    "));
        spans.push(Span::styled(
            msg.clone(),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ));
    }
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), area);
}

fn key_span(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
}

fn label_span(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::default().fg(Color::DarkGray))
}

// ─── Confirm modal ───────────────────────────────────────────────────────────

fn draw_confirm_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    row: Option<&FolderResult>,
) {
    let mut width = area.width.saturating_mul(3) / 5;
    width = width.clamp(50, area.width.saturating_sub(4).max(50));
    let height = 12u16.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    let modal = Rect { x, y, width, height };

    frame.render_widget(Clear, modal);

    let path = row.map(|r| r.path.display().to_string()).unwrap_or_else(|| "<missing>".into());
    let size = row.and_then(|r| r.size_bytes).map(human_bytes).unwrap_or_else(|| "—".into());
    let age = row.map(|r| human_age(r.last_modified)).unwrap_or_else(|| "—".into());
    let risk_reason = row.and_then(|r| r.risk.as_ref()).and_then(|a| {
        a.is_sensitive.then(|| a.reason.clone().unwrap_or_else(|| "sensitive path".into()))
    });

    let title_color = if state.dry_run { Color::Magenta } else { Color::Red };
    let title_style = Style::default().fg(title_color).add_modifier(Modifier::BOLD);
    let title = if state.dry_run {
        " ⚠  DRY-RUN — preview  ⚠ "
    } else {
        " ⚠  CONFIRM DELETE  ⚠ "
    };

    let mut body = vec![
        Line::from(""),
        Line::from(Span::styled(
            path,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("size  ", Style::default().fg(Color::DarkGray)),
            Span::styled(size, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("     last used  ", Style::default().fg(Color::DarkGray)),
            Span::styled(age, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]),
    ];
    if let Some(reason) = risk_reason {
        body.push(Line::from(vec![
            Span::styled("⚠ ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(reason, Style::default().fg(Color::Yellow)),
        ]));
    }
    body.push(Line::from(""));
    body.push(Line::from(if state.dry_run {
        Span::styled(
            "Dry-run mode — filesystem is NOT touched.",
            Style::default().fg(Color::Magenta),
        )
    } else {
        Span::styled(
            "This will be permanently removed.",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    }));
    body.push(Line::from(""));
    body.push(Line::from(vec![
        Span::styled("press ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " y ",
            Style::default().fg(Color::Black).bg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" confirm  ·  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " n / Esc ",
            Style::default().fg(Color::Black).bg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]));

    let block = Block::default()
        .title(Span::styled(title, title_style))
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(title_style);
    let p = Paragraph::new(body).alignment(Alignment::Center).block(block);
    frame.render_widget(p, modal);
}

// ─── Formatting helpers ──────────────────────────────────────────────────────

/// Format bytes as npkill-style human-readable (KB/MB/GB).
fn human_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = b as f64;
    if b < MB {
        format!("{:.0} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.2} GB", b / GB)
    }
}

/// Color-code a size by magnitude — quick scan tells you where the wins are.
fn size_style(b: u64) -> Style {
    if b >= 1_073_741_824 {
        // ≥ 1 GiB — red bold (biggest wins)
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if b >= 104_857_600 {
        // ≥ 100 MiB — yellow bold
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else if b >= 10_485_760 {
        // ≥ 10 MiB — white
        Style::default().fg(Color::White)
    } else {
        // tiny — gray
        Style::default().fg(Color::Gray)
    }
}

/// Human-friendly relative age: `30s`, `2m`, `3h`, `5d`, `2w`, `4mo`, `1y`.
fn human_age(t: Option<SystemTime>) -> String {
    let Some(t) = t else {
        return "—".into();
    };
    let Ok(elapsed) = SystemTime::now().duration_since(t) else {
        return "—".into();
    };
    let secs = elapsed.as_secs();
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
    const YEAR: u64 = 365 * DAY;
    if secs < MIN {
        format!("{secs}s")
    } else if secs < HOUR {
        format!("{}m", secs / MIN)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else if secs < WEEK {
        format!("{}d", secs / DAY)
    } else if secs < MONTH {
        format!("{}w", secs / WEEK)
    } else if secs < YEAR {
        format!("{}mo", secs / MONTH)
    } else {
        format!("{}y", secs / YEAR)
    }
}

/// Pick a braille spinner frame from the current wall-clock time. Frames
/// change roughly every 80 ms — pleasant motion without flicker.
fn spinner_frame() -> &'static str {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    SPINNER[((ms / 80) as usize) % SPINNER.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn human_bytes_kb_range() {
        assert!(human_bytes(0).ends_with("KB"));
        assert!(human_bytes(50 * 1024).ends_with("KB"));
    }

    #[test]
    fn human_bytes_mb_range() {
        let s = human_bytes(5 * 1024 * 1024);
        assert!(s.ends_with("MB"), "got {s}");
    }

    #[test]
    fn human_bytes_gb_range() {
        let s = human_bytes(3 * 1024 * 1024 * 1024);
        assert!(s.ends_with("GB"), "got {s}");
    }

    #[test]
    fn human_age_none_is_dash() {
        assert_eq!(human_age(None), "—");
    }

    #[test]
    fn human_age_unit_suffixes() {
        let now = SystemTime::now();
        assert!(human_age(Some(now - Duration::from_secs(30))).ends_with('s'));
        assert!(human_age(Some(now - Duration::from_secs(120))).ends_with('m'));
        assert!(human_age(Some(now - Duration::from_secs(7200))).ends_with('h'));
        assert!(human_age(Some(now - Duration::from_secs(86_400 * 2))).ends_with('d'));
        assert!(human_age(Some(now - Duration::from_secs(86_400 * 14))).ends_with('w'));
        let mo = human_age(Some(now - Duration::from_secs(86_400 * 60)));
        assert!(mo.ends_with("mo"), "got {mo}");
        let y = human_age(Some(now - Duration::from_secs(86_400 * 400)));
        assert!(y.ends_with('y'), "got {y}");
    }

    #[test]
    fn size_style_distinct_at_boundaries() {
        // Visual styles differ at the four magnitude tiers; we don't pin
        // specific colors (those are display preferences), only that the
        // smallest and largest produce different styles.
        assert_ne!(size_style(0), size_style(50 * 1024 * 1024 * 1024));
    }

    #[test]
    fn spinner_frame_is_one_of_set() {
        let frame = spinner_frame();
        assert!(SPINNER.contains(&frame));
    }
}
