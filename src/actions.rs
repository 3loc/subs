//! What the commands and TUI keys do. Each returns the one-line outcome.

use crate::prov::{claude_keychain, Prov};
use crate::store::{self, rjson, snap_path, wjson};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal;
use serde_json::{json, Value};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

pub type Res = Result<String, String>;

fn io(e: io::Error) -> String {
    e.to_string()
}

fn running(name: &str) -> usize {
    Command::new("pgrep")
        .args(["-x", name])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().count())
        .unwrap_or(0)
}

/// Swap a saved login in, saving the current one first.
pub fn use_(p: Prov, email: &str) -> Res {
    activate(p, email, false)
}

/// `force` rewrites the login even when the same account is already live,
/// which is how a fresh sign-in replaces a dead token.
fn activate(p: Prov, email: &str, force: bool) -> Res {
    if crate::demo::on() {
        return Ok(crate::demo::switch(email));
    }
    let name = p.name();
    let blob = rjson(&snap_path(p, email))
        .ok_or_else(|| format!("no saved {name} login for {email} · subs login {name} {email}"))?;
    let live = p.read(None);
    let cur = live.as_ref().and_then(|b| p.email(b));
    let same = cur.as_deref() == Some(email);
    if same && !force {
        return Ok(format!("{name}: {email} is already active"));
    }
    if let (Some(c), Some(l), false) = (&cur, &live, same) {
        wjson(&snap_path(p, c), l).map_err(io)?;
    }
    p.write(&blob)?;
    let mut msg = if same {
        format!("{name}: {email} signed in again")
    } else {
        format!("{name}: {} → {email}", cur.as_deref().unwrap_or("–"))
    };
    match running(name) {
        0 => {}
        n => msg += &format!(" · restart {n} {name} session{}", if n == 1 { "" } else { "s" }),
    }
    Ok(msg)
}

pub fn password(p: Prov, email: &str) -> Option<String> {
    store::accounts()[p.name()][email]["password"].as_str().filter(|s| !s.is_empty()).map(String::from)
}

pub fn store_password(p: Prov, email: &str, pw: &str) -> Result<(), String> {
    let mut a = store::accounts();
    a[p.name()][email]["password"] = json!(pw);
    store::save_accounts(&a).map_err(io)
}

pub fn add(p: Prov, email: &str) -> Res {
    let pw = if io::stdin().is_terminal() {
        read_secret(&format!("password for {} {email}: ", p.name()))
    } else {
        let mut s = String::new();
        io::stdin().lock().read_line(&mut s).map_err(io)?;
        s.trim_end_matches(['\r', '\n']).to_string()
    };
    if pw.is_empty() {
        return Err("empty password, nothing stored".into());
    }
    store_password(p, email, &pw)?;
    Ok(format!("{}: stored password for {email}", p.name()))
}

pub fn show_password(p: Prov, email: &str) -> Res {
    let name = p.name();
    match password(p, email) {
        None => Err(format!("{name}: no password stored for {email} (subs add {name} {email})")),
        Some(pw) if clip(&pw) => Ok(format!("{name}: password for {email} copied to clipboard")),
        Some(pw) => Ok(format!("{name}: password for {email}: {pw}")),
    }
}

pub fn rm(p: Prov, email: &str) -> Res {
    let mut a = store::accounts();
    if let Some(m) = a.get_mut(p.name()).and_then(Value::as_object_mut) {
        m.remove(email);
    }
    store::save_accounts(&a).map_err(io)?;
    let _ = fs::remove_file(snap_path(p, email));
    Ok(format!("{}: forgot {email}", p.name()))
}

pub struct Login {
    pub email: String,
    /// The new login is now the one this machine uses.
    pub live: bool,
    pub msg: String,
}

/// Sign in with the real CLI in a throwaway home, so the login in use is
/// never touched, then save the result. Signing in again as the account
/// that is already live replaces its token, since that is the point of it.
pub fn login(p: Prov, email: Option<&str>, use_it: bool) -> Result<Login, String> {
    let name = p.name();
    // A stored password goes on the clipboard, ready for the browser.
    if let Some(m) = email.filter(|e| password(p, e).is_some()).and_then(|e| show_password(p, e).ok()) {
        println!("{m}");
    }
    let dir = store::root().join(name);
    store::mkdir(&dir).map_err(io)?;
    for ent in fs::read_dir(&dir).map_err(io)?.flatten() {
        // left behind by a login that was killed hard
        let stale = ent.metadata().and_then(|m| m.modified()).is_ok_and(|t| {
            t.elapsed().unwrap_or_default() > Duration::from_secs(3600)
        });
        if stale && ent.file_name().to_string_lossy().starts_with(".login-") {
            let _ = fs::remove_dir_all(ent.path());
        }
    }
    // macOS keeps the default Claude login in the keychain under a name tied
    // to the config dir, so there it signs in in place and puts the old one back.
    let inplace = p == Prov::Claude && claude_keychain(None);
    let tmp = if inplace { None } else { Some(store::mkdtemp(&dir, ".login-").map_err(io)?) };
    let before = p.read(None);
    let ok = p.login(email, tmp.as_deref());
    let blob = p.read(tmp.as_deref());
    if let Some(t) = &tmp {
        let _ = fs::remove_dir_all(t);
    }
    let (blob, got) = blob
        .filter(|_| ok)
        .and_then(|b| p.email(&b).map(|g| (b, g)))
        .ok_or_else(|| format!("{name} login failed"))?;
    if email.is_some_and(|e| e != got) {
        println!("signed in as {got}, not {} · saved under {got}", email.unwrap_or_default());
    }
    wjson(&snap_path(p, &got), &blob).map_err(io)?;
    let again = before.as_ref().and_then(|b| p.email(b)).as_deref() == Some(got.as_str());
    let live = use_it || again;
    if inplace && !live {
        if let Some(b) = &before {
            p.write(b)?;
        }
    }
    let msg = match (live, inplace) {
        (false, _) => format!("{name}: saved {got}"),
        (true, true) => format!("{name}: {got} signed in and active"),
        (true, false) => activate(p, &got, true)?,
    };
    Ok(Login { email: got, live, msg })
}

pub fn clip(text: &str) -> bool {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let tools: [(&str, &[&str], bool); 3] = [
        ("wl-copy", &[], set("WAYLAND_DISPLAY")),
        ("xclip", &["-selection", "clipboard"], set("DISPLAY")),
        ("pbcopy", &[], true),
    ];
    for (cmd, args, usable) in tools {
        if !usable || !on_path(cmd) {
            continue;
        }
        let Ok(mut c) = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = c.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        return c.wait().is_ok_and(|s| s.success());
    }
    false
}

fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// Read a line from the terminal without echo.
pub fn read_secret(prompt: &str) -> String {
    eprint!("{prompt}");
    let _ = io::stderr().flush();
    let mut s = String::new();
    if terminal::enable_raw_mode().is_err() {
        let _ = io::stdin().lock().read_line(&mut s);
        return s.trim_end().to_string();
    }
    loop {
        let Ok(Event::Key(k)) = event::read() else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Enter => break,
            KeyCode::Backspace => {
                s.pop();
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = terminal::disable_raw_mode();
                eprintln!();
                std::process::exit(130);
            }
            KeyCode::Char(c) => s.push(c),
            _ => {}
        }
    }
    let _ = terminal::disable_raw_mode();
    eprintln!();
    s
}
