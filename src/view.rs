//! The account cards, shared by the TUI and `subs` status output.
//!
//!   codex ────────────────────────────────────────────────────────────────
//! ▎ ● ana@example.com                          1 full reset left · prolite
//! ▎   weekly  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━─  97% left   resets in 6d 19h

use crate::prov::{Win, ALL};
use crate::status::{until, Row, UsageState};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;
/// Widest the card column grows; past this bars stop stretching.
pub const MAX: usize = 76;
/// Columns a window line uses besides its bar.
const FIXED: usize = 41;

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
            span(
                format!("  {name} "),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            muted("─".repeat(cw.saturating_sub(name.len() + 3))),
        ]));
        let mine: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].prov == p).collect();
        if mine.is_empty() {
            lines.push(Line::from(vec![muted(format!(
                "    no accounts · n to sign in, or subs login {name}"
            ))]));
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
    let edge = if selected {
        span("▎", fg(ACCENT))
    } else {
        span(" ", Style::new())
    };
    let dot = if r.active {
        span("●", fg(Color::Green))
    } else {
        muted("○")
    };
    let meta = meta(r);
    let room = cw.saturating_sub(4 + width(&meta) + 2).max(8);
    let email = trunc(
        if r.email.is_empty() && r.token.as_ref().is_some_and(|t| t["present"] == true) {
            "setup-token"
        } else if r.email.is_empty() {
            "no login"
        } else {
            &r.email
        },
        room,
    );
    let style = if selected {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    let pad = cw.saturating_sub(4 + email.chars().count() + width(&meta));
    let mut head = vec![
        edge.clone(),
        span(" ", Style::new()),
        dot,
        span(" ", Style::new()),
        span(email, style),
    ];
    head.push(span(" ".repeat(pad), Style::new()));
    head.extend(meta);
    let mut out = vec![Line::from(head)];
    match &r.usage {
        UsageState::InactiveOAuth(_) => {
            out.push(Line::from(vec![
                edge,
                muted("   saved OAuth login · inactive (setup-token selected)"),
            ]));
        }
        UsageState::Unsupported => {
            out.push(Line::from(vec![
                edge,
                muted("   account and usage reporting not supported by this token"),
            ]));
        }
        UsageState::Error(e) => out.push(Line::from(vec![
            edge,
            span(
                format!("   {e}"),
                fg(if e.starts_with("saved ·") {
                    MUTED
                } else if !r.active && e.starts_with("sign in required") {
                    Color::Yellow
                } else {
                    Color::Red
                }),
            ),
        ])),
        UsageState::Available(u) if u.windows.is_empty() => {
            out.push(Line::from(vec![edge, muted("   no usage reported")]))
        }
        UsageState::Available(u) => {
            out.extend(u.windows.iter().map(|w| window(edge.clone(), w, bw)))
        }
    }
    if let Some(t) = r.token.as_ref().filter(|t| t["present"] == true) {
        out.push(Line::from(vec![muted(format!(
            "    {}",
            crate::prov::claude::health_summary(t)
        ))]));
    }
    if let Some(u) = r.usage.reported().filter(|u| u.spend.used.is_some()) {
        out.push(Line::from(vec![muted(format!(
            "    spend {}",
            u.spend.summary()
        ))]));
    }
    out
}

/// Right-hand side of a card's first line: state, credits, resets, plan.
fn meta(r: &Row) -> Vec<Span<'static>> {
    let Some(u) = r.usage.reported() else {
        return vec![];
    };
    let mut parts: Vec<Vec<Span<'static>>> = vec![];
    if u.limited {
        parts.push(vec![span(
            "used up",
            fg(Color::Red).add_modifier(Modifier::BOLD),
        )]);
    }
    if let Some(x) = &u.extra {
        parts.push(vec![muted(x.clone())]);
    }
    match u.resets {
        Some(0) => parts.push(vec![muted("no full resets")]),
        Some(n) => parts.push(vec![
            span(
                n.to_string(),
                fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            muted(if n == 1 {
                " full reset left"
            } else {
                " full resets left"
            }),
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
    let label = match w.label.as_str() {
        "5h" => "5-hour",
        "1d" => "daily",
        "7d" => "weekly",
        "30d" => "monthly",
        l => l,
    };
    let mut l = vec![
        edge,
        span("   ", Style::new()),
        muted(format!("{label:<8}")),
    ];
    // The bar is a fuel gauge: what is still left, on a track of what is used.
    match w.pct.map(|used| 100.0 - used.clamp(0.0, 100.0)) {
        Some(left) => {
            let c = level(100.0 - left);
            let halves = (left / 100.0 * (bw * 2) as f64).round() as usize;
            let (full, half) = (halves / 2, halves % 2 == 1);
            l.push(span("━".repeat(full), fg(c)));
            if half {
                l.push(span("╾", fg(c)));
            }
            l.push(muted("─".repeat(bw - full - half as usize)));
            let bold = if left <= 10.0 {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            l.push(span(
                format!("{:>5}", format!("{left:.0}%")),
                fg(c).add_modifier(bold),
            ));
            l.push(muted(" left"));
        }
        None => {
            l.push(muted("─".repeat(bw)));
            l.push(muted("    – left"));
        }
    }
    let reset = w
        .reset
        .map(|t| format!("resets in {}", until(t)))
        .unwrap_or_default();
    l.push(muted(format!("{reset:>19}")));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prov::{Prov, Spend, Usage};

    #[test]
    fn configured_token_keeps_dormant_oauth_states_neutral_but_preserves_actual_failures() {
        let mut rows = vec![
            Row {
                prov: Prov::Claude,
                email: "old@example.com".into(),
                active: false,
                password: false,
                token: None,
                usage: UsageState::InactiveOAuth(
                    "sign in required · subs login claude old@example.com".into(),
                ),
            },
            Row {
                prov: Prov::Claude,
                email: String::new(),
                active: true,
                password: false,
                token: Some(
                    serde_json::json!({"present": true, "state": "unverified", "valid": null}),
                ),
                usage: UsageState::Unsupported,
            },
        ];
        let render = |rows: &[Row]| {
            body(rows, None, 76)
                .lines
                .iter()
                .map(|l| ansi(l, true))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = render(&rows);
        assert!(text.contains("saved OAuth login · inactive (setup-token selected)"));
        assert!(text.contains("setup-token: configured"));
        assert!(text.contains("account and usage reporting not supported"));
        assert!(!text.contains("sign in required") && !text.contains("unverified"));
        assert!(!text.contains("\x1b[31m") && !text.contains("\x1b[33m"));
        let json = crate::status::to_json(&rows);
        assert!(json[0].get("err").is_none());
        assert_eq!(json[0]["usage_status"], "inactive");
        assert!(json[0]["inactive_reason"]
            .as_str()
            .unwrap()
            .contains("sign in required"));
        assert!(json[1].get("err").is_none());
        assert_eq!(json[1]["usage_status"], "unsupported");
        assert_eq!(json[1]["setup_token"]["state"], "unverified");
        assert!(json[1]["setup_token"]["valid"].is_null());

        rows[1].usage = UsageState::Error("HTTP 401".into());
        assert!(render(&rows).contains("\x1b[31m   HTTP 401"));
        rows[1].active = false;
        rows[1].token = Some(
            serde_json::json!({"present": true, "state": "unusable", "error": "unsafe permissions"}),
        );
        let text = render(&rows);
        assert!(text.contains("setup-token: unusable") && text.contains("unsafe permissions"));
        rows[0].usage = UsageState::Error("sign in required".into());
        assert!(render(&rows).contains("sign in required"));
    }

    #[test]
    fn normal_subscription_card_omits_unavailable_spend_and_absent_token() {
        let mut row = Row {
            prov: Prov::Claude,
            email: "a@example.com".into(),
            active: true,
            password: false,
            token: Some(serde_json::json!({"present": false, "state": "absent"})),
            usage: UsageState::Available(Usage {
                spend: Spend::unknown("flat subscription; dollar spend not exposed"),
                plan: Some("max 20x".into()),
                windows: vec![Win {
                    label: "7d".into(),
                    pct: Some(0.0),
                    reset: None,
                }],
                resets: None,
                extra: None,
                limited: false,
            }),
        };
        let render = |r: &Row| {
            card(r, false, 76, 35)
                .iter()
                .map(|l| ansi(l, false))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = render(&row);
        assert!(text.contains("max 20x") && text.contains("100%"));
        assert!(!text.contains("spend") && !text.contains("setup-token"));
        if let UsageState::Available(u) = &mut row.usage {
            u.spend.cap = Some(200.0);
        }
        assert!(!render(&row).contains("spend"));
        if let UsageState::Available(u) = &mut row.usage {
            u.spend.used = Some(1.25);
        }
        assert!(render(&row).contains("spend"));
        row.usage = UsageState::Error("HTTP 401".into());
        assert!(render(&row).contains("HTTP 401"));
        assert!(!render(&row).contains("spend n/a"));
    }
}
