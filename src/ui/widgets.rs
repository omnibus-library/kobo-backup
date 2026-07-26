//! Reusable widgets: the pieces that make every screen legible and honest.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};
use ratatui::Frame;

use crate::insights::LibraryInsights;
use crate::progress::ProgressUpdate;
use crate::util;
use crate::verify::{CheckStatus, VerifyReport};

pub const ACCENT: Color = Color::Cyan;
pub const OK: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const DANGER: Color = Color::Red;
pub const DIM: Color = Color::DarkGray;

/// Persistent footer with the active keymap.
pub fn footer(f: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, (key, action)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  •  ", Style::default().fg(DIM)));
        }
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default().fg(Color::Black).bg(DIM),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*action, Style::default().fg(Color::Gray)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        area,
    );
}

/// Screen title bar with step context, e.g. "BACKUP — step 2 of 7".
pub fn title_bar(f: &mut Frame, area: Rect, title: &str, step: Option<(usize, usize)>) {
    let mut spans = vec![Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some((current, total)) = step {
        spans.push(Span::styled(
            format!("  step {current} of {total}"),
            Style::default().fg(DIM),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A safety-status line: plain words about what has and has not been touched.
pub fn safety_line(f: &mut Frame, area: Rect, text: &str, color: Color) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(text, Style::default().fg(color)),
        ]))
        .alignment(Alignment::Center),
        area,
    );
}

/// Two-button confirm selector. `proceed_selected == false` means Abort is
/// focused — always the default, so Enter-mashing cannot pass a gate.
pub fn confirm_bar(
    f: &mut Frame,
    area: Rect,
    abort_label: &str,
    proceed_label: &str,
    proceed_selected: bool,
    proceed_is_danger: bool,
) {
    let selected = Style::default()
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let idle = Style::default().fg(Color::Gray);
    let abort_style = if !proceed_selected {
        selected.bg(WARN)
    } else {
        idle
    };
    let proceed_style = if proceed_selected {
        selected.bg(if proceed_is_danger { DANGER } else { OK })
    } else {
        idle
    };
    let line = Line::from(vec![
        Span::styled(format!("  {abort_label}  "), abort_style),
        Span::raw("      "),
        Span::styled(format!("  {proceed_label}  "), proceed_style),
    ]);
    f.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

/// Rich progress pane: overall gauge, current file, throughput, ETA, counts.
pub fn progress_pane(
    f: &mut Frame,
    area: Rect,
    progress: &ProgressUpdate,
    elapsed: std::time::Duration,
) {
    let rows = Layout::vertical([
        Constraint::Length(1), // phase
        Constraint::Length(1), // spacing
        Constraint::Length(3), // gauge
        Constraint::Length(1), // counts
        Constraint::Length(1), // throughput / eta
        Constraint::Length(1), // spacing
        Constraint::Length(1), // current file
        Constraint::Length(1), // detail
        Constraint::Min(0),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(progress.phase.as_str().bold()).alignment(Alignment::Center),
        rows[0],
    );

    let ratio = if progress.bytes_total > 0 {
        progress.bytes_done as f64 / progress.bytes_total as f64
    } else if progress.files_total > 0 {
        progress.files_done as f64 / progress.files_total as f64
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    f.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL))
            .gauge_style(Style::default().fg(ACCENT))
            .ratio(ratio)
            .label(format!("{:.1}%", ratio * 100.0)),
        rows[2],
    );

    let counts = if progress.bytes_total > 0 {
        format!(
            "{} / {} files   •   {} / {}",
            progress.files_done,
            progress.files_total,
            util::bytes(progress.bytes_done),
            util::bytes(progress.bytes_total),
        )
    } else {
        format!("{} files found so far", progress.files_done)
    };
    f.render_widget(Paragraph::new(counts).alignment(Alignment::Center), rows[3]);

    let mut speed = format!(
        "elapsed {}   •   {}",
        util::duration(elapsed),
        util::throughput(progress.bytes_done, elapsed)
    );
    if let Some(eta) = util::eta(progress.bytes_done, progress.bytes_total, elapsed) {
        speed.push_str(&format!("   •   ~{} remaining", util::duration(eta)));
    }
    f.render_widget(
        Paragraph::new(speed.fg(DIM)).alignment(Alignment::Center),
        rows[4],
    );

    let path = if progress.current_path.is_empty() {
        "—".to_string()
    } else {
        progress.current_path.clone()
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("current: ", Style::default().fg(DIM)),
            Span::raw(path),
        ]))
        .alignment(Alignment::Center),
        rows[6],
    );

    if let Some(detail) = &progress.detail {
        f.render_widget(
            Paragraph::new(detail.as_str().fg(OK)).alignment(Alignment::Center),
            rows[7],
        );
    }
}

/// The library insights panel: real facts about the data being protected.
pub fn insights_lines(insights: &LibraryInsights, header: &str) -> Vec<Line<'static>> {
    let fact = |label: &str, value: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{label:<28}"), Style::default().fg(DIM)),
            Span::styled(value, Style::default().fg(Color::White).bold()),
        ])
    };
    let opt = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_else(|| "–".into());

    let mut lines = vec![
        Line::from(header.to_string().fg(ACCENT).bold()),
        Line::raw(""),
    ];
    if let (Some(total), side, store) = (
        insights.total_books,
        insights.sideloaded_books,
        insights.store_books,
    ) {
        let mut v = format!("{total}");
        if let (Some(s), Some(k)) = (side, store) {
            v.push_str(&format!("   ({s} sideloaded, {k} Kobo-store)"));
        }
        lines.push(fact("Books on device", v));
    }
    lines.push(fact(
        "Annotations & highlights",
        opt(insights.annotations_total),
    ));
    if insights.annotations_total.unwrap_or(0) > 0 {
        let breakdown = format!(
            "{} highlights, {} notes, {} dog-ears",
            opt(insights.highlights),
            opt(insights.notes),
            opt(insights.dogears)
        );
        lines.push(fact("  breakdown", breakdown));
        lines.push(fact(
            "Books with annotations",
            opt(insights.books_with_annotations),
        ));
        if let Some(newest) = &insights.newest_annotation {
            lines.push(fact("  newest annotation", newest.clone()));
        }
    }
    lines.push(fact("Books in progress", opt(insights.books_in_progress)));
    lines.push(fact("Books finished", opt(insights.books_finished)));
    if let Some((title, pct)) = &insights.current_book {
        lines.push(fact("Currently reading", format!("{title} ({pct}%)")));
    }
    if let Some(shelves) = insights.shelves {
        lines.push(fact("Collections / shelves", shelves.to_string()));
    }
    lines.push(fact("Database size", util::bytes(insights.db_size_bytes)));
    lines
}

/// Verification report as a ✓/✗ table.
pub fn report_lines(report: &VerifyReport) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for check in &report.checks {
        let (mark, color) = match check.status {
            CheckStatus::Pass => ("✓", OK),
            CheckStatus::Warn => ("⚠", WARN),
            CheckStatus::Fail => ("✗", DANGER),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {mark} "), Style::default().fg(color).bold()),
            Span::styled(
                format!("{:<38}", check.name),
                Style::default().fg(Color::White).bold(),
            ),
            Span::styled(
                format!("({})", util::duration(check.duration)),
                Style::default().fg(DIM),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("     {}", check.detail),
            Style::default().fg(Color::Gray),
        )));
    }
    lines
}

/// Centered modal overlay (for abort/quit confirmation).
pub fn modal(f: &mut Frame, title: &str, body: &str, buttons: Line, danger: bool) {
    let area = f.area();
    let width = (area.width.saturating_sub(10)).clamp(30, 64);
    let height = 8;
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if danger { DANGER } else { WARN }))
        .title(format!(" {title} "));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let rows = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), rows[0]);
    f.render_widget(
        Paragraph::new(buttons).alignment(Alignment::Center),
        rows[2],
    );
}

/// Standard outer layout: title, body, safety line, confirm/footer rows.
pub struct Chrome {
    pub title: Rect,
    pub body: Rect,
    pub safety: Rect,
    pub actions: Rect,
    pub footer: Rect,
}

pub fn chrome(f: &mut Frame) -> Chrome {
    let rows = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1),
        Constraint::Min(5),    // body
        Constraint::Length(1), // safety
        Constraint::Length(1), // actions
        Constraint::Length(1), // footer
    ])
    .split(f.area());
    Chrome {
        title: rows[0],
        body: rows[2],
        safety: rows[3],
        actions: rows[4],
        footer: rows[5],
    }
}

/// Body rect with a margin.
pub fn padded(area: Rect, horizontal: u16) -> Rect {
    Layout::horizontal([
        Constraint::Length(horizontal),
        Constraint::Min(10),
        Constraint::Length(horizontal),
    ])
    .split(area)[1]
}
