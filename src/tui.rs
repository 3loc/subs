//! `subs tui`: the cards, live countdowns, and modals for the few inputs.

use crate::actions::{self, Res};
use crate::prov::{Prov, ALL};
use crate::push::{self, Done};
use crate::status::{self, Row};
use crate::view::{self, fg, muted, span, ACCENT, MAX, MUTED};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, PopKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::prelude::{CrosstermBackend, Frame, Terminal};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use std::io::{self, stdout, BufRead};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const KEYS: [(&str, &str); 8] = [
    ("↵", "use"),
    ("f", "push"),
    ("l", "login"),
    ("n", "new"),
    ("a", "pass"),
    ("p", "copy"),
    ("d", "delete"),
    ("q", "quit"),
];
/// Usage is re-fetched this often; countdowns tick every frame.
const AUTO: Duration = Duration::from_secs(300);
const TOAST: Duration = Duration::from_secs(6);

enum Then {
    NewLogin(Prov),
    Password(Prov, String),
    Forget(Prov, String),
    Use(Prov, String),
}

enum Modal {
    Pick { idx: usize },
    Input { title: String, prompt: String, value: String, secret: bool, then: Then },
    Confirm { title: String, text: String, detail: String, verb: &'static str, danger: bool, then: Then },
}

/// A fan-out in flight: one slot per host, filled as each answers.
struct Push {
    prov: Prov,
    email: String,
    note: String,
    hosts: Vec<String>,
    done: Vec<Option<Done>>,
    rx: Receiver<(usize, Done)>,
    started: Instant,
}

impl Push {
    fn finished(&self) -> bool {
        self.done.iter().all(Option::is_some)
    }

    fn counts(&self) -> (usize, usize, usize) {
        let count = |f: fn(&Done) -> bool| self.done.iter().flatten().filter(|d| f(d)).count();
        (count(|d| matches!(d, Done::Ok(_))), count(|d| matches!(d, Done::Offline)), count(|d| matches!(d, Done::Failed(_))))
    }

    fn summary(&self) -> (String, bool) {
        let (ok, off, bad) = self.counts();
        (format!("{} {} · {}", self.prov.name(), self.email, push::summary(ok, off, bad, self.hosts.len())), bad == 0)
    }
}

#[derive(Default)]
struct App {
    rows: Vec<Row>,
    sel: usize,
    scroll: usize,
    loading: Option<(Receiver<Vec<Row>>, Instant)>,
    again: bool,
    updated: Option<Instant>,
    toast: Option<(String, bool, Instant)>,
    modal: Option<Modal>,
    login: Option<(Prov, Option<String>)>,
    push: Option<Push>,
    quit: bool,
}

impl App {
    fn refresh(&mut self) {
        if self.loading.is_some() {
            self.again = true;
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(status::status());
        });
        self.loading = Some((rx, Instant::now()));
    }

    fn poll(&mut self) {
        if let Some(p) = self.push.as_mut() {
            while let Ok((i, d)) = p.rx.try_recv() {
                p.done[i] = Some(d);
            }
        }
        let got = match &self.loading {
            Some((rx, _)) => rx.try_recv(),
            None => return,
        };
        match got {
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => self.say(Err("refresh failed".into())),
            Ok(rows) => {
                let keep = self.rows.get(self.sel).map(|r| (r.prov, r.email.clone()));
                self.rows = rows;
                let at = keep.and_then(|(p, e)| self.rows.iter().position(|r| r.prov == p && r.email == e));
                self.sel = at.unwrap_or(self.sel).min(self.rows.len().saturating_sub(1));
                self.updated = Some(Instant::now());
            }
        }
        self.loading = None;
        if std::mem::take(&mut self.again) {
            self.refresh();
        }
    }

    fn say(&mut self, r: Res) {
        let (msg, ok) = match r {
            Ok(m) => (m, true),
            Err(m) => (m, false),
        };
        self.toast = Some((msg, ok, Instant::now()));
    }

    /// Fan `p`'s live login out to the hosts file, shown in an overlay.
    fn start_push(&mut self, p: Prov, note: String) {
        let hosts = push::hosts(p);
        if hosts.is_empty() {
            return self.say(Ok(format!("{note} · no hosts to push to, list them in {}", push::config().display())));
        }
        let (blob, email) = if crate::demo::on() {
            (serde_json::Value::Null, crate::demo::ACTIVE.lock().unwrap().to_string())
        } else {
            let Some(blob) = p.read(None) else { return self.say(Err(format!("no live {} login to push", p.name()))) };
            let email = p.email(&blob).unwrap_or_default();
            (blob, email)
        };
        let (tx, rx) = mpsc::channel();
        push::start(p, &hosts, blob, tx);
        let done = hosts.iter().map(|_| None).collect();
        self.push = Some(Push { prov: p, email, note, hosts, done, rx, started: Instant::now() });
    }

    /// Make `email` the live login here, then push it to the hosts.
    fn switch(&mut self, p: Prov, email: &str) {
        match actions::use_(p, email) {
            Ok(note) => {
                for row in self.rows.iter_mut().filter(|r| r.prov == p) {
                    row.active = row.email == email;
                }
                self.start_push(p, note);
            }
            Err(e) => self.say(Err(e)),
        }
        self.refresh();
    }

    /// After a sign-in: a re-login of the live account goes out at once,
    /// any other account asks first.
    fn signed_in(&mut self, p: Prov, l: actions::Login) {
        if l.live {
            self.start_push(p, l.msg);
        } else {
            let n = push::hosts(p).len();
            let hosts = if n == 0 { String::new() } else { format!(" and {n} host{}", if n == 1 { "" } else { "s" }) };
            self.modal = Some(Modal::Confirm {
                title: "signed in".into(),
                text: format!("Use {} for {} now?", l.email, p.name()),
                detail: format!("it goes live on this machine{hosts}"),
                verb: "use it",
                danger: false,
                then: Then::Use(p, l.email),
            });
        }
        self.refresh();
    }

    fn cur(&self) -> Option<(Prov, String, bool)> {
        self.rows.get(self.sel).map(|r| (r.prov, r.email.clone(), r.active))
    }

    fn key(&mut self, k: KeyEvent) {
        if let Some(p) = self.push.take() {
            let r = if p.finished() {
                let (msg, ok) = p.summary();
                if ok { Ok(msg) } else { Err(msg) }
            } else {
                let left = p.done.iter().filter(|d| d.is_none()).count();
                Ok(format!("still pushing to {left} host{} in the background", if left == 1 { "" } else { "s" }))
            };
            return self.say(r);
        }
        if self.modal.is_some() {
            return self.modal_key(k);
        }
        let n = self.rows.len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            // Not Esc: a terminal answering a query can send a stray one.
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab if n > 0 => self.sel = (self.sel + 1) % n,
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab if n > 0 => self.sel = (self.sel + n - 1) % n,
            KeyCode::Char('g') | KeyCode::Home => self.sel = 0,
            KeyCode::Char('G') | KeyCode::End => self.sel = n.saturating_sub(1),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('n') => self.modal = Some(Modal::Pick { idx: 0 }),
            _ => {}
        }
        let Some((p, email, active)) = self.cur() else { return };
        match k.code {
            KeyCode::Enter if active => self.say(Ok(format!("{email} is already the active {} login", p.name()))),
            KeyCode::Enter => {
                self.switch(p, &email);
            }
            KeyCode::Char('f') => self.start_push(p, format!("{}: pushing the active login again", p.name())),
            KeyCode::Char('l') => self.login = Some((p, Some(email))),
            KeyCode::Char('a') => {
                self.modal = Some(Modal::Input {
                    title: "password".into(),
                    prompt: format!("stored in plain text for {} {email}", p.name()),
                    value: String::new(),
                    secret: true,
                    then: Then::Password(p, email),
                })
            }
            KeyCode::Char('p') => {
                let r = match actions::password(p, &email) {
                    None => Err("no password stored · a to add one".into()),
                    Some(pw) if actions::clip(&pw) => Ok(format!("password for {email} copied")),
                    Some(pw) => Ok(format!("no clipboard here · password: {pw}")),
                };
                self.say(r);
            }
            KeyCode::Char('d') => {
                self.modal = Some(Modal::Confirm {
                    title: "forget".into(),
                    text: format!("Forget the {} login {email}?", p.name()),
                    detail: "its saved login and password are deleted".into(),
                    verb: "forget",
                    danger: true,
                    then: Then::Forget(p, email),
                })
            }
            _ => {}
        }
    }

    fn modal_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
            self.modal = None;
            return;
        }
        let enter = k.code == KeyCode::Enter;
        let yes = matches!(k.code, KeyCode::Char('y' | 'Y'));
        match self.modal.as_mut() {
            Some(Modal::Pick { idx }) => match k.code {
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h' | 'k') => *idx = 0,
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l' | 'j') => *idx = 1,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Char(' ') => *idx = 1 - *idx,
                _ => {}
            },
            Some(Modal::Input { value, .. }) => match k.code {
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char('u') if ctrl => value.clear(),
                KeyCode::Char(c) if !ctrl => value.push(c),
                _ => {}
            },
            _ => {}
        }
        match self.modal.take() {
            Some(Modal::Confirm { then, .. }) => {
                if yes {
                    self.submit(then, String::new())
                }
            }
            Some(Modal::Pick { idx }) if enter => {
                let p = ALL[idx];
                self.modal = Some(Modal::Input {
                    title: format!("new {} login", p.name()),
                    prompt: "email · optional, enter to skip".into(),
                    value: String::new(),
                    secret: false,
                    then: Then::NewLogin(p),
                })
            }
            Some(Modal::Input { value, then, .. }) if enter => self.submit(then, value),
            m => self.modal = m,
        }
    }

    fn submit(&mut self, then: Then, value: String) {
        match then {
            Then::NewLogin(p) => {
                let email = value.trim().to_string();
                self.login = Some((p, (!email.is_empty()).then_some(email)));
            }
            Then::Password(_, _) if value.is_empty() => self.say(Err("empty password, nothing stored".into())),
            Then::Password(p, email) => {
                let r = actions::store_password(p, &email, &value).map(|_| format!("password stored for {email}"));
                if r.is_ok() {
                    for row in self.rows.iter_mut().filter(|r| r.prov == p && r.email == email) {
                        row.password = true;
                    }
                }
                self.say(r);
            }
            Then::Use(p, email) => self.switch(p, &email),
            Then::Forget(p, email) => {
                let r = actions::rm(p, &email);
                if r.is_ok() {
                    self.rows.retain(|r| !(r.prov == p && r.email == email));
                    self.sel = self.sel.min(self.rows.len().saturating_sub(1));
                }
                self.say(r);
                self.refresh();
            }
        }
    }
}

fn enter() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    // A CLI that ran in between (claude, codex) can leave its keyboard and
    // focus modes on, and answers to its terminal queries still queued.
    execute!(
        stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        EnterAlternateScreen,
        cursor::Hide
    )?;
    while event::poll(Duration::from_millis(30))? {
        let _ = event::read()?;
    }
    Ok(())
}

fn leave() -> io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen, cursor::Show)
}

pub fn run() -> io::Result<()> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave();
        hook(info)
    }));
    enter()?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = App::default();
    app.refresh();
    while !app.quit {
        app.poll();
        if let Some((p, email)) = app.login.take() {
            leave()?;
            let who = email.as_deref().map(|e| format!(" as {e}")).unwrap_or_default();
            println!("\n\x1b[1;36msubs\x1b[0m · signing in to {}{who}", p.name());
            println!("\x1b[90mfollow the link, finish in the browser; you come back here when it is done\x1b[0m\n");
            let r = actions::login(p, email.as_deref(), false);
            if let Err(e) = &r {
                eprintln!("\n\x1b[31m✗ {e}\x1b[0m · press enter to go back");
                let _ = io::stdin().lock().read_line(&mut String::new());
            }
            enter()?;
            term.clear()?;
            match r {
                Ok(l) => app.signed_in(p, l),
                Err(e) => app.say(Err(e)),
            }
        }
        term.draw(|f| draw(f, &mut app))?;
        let busy = app.loading.is_some() || app.push.as_ref().is_some_and(|p| !p.finished());
        let wait = if busy { 80 } else { 1000 };
        if event::poll(Duration::from_millis(wait))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    app.key(k);
                }
            }
        }
        if app.loading.is_none() && app.updated.is_some_and(|t| t.elapsed() > AUTO) {
            app.refresh();
        }
    }
    leave()
}

fn ago(t: Instant) -> String {
    match t.elapsed().as_secs() {
        s if s < 10 => "updated just now".into(),
        s if s < 60 => format!("updated {s}s ago"),
        s => format!("updated {}m ago", s / 60),
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 48 || area.height < 12 {
        let msg = Paragraph::new(Line::from(muted("subs · make the window a little bigger")).centered());
        f.render_widget(msg, area.inner(Margin { horizontal: 0, vertical: area.height / 2 }));
        return;
    }
    let w = (area.width - 4).min(MAX as u16);
    let col = Rect { x: area.x + (area.width - w) / 2, y: area.y + 1, width: w, height: area.height - 2 };
    let [head, _, body, _, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(col);

    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut title: Vec<Span> = vec![span("  subs", fg(ACCENT).add_modifier(Modifier::BOLD))];
    if !app.rows.is_empty() {
        let n = app.rows.len();
        title.push(muted(format!("   {n} account{}", if n == 1 { "" } else { "s" })));
    }
    f.render_widget(Paragraph::new(Line::from(title)), head);
    let state = match (&app.loading, app.updated) {
        (Some((_, t)), _) => Line::from(vec![
            span(SPIN[(t.elapsed().as_millis() / 80) as usize % SPIN.len()], fg(ACCENT)),
            muted(" fetching usage "),
        ]),
        (None, Some(t)) => Line::from(muted(format!("{} ", ago(t)))),
        _ => Line::default(),
    };
    f.render_widget(Paragraph::new(state.right_aligned()), head);

    if app.rows.is_empty() && app.loading.is_some() && app.updated.is_none() {
        f.render_widget(Paragraph::new(Line::from(muted("  reading accounts…"))), body);
    } else {
        let b = view::body(&app.rows, Some(app.sel), w as usize);
        let h = body.height as usize;
        if let Some(&(top, end)) = b.spans.get(app.sel) {
            if top < app.scroll {
                app.scroll = top;
            }
            if end > app.scroll + h {
                app.scroll = end - h;
            }
        }
        app.scroll = app.scroll.min(b.lines.len().saturating_sub(h));
        f.render_widget(Paragraph::new(b.lines).scroll((app.scroll as u16, 0)), body);
    }

    let toast = app.toast.as_ref().filter(|t| t.2.elapsed() < TOAST);
    let footer = match toast {
        Some((msg, ok, _)) => {
            let (icon, c) = if *ok { ("✓", Color::Green) } else { ("✗", Color::Red) };
            Line::from(vec![span(format!("  {icon} "), fg(c).add_modifier(Modifier::BOLD)), span(msg.clone(), Style::new())])
        }
        None => {
            let mut l = vec![span("  ", Style::new())];
            let mut used = 2;
            for (k, what) in KEYS {
                let n = k.chars().count() + what.len() + 3;
                if used + n > w as usize {
                    break;
                }
                used += n;
                l.extend([span(k, bold), muted(format!(" {what}  "))]);
            }
            Line::from(l)
        }
    };
    f.render_widget(Paragraph::new(footer), foot);

    if let Some(m) = &app.modal {
        modal(f, m, area);
    }
    if let Some(p) = &app.push {
        push_overlay(f, p, area);
    }
}

fn modal(f: &mut Frame, m: &Modal, area: Rect) {
    let w = 56.min(area.width - 4);
    let h = 8;
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let (title, color) = match m {
        Modal::Pick { .. } => ("new login", ACCENT),
        Modal::Input { title, .. } => (title.as_str(), ACCENT),
        Modal::Confirm { title, danger, .. } => (title.as_str(), if *danger { Color::Red } else { ACCENT }),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(color))
        .title(Line::from(span(format!(" {title} "), fg(color).add_modifier(Modifier::BOLD))));
    let inner = block.inner(r).inner(Margin { horizontal: 2, vertical: 1 });
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    let hint = |s: &str| Line::from(muted(s.to_string()));
    let lines = match m {
        Modal::Pick { idx } => {
            let mut opts = vec![];
            for (i, p) in ALL.iter().enumerate() {
                let on = i == *idx;
                opts.push(span(if on { "● " } else { "○ " }, fg(if on { ACCENT } else { MUTED })));
                let st = if on { Style::new().add_modifier(Modifier::BOLD) } else { fg(MUTED) };
                opts.push(span(format!("{:<10}", p.name()), st));
            }
            vec![hint("provider"), Line::from(opts), Line::default(), hint("←/→ choose · enter next · esc cancel")]
        }
        Modal::Input { prompt, value, secret, .. } => {
            let shown = if *secret { "•".repeat(value.chars().count()) } else { value.clone() };
            let room = (inner.width as usize).saturating_sub(3);
            let skip = shown.chars().count().saturating_sub(room);
            let shown: String = shown.chars().skip(skip).collect();
            f.set_cursor_position((inner.x + 2 + shown.chars().count() as u16, inner.y + 1));
            vec![
                hint(prompt),
                Line::from(vec![span("› ", fg(ACCENT)), span(shown, Style::new())]),
                Line::default(),
                hint("enter ok · esc cancel"),
            ]
        }
        Modal::Confirm { text, detail, verb, .. } => vec![
            Line::from(text.clone()),
            hint(detail),
            Line::default(),
            Line::from(vec![span("y", Style::new().add_modifier(Modifier::BOLD)), muted(format!(" {verb} · any other key cancels"))]),
        ],
    };
    f.render_widget(Paragraph::new(lines), inner);
}

fn push_overlay(f: &mut Frame, p: &Push, area: Rect) {
    let w = 64.min(area.width - 4);
    let notes = wrap(&p.note, w.saturating_sub(6) as usize).len() as u16;
    let h = (p.hosts.len() as u16 + 7 + notes).min(area.height - 2);
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let title = format!(" push {} · {} ", p.prov.name(), p.email);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(fg(ACCENT))
        .title(Line::from(span(title, fg(ACCENT).add_modifier(Modifier::BOLD))));
    let inner = block.inner(r).inner(Margin { horizontal: 2, vertical: 1 });
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    let spin = SPIN[(p.started.elapsed().as_millis() / 80) as usize % SPIN.len()];
    let hw = p.hosts.iter().map(|h| h.chars().count()).max().unwrap_or(0) + 2;
    let room = (inner.width as usize).saturating_sub(hw + 2);
    let mut lines: Vec<Line> = wrap(&p.note, inner.width as usize).into_iter().map(|l| Line::from(muted(l))).collect();
    lines.push(Line::default());
    for (host, d) in p.hosts.iter().zip(&p.done) {
        let (icon, ic, text, tc) = match d {
            None => (spin, ACCENT, "pushing…".to_string(), MUTED),
            Some(Done::Ok(m)) => ("✓", Color::Green, m.clone(), MUTED),
            Some(Done::Offline) => ("○", MUTED, "offline".to_string(), MUTED),
            Some(Done::Failed(m)) => ("✗", Color::Red, m.clone(), Color::Red),
        };
        lines.push(Line::from(vec![
            span(format!("{icon} "), fg(ic)),
            span(format!("{host:<hw$}"), Style::new()),
            span(text.chars().take(room).collect::<String>(), fg(tc)),
        ]));
    }
    lines.push(Line::default());
    let n = p.done.iter().flatten().count();
    lines.push(if p.finished() {
        let (ok, off, bad) = p.counts();
        Line::from(vec![muted(push::summary(ok, off, bad, p.hosts.len())), muted(" · any key closes")])
    } else {
        Line::from(muted(format!("{n} of {} done · any key hides this, the push carries on", p.hosts.len())))
    });
    f.render_widget(Paragraph::new(lines), inner);
}

/// Break `text` into lines of at most `width` columns at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for word in text.split(' ') {
        match out.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= width => {
                l.push(' ');
                l.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
    }
    out
}
