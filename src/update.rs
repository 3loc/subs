//! Keep each provider's CLI current across the hosts.
//!
//! Hosts mostly run these CLIs headless (T3, cron, ssh), where Claude's own
//! updater never runs, so they fall behind the models the providers ship.
//! Every push therefore brings the host's CLI up to at least this machine's
//! version, through the CLI's own updater; `subs update` first brings this
//! machine to the latest release. Nothing is installed where the CLI is
//! missing.

use crate::prov::Prov;
use crate::push::{self, Done};
use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// A native Claude download is ~350 MB; a slow link needs minutes.
const TIMEOUT: Duration = Duration::from_secs(600);

pub fn bin(p: Prov) -> &'static str {
    match p {
        Prov::Codex => "codex",
        Prov::Claude => "claude",
        Prov::Cursor => "cursor-agent",
    }
}

/// "2.1.325 (Claude Code)", "codex-cli 0.164.0", "2026.11.05-86047df":
/// the dotted number.
pub fn parse(s: &str) -> Option<String> {
    let line = s.lines().next()?;
    let v: String = line[line.find(|c: char| c.is_ascii_digit())?..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let v = v.trim_end_matches('.');
    v.contains('.').then(|| v.to_string())
}

/// This machine's CLI version, None when it is not installed.
pub fn local(p: Prov) -> Option<String> {
    if crate::demo::on() {
        return None;
    }
    let o = Command::new(bin(p))
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| parse(&String::from_utf8_lossy(&o.stdout)))
        .flatten()
}

const HEAD: &str = r#"PATH="$HOME/.local/bin:$PATH:/opt/homebrew/bin"
export PATH
v() { "$1" --version 2>/dev/null </dev/null | sed -n '1s/^[^0-9]*\([0-9][0-9]*\(\.[0-9][0-9]*\)*\).*/\1/p'; }
older() { awk -v a="$1" -v b="$2" 'BEGIN { n = split(a, x, "."); m = split(b, y, "."); if (m > n) n = m; for (i = 1; i <= n; i++) { if (x[i] + 0 < y[i] + 0) exit 0; if (x[i] + 0 > y[i] + 0) exit 1 } exit 1 }'; }
log=$(mktemp "${TMPDIR:-/tmp}/subs-update.XXXXXX") || exit 1
trap 'rm -f "$log"' EXIT
"#;

// Updaters read nothing from stdin: under `sh -s` it is the rest of this script.
const SECTION: &str = r#"(
b=$(command -v BIN 2>/dev/null) || { echo "subs-cli: NAME missing"; exit 0; }
have=$(v "$b")
if [ -n "$have" ] && ! older "$have" 'WANT'; then echo "subs-cli: NAME current $have"; exit 0; fi
if ! { UPDATE; } </dev/null >"$log" 2>&1; then
  echo "subs-cli: NAME failed $(tr '\r' '\n' <"$log" | grep -v '^[[:space:]]*$' | tail -n 1 | cut -c1-72)"
  exit 0
fi
now=$(v "$b")
if [ -z "$now" ] || older "$now" 'WANT'; then echo "subs-cli: NAME behind ${now:-unknown}"; exit 0; fi
echo "subs-cli: NAME updated ${have:-unknown} $now"
)
"#;

/// A script that brings each CLI up to at least its version.
fn script(want: &[(Prov, String)]) -> String {
    let mut s = HEAD.to_string();
    for (p, w) in want {
        // Native Claude takes an exact version, so a host matches this
        // machine; the others only know "latest".
        let up = match p {
            Prov::Claude => {
                r#"if [ -d "$HOME/.local/share/claude/versions" ]; then "$b" install 'WANT'; else "$b" update; fi"#
            }
            _ => r#""$b" update"#,
        };
        s += &SECTION
            .replace("UPDATE", up)
            .replace("WANT", w)
            .replace("NAME", p.name())
            .replace("BIN", bin(*p));
    }
    s
}

/// Text from a remote shell, safe for a terminal.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// What the script said for each CLI: one note each, and whether all are current.
fn read(out: &str, want: &[(Prov, String)]) -> (String, bool) {
    let mut notes = vec![];
    let mut ok = true;
    for (p, w) in want {
        let said = out
            .lines()
            .filter_map(|l| l.strip_prefix("subs-cli: "))
            .find_map(|l| l.strip_prefix(p.name())?.strip_prefix(' '));
        let words: Vec<&str> = said.unwrap_or("").splitn(2, ' ').collect();
        let (b, rest) = (bin(*p), words.get(1).map(|r| clean(r)).unwrap_or_default());
        notes.push(match words[0] {
            "current" => format!("{b} {rest}"),
            "missing" => format!("no {b}"),
            "updated" => {
                let (from, to) = rest.split_once(' ').unwrap_or(("?", &rest));
                format!("{b} {from} → {to}")
            }
            "behind" => {
                ok = false;
                format!("{b} still {rest} after its update, wanted {w}")
            }
            "failed" => {
                ok = false;
                format!("{b} update failed: {rest}")
            }
            _ => {
                ok = false;
                format!("{b} version not confirmed")
            }
        });
    }
    (notes.join(" · "), ok)
}

/// Bring `host`'s CLIs up to at least `want`.
pub fn host(host: &str, want: &[(Prov, String)]) -> Done {
    if want.is_empty() {
        return Done::Ok("no CLI to check".into());
    }
    match push::exec(host, "sh -s", &script(want), TIMEOUT, false) {
        Err(d) => d,
        Ok(out) => match read(&out, want) {
            (n, true) => Done::Ok(n),
            (n, false) => Done::Failed(n),
        },
    }
}

/// After a push: the host's CLI too. A changed version leads the line.
pub fn after_push(p: Prov, host_: &str, want: Option<&str>, pushed: Done) -> Done {
    let (Done::Ok(m), Some(w)) = (&pushed, want) else {
        return pushed;
    };
    match host(host_, &[(p, w.to_string())]) {
        Done::Ok(n) if n.contains('→') => Done::Ok(format!("{n} · {m}")),
        Done::Ok(n) => Done::Ok(format!("{m} · {n}")),
        Done::Failed(e) => Done::Failed(format!("credentials installed; {e}")),
        Done::Offline => Done::Failed("credentials installed; CLI version not checked".into()),
    }
}

/// Run the CLI's own updater here. (before, after) versions.
fn here(p: Prov) -> Result<(Option<String>, String), String> {
    let before = local(p);
    let out = Command::new(bin(p))
        .arg("update")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", bin(p)))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr) + String::from_utf8_lossy(&out.stdout);
        let last = text
            .split(['\n', '\r'])
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output");
        return Err(format!("{} update failed: {}", bin(p), clean(last.trim())));
    }
    let after = local(p).ok_or_else(|| format!("{} --version failed after its update", bin(p)))?;
    Ok((before, after))
}

/// `subs update`: the latest CLIs here, then at least those on every host.
/// False when an update failed.
pub fn cli(provs: &[Prov], local_only: bool, restart_t3: bool) -> bool {
    let named = provs.len() == 1;
    let mut good = true;
    let mut want: Vec<(Prov, String)> = vec![];
    for &p in provs {
        let b = bin(p);
        if local(p).is_none() {
            if named {
                eprintln!("subs: {b} is not installed here");
                good = false;
            }
            continue;
        }
        let tty = std::io::stdout().is_terminal();
        if tty {
            print!("{b}: checking for an update…");
            let _ = std::io::stdout().flush();
        }
        let r = here(p);
        if tty {
            print!("\r\x1b[K");
        }
        match r {
            Ok((before, after)) => {
                match before {
                    Some(b0) if b0 != after => println!("{b}: {b0} → {after}"),
                    _ => println!("{b}: {after} is the latest"),
                }
                want.push((p, after));
            }
            Err(e) => {
                eprintln!("subs: {e}");
                good = false;
                // Hosts still come up to what this machine has.
                if let Some(v) = local(p) {
                    want.push((p, v));
                }
            }
        }
    }
    if local_only || want.is_empty() {
        return push::finish_t3(good, restart_t3);
    }
    let takes: Vec<(Prov, Vec<String>)> = want.iter().map(|(p, _)| (*p, push::hosts(*p))).collect();
    let mut hosts: Vec<String> = vec![];
    for h in takes.iter().flat_map(|(_, hs)| hs) {
        if !hosts.contains(h) {
            hosts.push(h.clone());
        }
    }
    if hosts.is_empty() {
        println!(
            "no hosts to update · list them in {}",
            push::config().display()
        );
        return push::finish_t3(good, restart_t3);
    }
    println!();
    println!(
        "updating {} on {} host{}",
        want.iter()
            .map(|(p, _)| bin(*p))
            .collect::<Vec<_>>()
            .join(", "),
        hosts.len(),
        if hosts.len() == 1 { "" } else { "s" }
    );
    let (tx, rx) = std::sync::mpsc::channel();
    for (i, h) in hosts.iter().enumerate() {
        let w: Vec<(Prov, String)> = want
            .iter()
            .filter(|(p, _)| takes.iter().any(|(q, hs)| q == p && hs.contains(h)))
            .cloned()
            .collect();
        let (h, tx) = (h.clone(), tx.clone());
        thread::spawn(move || {
            let mut d = host(&h, &w);
            if restart_t3 {
                d = push::restart_t3_after(&h, d, "CLIs updated");
            }
            let _ = tx.send((i, d));
        });
    }
    let (ok, off, bad) = push::wait(&hosts, rx);
    println!();
    println!(
        "{}",
        push::tally("updated", ok, off.len(), bad, hosts.len())
    );
    if !off.is_empty() {
        println!("offline hosts catch up on the next: subs update");
    }
    if !restart_t3 {
        println!("T3 keeps the CLI version it saw at startup · use --restart-t3 to restart active T3 user services");
    }
    push::finish_t3(good && bad == 0, restart_t3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_finds_the_version() {
        assert_eq!(parse("2.1.325 (Claude Code)\n").as_deref(), Some("2.1.325"));
        assert_eq!(parse("codex-cli 0.164.0").as_deref(), Some("0.164.0"));
        assert_eq!(parse("2026.11.05-86047df").as_deref(), Some("2026.11.05"));
        assert_eq!(parse("command not found"), None);
        assert_eq!(parse("v12"), None);
    }

    #[test]
    fn read_names_each_outcome() {
        let want = [
            (Prov::Claude, "2.1.325".to_string()),
            (Prov::Codex, "0.164.0".to_string()),
        ];
        let out =
            "noise\nsubs-cli: claude updated 2.1.278 2.1.325\nsubs-cli: codex current 0.169.0\n";
        assert_eq!(
            read(out, &want),
            ("claude 2.1.278 → 2.1.325 · codex 0.169.0".into(), true)
        );
        let out = "subs-cli: claude failed \x1b[31mno network\nsubs-cli: codex missing\n";
        assert_eq!(
            read(out, &want),
            (
                "claude update failed: [31mno network · no codex".into(),
                false
            )
        );
        assert!(!read("", &want).1);
    }

    /// Runs the real script against fake CLIs in a scratch $HOME.
    fn run(fake: &str, want: &str) -> (String, String) {
        let tmp = crate::store::mkdtemp(&std::env::temp_dir(), "subs-update-").unwrap();
        let bin = tmp.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let f = bin.join("codex");
        std::fs::write(
            &f,
            fake.replace("STATE", &tmp.join("ver").display().to_string()),
        )
        .unwrap();
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut c = Command::new("sh")
            .arg("-s")
            .env("HOME", &tmp)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let s = script(&[(Prov::Codex, want.to_string())]) + "echo subs-cli: end\n";
        c.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        let out = String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap();
        let ver = std::fs::read_to_string(tmp.join("ver")).unwrap_or_default();
        std::fs::remove_dir_all(tmp).unwrap();
        (out, ver)
    }

    // A fake CLI: --version prints the saved version (0.9.2 at first), update
    // saves 0.10.0 and tries to swallow the rest of the script from stdin.
    const FAKE: &str = "#!/bin/sh\ncase $1 in\n--version) if [ -f STATE ]; then cat STATE; else echo 'codex-cli 0.9.2'; fi ;;\nupdate) cat >/dev/null; echo 'codex-cli 0.10.0' > STATE ;;\nesac\n";

    #[test]
    fn script_updates_an_older_cli_numerically() {
        let (out, ver) = run(FAKE, "0.10.0");
        assert!(
            out.contains("subs-cli: codex updated 0.9.2 0.10.0"),
            "{out}"
        );
        assert!(
            out.contains("subs-cli: end"),
            "the updater ate the script: {out}"
        );
        assert_eq!(ver.trim(), "codex-cli 0.10.0");
    }

    #[test]
    fn script_leaves_a_current_cli_alone() {
        let (out, ver) = run(FAKE, "0.9.1");
        assert!(out.contains("subs-cli: codex current 0.9.2"), "{out}");
        assert!(ver.is_empty(), "updated a current CLI");
    }

    #[test]
    fn script_reports_an_update_that_does_not_reach_the_version() {
        let (out, _) = run(FAKE, "0.11.0");
        assert!(out.contains("subs-cli: codex behind 0.10.0"), "{out}");
    }

    #[test]
    fn script_reports_a_failed_update() {
        let failing = "#!/bin/sh\n[ \"$1\" = --version ] && echo 'codex-cli 0.1.0' && exit 0\nprintf 'downloading 50%%\\rnetwork unreachable\\n' >&2\nexit 1\n";
        let (out, _) = run(failing, "0.2.0");
        assert!(
            out.contains("subs-cli: codex failed network unreachable"),
            "{out}"
        );
        // No readable version counts as older, and the update is tried.
        let (out, _) = run("#!/bin/sh\nexit 127\n", "0.2.0");
        assert!(out.contains("subs-cli: codex failed"), "{out}");
    }
}
