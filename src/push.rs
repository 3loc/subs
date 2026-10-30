//! Fan the active login out to other machines over ssh.
//!
//! The machines are listed in ~/.config/subs/hosts ($SUBS_HOSTS overrides),
//! one ssh host per line, optionally followed by the providers it takes:
//!
//!     desktop
//!     laptop   claude      # only Claude goes here
//!
//! Each push is a small POSIX sh script on the host's stdin with the
//! credential in a heredoc, so it never shows up in `ps` and the host needs
//! nothing but sh. A host that does not answer is offline, not failed.

use crate::prov::Prov;
use crate::store::{env_path, home};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(45);
const KEYCHAIN: &str = "Claude Code-credentials";

pub enum Done {
    Ok(String),
    Offline,
    Failed(String),
}

pub fn config() -> PathBuf {
    env_path("SUBS_HOSTS").unwrap_or_else(|| {
        env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")).join("subs/hosts")
    })
}

/// The hosts that take `p`'s login, minus this machine.
pub fn hosts(p: Prov) -> Vec<String> {
    if crate::demo::on() {
        return crate::demo::HOSTS.map(String::from).to_vec();
    }
    let Ok(text) = std::fs::read_to_string(config()) else { return vec![] };
    let me = local_names();
    let mut out: Vec<String> = vec![];
    for line in text.lines() {
        let mut words = line.split('#').next().unwrap_or("").split_whitespace();
        let Some(host) = words.next() else { continue };
        let only: Vec<&str> = words.collect();
        if (only.is_empty() || only.contains(&p.name())) && !me.iter().any(|m| m == host) && !out.iter().any(|h| h == host) {
            out.push(host.to_string());
        }
    }
    out
}

fn local_names() -> Vec<String> {
    let mut v = vec!["localhost".to_string()];
    if let Ok(o) = Command::new("hostname").output() {
        let h = String::from_utf8_lossy(&o.stdout).trim().to_string();
        v.push(h.split('.').next().unwrap_or_default().to_string());
        v.push(h);
    }
    v
}

/// Push `blob` to every host in parallel; each result goes to `tx` as it lands.
pub fn start(p: Prov, hosts: &[String], blob: Value, tx: Sender<(usize, Done)>) {
    if crate::demo::on() {
        return crate::demo::start(tx);
    }
    for (i, host) in hosts.iter().enumerate() {
        let (host, blob, tx) = (host.clone(), blob.clone(), tx.clone());
        thread::spawn(move || {
            let _ = tx.send((i, push(p, &host, &blob)));
        });
    }
}

const SPLIT: &str = "\nSUBS_SPLIT\n";

pub fn push(p: Prov, host: &str, blob: &Value) -> Done {
    // Read what the host has first: a newer token for the same login stays,
    // since pushing an older copy over it can undo a refresh it already did.
    let read = match p {
        Prov::Bifrost => return Done::Failed("Bifrost is read-only".into()),
        Prov::Cursor => {
            r#"if [ "$(uname -s)" = Darwin ]; then cat "$HOME/.cursor/auth.json"; else cat "$HOME/.config/cursor/auth.json"; fi 2>/dev/null; true"#
        }
        Prov::Codex => "cat \"$HOME/.codex/auth.json\" 2>/dev/null; true",
        Prov::Claude => "cat \"$HOME/.claude/.credentials.json\" 2>/dev/null; printf '\\nSUBS_SPLIT\\n'; cat \"$HOME/.claude.json\" 2>/dev/null; true",
    };
    let have = match ssh(host, read, "") {
        Ok(out) => out,
        Err(d) => return d,
    };
    let (cred, state) = have.split_once(SPLIT).unwrap_or((&have, ""));
    let theirs = serde_json::from_str::<Value>(cred).ok().map(|c| match p {
        Prov::Bifrost => Value::Null,
        Prov::Codex => c,
        Prov::Cursor => json!({"credentials": c}),
        Prov::Claude => {
            let acct = serde_json::from_str::<Value>(state).map(|s| s["oauthAccount"].clone()).unwrap_or_default();
            json!({"credentials": c, "account": acct})
        }
    });
    let script = match theirs {
        Some(t) if keep_theirs(p, &t, blob) => {
            return Done::Ok("has a newer token, kept".into());
        }
        _ => match p {
            Prov::Bifrost => unreachable!("read-only provider"),
            Prov::Codex => codex_script(blob),
            Prov::Claude => claude_script(blob, state),
            Prov::Cursor => cursor_script(blob),
        },
    };
    match ssh(host, "sh -s", &script) {
        Err(d) => d,
        Ok(out) => match out.lines().rev().find_map(|l| l.strip_prefix("subs: ")) {
            Some(r) => Done::Ok(r.trim().to_string()),
            None => Done::Failed("no answer from the host's shell".into()),
        },
    }
}

/// The host already has this login with a token at least as new as ours.
fn keep_theirs(p: Prov, theirs: &Value, ours: &Value) -> bool {
    if p == Prov::Cursor && (p.expires(theirs).is_none() || p.expires(ours).is_none()) {
        return false;
    }
    let workspace = |v: &Value| v["tokens"]["account_id"].clone();
    let same = p.email(theirs).is_some()
        && p.email(theirs) == p.email(ours)
        && (p == Prov::Claude || workspace(theirs) == workspace(ours));
    same && theirs != ours && p.expires(theirs) >= p.expires(ours)
}

fn ssh(host: &str, cmd: &str, input: &str) -> Result<String, Done> {
    let mut c = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=6", "-o", "RemoteCommand=none", "-T", host, cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Done::Failed(format!("ssh: {e}")))?;
    let (mut stdin, mut so, mut se) = (c.stdin.take().unwrap(), c.stdout.take().unwrap(), c.stderr.take().unwrap());
    let input = input.to_string();
    let w = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let ro = thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let re = thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match c.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = c.kill();
                let _ = c.wait();
                break None;
            }
        }
    };
    let _ = w.join();
    let out = ro.join().unwrap_or_default();
    let err = re.join().unwrap_or_default();
    let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    match status {
        None => Err(Done::Failed("timed out".into())),
        Some(s) if s.code() == Some(255) => {
            let down = ["timed out", "No route", "refused", "unreachable", "Network is down"];
            Err(if down.iter().any(|d| err.contains(d)) {
                Done::Offline
            } else {
                Done::Failed(last.trim_start_matches("ssh: ").to_string())
            })
        }
        Some(s) if !s.success() => Err(Done::Failed(if last.is_empty() { format!("exit {}", s.code().unwrap_or(-1)) } else { last })),
        Some(_) => Ok(out),
    }
}

/// `cat > FILE` of `body`, under a delimiter the body cannot contain.
fn heredoc(file: &str, body: &str) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let delim = format!("SUBS_{nanos:x}");
    format!("cat > \"{file}\" <<'{delim}'\n{body}\n{delim}\n")
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn codex_script(blob: &Value) -> String {
    format!(
        r#"set -eu
umask 077
d="$HOME/.codex"
mkdir -p "$d"
t=$(mktemp "$d/.auth.json.subs.XXXXXX")
trap 'rm -f "$t"' EXIT
{payload}if [ -f "$d/auth.json" ] && cmp -s "$t" "$d/auth.json"; then
  echo "subs: unchanged"; exit 0
fi
mv -f "$t" "$d/auth.json"
# app-server daemons hold the old token and would write it back; they respawn on demand
pkill -TERM -f 'codex app-server --listen' 2>/dev/null || true
n=$(ps -U "$(id -u)" -o args= | grep -E '(^|/)codex( |$)' | grep -vc app-server || true)
if [ "$n" -gt 0 ]; then
  echo "subs: updated · restart $n codex session(s)"
else
  echo "subs: updated"
fi
"#,
        payload = heredoc("$t", &pretty(blob))
    )
}

fn cursor_script(blob: &Value) -> String {
    format!(
        r#"set -eu
umask 077
if [ "$(uname -s)" = Darwin ]; then d="$HOME/.cursor"; else d="$HOME/.config/cursor"; fi
mkdir -p "$d"
t=$(mktemp "$d/.auth.json.subs.XXXXXX")
trap 'rm -f "$t"' EXIT
{payload}chmod 600 "$t"
if [ -f "$d/auth.json" ] && cmp -s "$t" "$d/auth.json"; then
  echo "subs: unchanged"; exit 0
fi
mv -f "$t" "$d/auth.json"
echo "subs: updated"
"#,
        payload = heredoc("$t", &pretty(&blob["credentials"]))
    )
}

fn claude_script(blob: &Value, state: &str) -> String {
    // ~/.claude.json names the account; only rewrite it when that changes,
    // and never when it is there but unreadable.
    let state = if state.trim().is_empty() { Some(json!({})) } else { serde_json::from_str::<Value>(state).ok() };
    let merged = state.as_ref().filter(|s| s.is_object()).and_then(|s| {
        let mut m = s.clone();
        // Claude keeps its own richer copy of the account; only replace it
        // when it names someone else.
        if m["oauthAccount"]["emailAddress"] != blob["account"]["emailAddress"] {
            m["oauthAccount"] = blob["account"].clone();
        }
        m["hasCompletedOnboarding"] = json!(true);
        (m != *s).then_some(m)
    });
    let state_block = match merged {
        None => String::new(),
        Some(m) => format!(
            "s=$(mktemp \"$HOME/.claude.json.subs.XXXXXX\")\n{}mv -f \"$s\" \"$HOME/.claude.json\"\ns=\nr=updated\n",
            heredoc("$s", &pretty(&m))
        ),
    };
    format!(
        r#"set -eu
umask 077
mkdir -p "$HOME/.claude"
s=
t=$(mktemp "$HOME/.claude/.credentials.json.subs.XXXXXX")
trap 'rm -f "$t" ${{s:+"$s"}}' EXIT
{payload}r=updated
if [ -f "$HOME/.claude/.credentials.json" ] && cmp -s "$t" "$HOME/.claude/.credentials.json"; then
  r=unchanged
else
  mv -f "$t" "$HOME/.claude/.credentials.json"
fi
{state_block}if [ "$(uname -s)" = Darwin ]; then
  # Claude reads the keychain before the file, and ssh cannot write the
  # keychain, only delete from it: clear the item so the file wins.
  n=0
  while [ "$n" -lt 8 ] && security find-generic-password -s "{KEYCHAIN}" >/dev/null 2>&1; do
    security delete-generic-password -s "{KEYCHAIN}" >/dev/null 2>&1 || break
    n=$((n + 1))
  done
  if security find-generic-password -s "{KEYCHAIN}" >/dev/null 2>&1; then
    echo "an old keychain login still wins; on that Mac run: security delete-generic-password -s '{KEYCHAIN}'" >&2
    exit 3
  fi
  if [ "$n" -gt 0 ]; then r="updated · cleared old keychain login"; fi
fi
echo "subs: $r"
"#,
        payload = heredoc("$t", &pretty(&blob["credentials"]))
    )
}

/// Run a push from the command line, one line per host as they finish.
/// False when a reachable host refused it.
pub fn cli(p: Prov) -> bool {
    let hosts = hosts(p);
    if hosts.is_empty() {
        println!("{}: no hosts to push to · list them in {}", p.name(), config().display());
        return true;
    }
    let blob = if crate::demo::on() {
        serde_json::json!({"demo": true})
    } else {
        let Some(b) = p.read(None) else {
            eprintln!("subs: no live {} login to push", p.name());
            return false;
        };
        b
    };
    let email = if crate::demo::on() { crate::demo::ACTIVE.lock().unwrap().to_string() } else { p.email(&blob).unwrap_or_default() };
    let color = std::io::IsTerminal::is_terminal(&std::io::stdout()) && std::env::var_os("NO_COLOR").is_none();
    let paint = |code: &str, s: &str| if color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() };
    println!("{}: pushing {email} to {} host{}", p.name(), hosts.len(), if hosts.len() == 1 { "" } else { "s" });
    let (tx, rx) = std::sync::mpsc::channel();
    start(p, &hosts, blob, tx);
    let w = hosts.iter().map(String::len).max().unwrap_or(0);
    let (mut ok, mut off, mut bad) = (0, vec![], 0);
    // While hosts answer, one live line names who is still being waited on.
    let live = color && std::io::IsTerminal::is_terminal(&std::io::stdout());
    let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut waiting: Vec<bool> = vec![true; hosts.len()];
    let started = Instant::now();
    let mut got = 0;
    while got < hosts.len() {
        if live {
            let names: Vec<&str> = hosts.iter().zip(&waiting).filter(|(_, w)| **w).map(|(h, _)| h.as_str()).collect();
            let frame = spin[(started.elapsed().as_millis() / 80) as usize % spin.len()];
            print!("\r\x1b[K  {} {}", paint("36", frame), paint("90", &format!("waiting for {}", names.join(", "))));
            let _ = std::io::stdout().flush();
        }
        let Ok((i, d)) = rx.recv_timeout(Duration::from_millis(80)) else { continue };
        got += 1;
        waiting[i] = false;
        if live {
            print!("\r\x1b[K");
        }
        let h = format!("{:<w$}", hosts[i]);
        match d {
            Done::Ok(m) => {
                ok += 1;
                println!("  {} {h}  {}", paint("32", "✓"), paint("90", &m));
            }
            Done::Offline => {
                off.push(hosts[i].clone());
                println!("  {} {h}  {}", paint("90", "○"), paint("90", "offline"));
            }
            Done::Failed(m) => {
                bad += 1;
                println!("  {} {h}  {}", paint("31", "✗"), paint("31", &m));
            }
        }
    }
    println!();
    println!("{}", summary(ok, off.len(), bad, hosts.len()));
    if !off.is_empty() {
        println!("offline hosts get it on the next: subs push {}", p.name());
    }
    bad == 0
}

pub fn summary(ok: usize, off: usize, bad: usize, n: usize) -> String {
    let mut s = format!("pushed to {ok} of {n}");
    if off > 0 {
        s += &format!(" · {off} offline");
    }
    if bad > 0 {
        s += &format!(" · {bad} failed");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex(email: &str, exp: u64, ws: &str) -> Value {
        let b64 = |j: String| {
            const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let (mut out, bytes) = (String::new(), j.into_bytes());
            for c in bytes.chunks(3) {
                let n = c.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
                for i in 0..=c.len() {
                    out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                }
            }
            out
        };
        let t = b64(format!(r#"{{"email":"{email}","exp":{exp}}}"#));
        json!({"tokens": {"id_token": format!("h.{t}.s"), "access_token": format!("h.{t}.s"), "account_id": ws}})
    }

    #[test]
    fn cursor_keeps_only_identifiable_newer_tokens() {
        let cursor = |email, exp| json!({"credentials": {"accessToken": codex(email, exp, "w")["tokens"]["access_token"]}});
        assert!(keep_theirs(Prov::Cursor, &cursor("a@x", 200), &cursor("a@x", 100)));
        assert!(!keep_theirs(Prov::Cursor, &cursor("b@x", 200), &cursor("a@x", 100)));
        let opaque = json!({"credentials": {"accessToken": "synthetic"}, "account": {"email": "a@x"}});
        assert!(!keep_theirs(Prov::Cursor, &opaque, &cursor("a@x", 100)));
    }

    #[test]
    fn cursor_script_installs_only_auth_atomically_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::store::mkdtemp(&std::env::temp_dir(), "subs-cursor-push-").unwrap();
        let blob = json!({"credentials": {"accessToken": "synthetic", "refreshToken": "synthetic"}, "account": {"email": "a@x"}});
        let script = cursor_script(&blob);
        assert!(!script.contains("pkill"));
        for expected in ["subs: updated", "subs: unchanged"] {
            let mut child = Command::new("sh").arg("-s").env("HOME", &tmp)
                .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), expected);
        }
        let path = tmp.join(if crate::prov::MAC { ".cursor/auth.json" } else { ".config/cursor/auth.json" });
        assert_eq!(crate::store::rjson(&path).unwrap(), blob["credentials"]);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn newer_token_for_the_same_login_stays() {
        let p = Prov::Codex;
        assert!(keep_theirs(p, &codex("a@x", 200, "w"), &codex("a@x", 100, "w")));
        assert!(!keep_theirs(p, &codex("a@x", 100, "w"), &codex("a@x", 200, "w")));
        assert!(!keep_theirs(p, &codex("b@x", 900, "w"), &codex("a@x", 100, "w")));
        assert!(!keep_theirs(p, &codex("a@x", 900, "v"), &codex("a@x", 100, "w")));
        assert!(!keep_theirs(p, &codex("a@x", 100, "w"), &codex("a@x", 100, "w")));
    }

    #[test]
    fn hosts_file_filters_providers_and_self() {
        let dir = std::env::temp_dir().join(format!("subs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("hosts");
        std::fs::write(&f, "# fleet\ndesktop\nlaptop claude   # mac\n\nlocalhost\ndesktop\n").unwrap();
        std::env::set_var("SUBS_HOSTS", &f);
        assert_eq!(hosts(Prov::Codex), ["desktop"]);
        assert_eq!(hosts(Prov::Claude), ["desktop", "laptop"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
