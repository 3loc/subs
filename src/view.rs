//! The account cards, shared by the TUI and `subs` status output.
//!
//!   codex ──────────────────────────────────────────────────────
//! ▎ ● ana@example.com                      2 resets · prolite
//! ▎   5h   ━━━━━━━━━━━━━━──────────────────────────  31%   ↻ 2h 14m
//! ▎   7d   ━━━━━━━━━━━━━━━━━━━━━━━━━━━─────────────  58%    ↻ 3d 4h

use crate::prov::{Win, ALL};
use crate::status::{until, Row};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;
/// Widest the card column grows; past this bars stop stretching.
pub const MAX: usize = 70;
/// Columns a window line uses besides its bar.
const FIXED: usize = 25;

pub fn span(t: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(t.into(), style)
}

pub fn fg(c: Color) -> Style {
    Style::new().fg(c)
}

pub fn muted(t: impl Into<String>) -> Span<'static> {
    span(t, fg(MUTED))
}

pub fn level(pct: f64) -> Color {
    match pct {
        p if p >= 90.0 => Color::Red,
        p if p >= 70.0 => Color::Yellow,
        _ => Color::Green,
    }
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).chain(['…']).collect()
    }
}

pub struct Body {
    pub lines: Vec<Line<'static>>,
    /// Per row: first line to keep in view (its section header for the first
    /// card of a section) and the line after its card.
    pub spans: Vec<(usize, usize)>,
}

pub fn body(rows: &[Row], sel: Option<usize>, cols: usize) -> Body {
    let bw = cols.saturating_sub(FIXED).clamp(6, MAX - FIXED);
    let cw = bw + FIXED;
    let mut lines = vec![];
    let mut spans = vec![(0, 0); rows.len()];
    for p in ALL {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let top = lines.len();
        let name = p.name();
        lines.push(Line::from(vec![
            span(format!("  {name} "), Style::new().add_modifier(Modifier::BOLD)),
            muted("─".repeat(cw.saturating_sub(name.len() + 3))),
        ]));
        let mine: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].prov == p).collect();
        if mine.is_empty() {
            lines.push(Line::from(vec![muted(format!("    no accounts · n to sign in, or subs login {name}"))]));
        }
        for (n, i) in mine.into_iter().enumerate() {
            if n > 0 {
                lines.push(Line::default());
            }
            let start = lines.len();
            lines.extend(card(&rows[i], sel == Some(i), cw, bw));
            spans[i] = (if n == 0 { top } else { start }, lines.len());
        }
    }
    Body { lines, spans }
}

fn card(r: &Row, selected: bool, cw: usize, bw: usize) -> Vec<Line<'static>> {
    let edge = if selected { span("▎", fg(ACCENT)) } else { span(" ", Style::new()) };
    let dot = if r.active { span("●", fg(Color::Green)) } else { muted("○") };
    let meta = meta(r);
    let room = cw.saturating_sub(4 + width(&meta) + 2).max(8);
    let email = trunc(&r.email, room);
    let style = if selected { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let pad = cw.saturating_sub(4 + email.chars().count() + width(&meta));
    let mut head = vec![edge.clone(), span(" ", Style::new()), dot, span(" ", Style::new()), span(email, style)];
    head.push(span(" ".repeat(pad), Style::new()));
    head.extend(meta);
    let mut out = vec![Line::from(head)];
    match &r.usage {
        Err(e) => out.push(Line::from(vec![edge, span(format!("   ✗ {e}"), fg(Color::Red))])),
        Ok(u) if u.windows.is_empty() => out.push(Line::from(vec![edge, muted("   no usage reported")])),
        Ok(u) => out.extend(u.windows.iter().map(|w| window(edge.clone(), w, bw))),
    }
    out
}

/// Right-hand side of a card's first line: state, credits, resets, plan.
fn meta(r: &Row) -> Vec<Span<'static>> {
    let Ok(u) = &r.usage else { return vec![] };
    let mut parts: Vec<Vec<Span<'static>>> = vec![];
    if u.limited {
        parts.push(vec![span("limited", fg(Color::Red).add_modifier(Modifier::BOLD))]);
    }
    if let Some(x) = &u.extra {
        parts.push(vec![muted(x.clone())]);
    }
    match u.resets {
        Some(0) => parts.push(vec![muted("no resets")]),
        Some(n) => parts.push(vec![
            span(n.to_string(), fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            muted(if n == 1 { " reset" } else { " resets" }),
        ]),
        None => {}
    }
    if let Some(p) = &u.plan {
        parts.push(vec![span(p.clone(), fg(Color::Magenta))]);
    }
    let mut out = vec![];
    for (i, p) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(muted(" · "));
        }
        out.extend(p);
    }
    out
}

fn window(edge: Span<'static>, w: &Win, bw: usize) -> Line<'static> {
    let mut l = vec![edge, span("   ", Style::new()), muted(format!("{:<4}", w.label)), span(" ", Style::new())];
    match w.pct {
        Some(p) => {
            let c = level(p);
            let halves = (p.clamp(0.0, 100.0) / 100.0 * (bw * 2) as f64).round() as usize;
            let (full, half) = (halves / 2, halves % 2 == 1);
            l.push(span("━".repeat(full), fg(c)));
            if half {
                l.push(span("╾", fg(c)));
            }
            l.push(muted("─".repeat(bw - full - half as usize)));
            let bold = if p >= 90.0 { Modifier::BOLD } else { Modifier::empty() };
            l.push(span(format!("{:>5}", format!("{p:.0}%")), fg(c).add_modifier(bold)));
        }
        None => {
            l.push(muted("─".repeat(bw)));
            l.push(muted("    –"));
        }
    }
    l.push(muted(format!("{:>11}", w.reset.map(|t| format!("↻ {}", until(t))).unwrap_or_default())));
    Line::from(l)
}

/// A line as ANSI text for the terminal, plain when `color` is off.
pub fn ansi(line: &Line, color: bool) -> String {
    let mut out = String::new();
    for s in &line.spans {
        let mut codes = vec![];
        if s.style.add_modifier.contains(Modifier::BOLD) {
            codes.push("1");
        }
        codes.extend(s.style.fg.and_then(sgr));
        if color && !codes.is_empty() {
            out += &format!("\x1b[{}m{}\x1b[0m", codes.join(";"), s.content);
        } else {
            out += &s.content;
        }
    }
    out.trim_end().to_string()
}

fn sgr(c: Color) -> Option<&'static str> {
    Some(match c {
        Color::Red => "31",
        Color::Green => "32",
        Color::Yellow => "33",
        Color::Magenta => "35",
        Color::Cyan => "36",
        Color::DarkGray => "90",
        _ => return None,
    })
}
