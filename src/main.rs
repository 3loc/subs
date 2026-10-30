//! subs - see and switch ChatGPT/Codex, Claude, and Cursor subscription logins.
//!
//! State lives in $SUBS_HOME (default ${XDG_DATA_HOME:-~/.local/share}/subs), mode 0700:
//!
//!     accounts.json        {provider: {email: {"password": ...}}}
//!     codex/EMAIL.json     a saved ~/.codex/auth.json
//!     claude/EMAIL.json    a saved Claude credential + oauthAccount
//!     cursor/EMAIL.json    a saved Cursor credential + account email
//!
//! Logins run in a throwaway provider config directory (Cursor: XDG/HOME), so signing in to
//! a new account never touches - or revokes - the one in use. `use` swaps the
//! saved credential in, saving the current one first. Tokens are never
//! refreshed here: a refresh rotates the token and kills every other copy, so
//! that stays the job of the real CLI, which runs against the live file.

mod actions;
mod demo;
mod prov;
mod push;
mod status;
mod store;
mod tui;
mod view;

use prov::Prov;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::exit;
use std::time::Duration;

const HELP: &str = "\
usage: subs [command]

  subs                                    status of every account
  subs tui                                interactive view
  subs login codex|claude|cursor [EMAIL]  sign in; --use also makes it active
  subs use   codex|claude|cursor EMAIL    make a saved login the active one and push
                                          it to your hosts (--local: just this machine)
  subs push  codex|claude|cursor          push the active login to your hosts again
  subs add   codex|claude|cursor EMAIL    store the account's password
  subs pass  codex|claude|cursor EMAIL    copy (or print) that password
  subs rm    codex|claude|cursor EMAIL    forget an account
  subs json                               status as JSON

Hosts: one ssh host per line in ~/.config/subs/hosts, optionally followed by
the providers it takes (\"laptop claude\"). Offline hosts are skipped.";

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("subs: {msg}");
    exit(1)
}

/// Fetch status, with a spinner on stderr while the providers answer.
fn fetch() -> Vec<status::Row> {
    if !io::stderr().is_terminal() {
        return status::status();
    }
    let job = std::thread::spawn(status::status);
    let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut i = 0;
    while !job.is_finished() {
        eprint!("\r\x1b[36m{}\x1b[0m \x1b[90mfetching usage\x1b[0m", spin[i % spin.len()]);
        let _ = io::stderr().flush();
        std::thread::sleep(Duration::from_millis(80));
        i += 1;
    }
    eprint!("\r\x1b[K");
    job.join().unwrap_or_default()
}

fn cmd_status() {
    let rows = fetch();
    let out = io::stdout();
    let color = out.is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let cols = ratatui::crossterm::terminal::size().map_or(80, |(w, _)| w as usize);
    let b = view::body(&rows, None, cols.saturating_sub(2).min(view::MAX));
    let mut out = out.lock();
    for l in &b.lines {
        let _ = writeln!(out, "{}", view::ansi(l, color));
    }
}

fn report(r: actions::Res) {
    match r {
        Ok(m) => println!("{m}"),
        Err(m) => die(m),
    }
}

fn main() {
    let root = store::root();
    if let Err(e) = store::mkdir(&root) {
        die(format!("{}: {e}", root.display()));
    }
    // it holds passwords and live tokens
    let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().map_or("status", String::as_str);
    match cmd {
        "-h" | "--help" | "help" => return println!("{HELP}"),
        "-V" | "--version" => return println!("subs {}", env!("CARGO_PKG_VERSION")),
        "status" => return cmd_status(),
        "json" => return println!("{:#}", status::to_json(&fetch())),
        "tui" => {
            if let Err(e) = tui::run() {
                die(e)
            }
            return;
        }
        _ => {}
    }
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    let Some(p) = args.first().and_then(|a| Prov::parse(a)) else {
        die(format!("usage: subs {cmd} codex|claude|cursor EMAIL (subs help)"))
    };
    let email = args.get(1).copied();
    let flag = |f: &str| argv.iter().any(|a| a == f);
    let fan = |ok: bool| {
        if !ok || flag("--local") {
            return;
        }
        if !push::cli(p) {
            exit(1)
        }
    };
    match cmd {
        "login" => {
            let l = actions::login(p, email, flag("--use")).unwrap_or_else(|e| die(e));
            println!("{}", l.msg);
            if !l.live {
                println!("make it the active login with: subs use {} {}", p.name(), l.email);
            } else if !flag("--local") {
                println!();
            }
            return fan(l.live);
        }
        "push" => return fan(true),
        _ => {}
    }
    let Some(email) = email else { die(format!("usage: subs {cmd} {} EMAIL", p.name())) };
    if cmd == "use" {
        let r = actions::use_(p, email);
        let switched = r.as_ref().is_ok_and(|m| !m.ends_with("is already active"));
        report(r);
        if switched && !flag("--local") {
            println!();
        }
        return fan(switched);
    }
    report(match cmd {
        "add" => actions::add(p, email),
        "pass" => actions::show_password(p, email),
        "rm" => actions::rm(p, email),
        _ => die(format!("unknown command {cmd} (subs help)")),
    })
}
