//! Prove that Claude works with the managed setup-token: on each host, one
//! small model request from a fresh shell, and the running T3 server started
//! with that same token.
//!
//! The script carries the expected token in a heredoc on stdin, like a push,
//! and answers only in fixed status words. No token, environment or provider
//! text ever comes back. Saved OAuth logins are not checked: a request can
//! refresh, and so rotate, a login that other machines share.

use crate::prov::{claude, Prov};
use crate::push::{self, Done};
use std::thread;
use std::time::Duration;

/// Claude's start-up plus one Haiku reply, on a slow host.
const TIMEOUT: Duration = Duration::from_secs(150);

/// What a T3 server that still holds another token means.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum T3 {
    /// T3 was restarted to take the token, or is being checked: it must have it.
    Need,
    /// T3 was not restarted, so an older token there is expected: only noted.
    Note,
    /// T3 restarts later (this machine, from inside T3): not looked at now.
    Skip,
}

const SCRIPT: &str = r#"set +xv
set -u
umask 077
PATH="$HOME/.local/bin:$PATH:/opt/homebrew/bin:/usr/local/bin"
export PATH
unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL CLAUDE_CODE_USE_BEDROCK CLAUDE_CODE_USE_VERTEX CLAUDE_CODE_OAUTH_TOKEN CLAUDECODE
w=$(mktemp "${TMPDIR:-/tmp}/subs-check.XXXXXX") || exit 1
h=$(mktemp "${TMPDIR:-/tmp}/subs-check.XXXXXX") || { rm -f "$w"; exit 1; }
o=$(mktemp "${TMPDIR:-/tmp}/subs-check.XXXXXX") || { rm -f "$w" "$h"; exit 1; }
trap 'rm -f "$w" "$h" "$o"' EXIT
WANT
# What a new shell gets: the managed hook, nothing inherited.
hook="${XDG_CONFIG_HOME:-$HOME/.config}/subs/claude-shell.sh"
set +u
# shellcheck disable=SC1090
if [ -f "$hook" ]; then . "$hook" </dev/null 2>/dev/null; fi
set -u
if [ -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then echo 'subs-check: token absent'; exit 0; fi
printf '%s\n' "$CLAUDE_CODE_OAUTH_TOKEN" > "$h"
if ! cmp -s "$w" "$h"; then echo 'subs-check: token differs'; exit 0; fi
echo 'subs-check: token same'
if ! command -v claude >/dev/null 2>&1; then
  echo 'subs-check: model missing'
elif (cd "$HOME" && printf 'Reply with the single word ok.\n' | claude -p --tools '' --model haiku --strict-mcp-config --no-session-persistence) >"$o" 2>&1 && [ -s "$o" ]; then
  echo 'subs-check: model ok'
elif grep -Eqi 'not authenticated|authentication_error|invalid (access )?token|token has expired|oauth session expired|login required|sign in|/login|401' "$o"; then
  echo 'subs-check: model auth'
elif grep -Eqi 'usage limit|rate.?limit|hit your limit|out of extra usage|credit balance|429' "$o"; then
  echo 'subs-check: model limited'
else
  echo 'subs-check: model failed'
fi
tries=TRIES
[ "$tries" -gt 0 ] || exit 0
envof() {
  if [ -r "/proc/$1/environ" ]; then
    tr '\0' '\n' < "/proc/$1/environ"
  else
    ps -E -ww -p "$1" -o command= 2>/dev/null | tr ' ' '\n'
  fi
}
# Running T3 servers: the systemd user service, or `t3 serve` on a Mac.
servers() {
  if [ "$(uname -s)" = Darwin ]; then
    ps -x -o pid=,command= | awk '{ n = split($2, p, "/"); if (p[n] == "t3" && $3 == "serve") print $1 }'
  elif command -v systemctl >/dev/null 2>&1 && systemctl --user is-active --quiet t3code.service </dev/null 2>/dev/null; then
    systemctl --user show t3code.service -p MainPID --value </dev/null 2>/dev/null | grep -v '^0$'
  fi
}
# A server that has just restarted may still be its start-up shell.
n=0
while :; do
  pids=$(servers)
  st=
  for pid in $pids; do
    envof "$pid" | sed -n 's/^CLAUDE_CODE_OAUTH_TOKEN=//p' > "$h"
    if envof "$pid" | grep -q '^ANTHROPIC_API_KEY='; then st=apikey
    elif [ ! -s "$h" ]; then st=missing
    elif ! cmp -s "$w" "$h"; then st=stale
    fi
    [ -z "$st" ] || break
  done
  n=$((n + 1))
  if [ -n "$pids" ] && [ -z "$st" ]; then break; fi
  [ "$n" -lt "$tries" ] || break
  sleep 1
done
if [ -z "$pids" ]; then echo 'subs-check: t3 none'; else echo "subs-check: t3 ${st:-current}"; fi
"#;

/// `fresh`: T3 has just been restarted, so give it a moment to come up.
fn script(token: &claude::Token, t3: T3, fresh: bool) -> String {
    let tries = match (t3, fresh) {
        (T3::Skip, _) => 0,
        (_, true) => 15,
        (_, false) => 1,
    };
    SCRIPT
        .replace("WANT\n", &token.heredoc("$w"))
        .replace("TRIES", &tries.to_string())
}

/// What the script said, as one host line. Only the fixed words count.
fn read(out: &str, t3: T3) -> Done {
    let said = |what: &str| {
        out.lines()
            .filter_map(|l| l.strip_prefix("subs-check: "))
            .find_map(|l| l.strip_prefix(what)?.strip_prefix(' '))
            .unwrap_or("")
    };
    let fail = |m: &str| Done::Failed(m.to_string());
    match said("token") {
        "same" => {}
        "differs" => return fail("has a different Claude token · subs push claude"),
        "absent" => return fail("no Claude subscription token · subs push claude"),
        _ => return fail("Claude check did not finish"),
    }
    let mut notes = vec![];
    match said("model") {
        "ok" => notes.push("Claude replies"),
        "missing" => notes.push("no claude CLI to ask"),
        "auth" => return fail("Claude rejected the token · subs login claude EMAIL --token"),
        "limited" => return fail("Claude accepts the token, but its usage limit is reached"),
        "failed" => return fail("Claude request failed"),
        _ => return fail("Claude request not confirmed"),
    }
    let problem = match said("t3") {
        "current" => {
            notes.push("T3 has the token");
            None
        }
        "none" => None,
        _ if t3 == T3::Skip => None,
        "stale" => Some("T3 still runs an older token"),
        "missing" => Some("T3 runs without the token; its startup must load subs/claude-shell.sh"),
        "apikey" => Some("T3 has ANTHROPIC_API_KEY set, which overrides the subscription"),
        _ => Some("T3 token not confirmed"),
    };
    match problem {
        None => Done::Ok(notes.join(" · ")),
        Some(p) if t3 == T3::Need => Done::Failed(format!("{} · {p}", notes.join(" · "))),
        Some(p) => {
            notes.push(p);
            notes.push("--restart-t3 restarts it");
            Done::Ok(notes.join(" · "))
        }
    }
}

fn token() -> Result<claude::Token, String> {
    claude::load_token()?.ok_or_else(|| "Claude setup-token is no longer installed".to_string())
}

/// Check `host` over ssh.
pub fn host(host: &str, t3: T3, fresh: bool) -> Done {
    let token = match token() {
        Ok(t) => t,
        Err(e) => return Done::Failed(e),
    };
    match push::exec(host, "sh -s", &script(&token, t3, fresh), TIMEOUT, true) {
        Ok(out) => read(&out, t3),
        Err(d) => d,
    }
}

/// Check this machine, then record the account's limits for its card.
pub fn here(t3: T3, fresh: bool) -> Done {
    let token = match token() {
        Ok(t) => t,
        Err(e) => return Done::Failed(e),
    };
    let done = match push::exec_here(&script(&token, t3, fresh), TIMEOUT) {
        Ok(out) => read(&out, t3),
        Err(d) => d,
    };
    let Done::Ok(m) = done else { return done };
    match claude::observe(&token) {
        Ok(w) if !w.is_empty() => Done::Ok(format!("{m} · {}", limits(&w))),
        // The machine works; only the limits are unknown.
        _ => Done::Ok(format!("{m} · limits not reported")),
    }
}

/// "7d 2% used · Fable 7d 0% used", and a used-up window first.
fn limits(w: &[crate::prov::Win]) -> String {
    w.iter()
        .map(|w| match w.pct {
            Some(p) if p >= 100.0 => format!(
                "{} used up, resets in {}",
                w.label,
                w.reset.map_or("?".into(), crate::status::until)
            ),
            Some(p) => format!("{} {p:.0}% used", w.label),
            None => format!("{} unknown", w.label),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// After a push to `host` succeeded, prove it took.
pub fn after_push(h: &str, done: Done, t3: T3) -> Done {
    let Done::Ok(m) = done else { return done };
    let fresh = m.contains(push::T3_RESTARTED);
    match host(h, t3, fresh) {
        Done::Ok(n) => Done::Ok(format!("{m} · {n}")),
        Done::Failed(e) => Done::Failed(format!("credentials installed; {e}")),
        Done::Offline => {
            Done::Failed("credentials installed; went offline before the check".into())
        }
    }
}

/// `subs check claude`: this machine, then every host, in parallel.
/// False when a reachable machine fails.
pub fn cli(local_only: bool) -> bool {
    let token = match claude::load_token() {
        Ok(Some(t)) => t,
        Ok(None) => {
            eprintln!("subs: no Claude subscription token to check · subs login claude EMAIL --token (a saved OAuth login is not checked: a request could rotate it)");
            return false;
        }
        Err(e) => {
            eprintln!("subs: {e}");
            return false;
        }
    };
    let who = token
        .email()
        .map_or("the subscription token".into(), |e| format!("{e} [token]"));
    drop(token);
    let mut names = vec![push::here_name()];
    if !local_only {
        names.extend(push::hosts(Prov::Claude));
    }
    let n = names.len() - 1;
    println!(
        "claude: checking {who} here{}",
        match n {
            0 => String::new(),
            1 => " and on 1 host".into(),
            n => format!(" and on {n} hosts"),
        }
    );
    let (tx, rx) = std::sync::mpsc::channel();
    for (i, h) in names.iter().enumerate() {
        let (h, tx) = (h.clone(), tx.clone());
        thread::spawn(move || {
            let d = if i == 0 {
                here(T3::Need, false)
            } else {
                host(&h, T3::Need, false)
            };
            let _ = tx.send((i, d));
        });
    }
    let (ok, off, bad) = push::wait(&names, rx);
    println!();
    println!(
        "{}",
        push::tally("working on", ok, off.len(), bad, names.len())
    );
    if !off.is_empty() {
        println!("offline hosts get the token on the next: subs push claude");
    }
    bad == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(token: &str, model: &str, t3: &str) -> String {
        format!("subs-check: token {token}\nsubs-check: model {model}\nsubs-check: t3 {t3}\n")
    }

    fn ok(d: Done) -> String {
        match d {
            Done::Ok(m) => m,
            Done::Failed(m) => panic!("failed: {m}"),
            Done::Offline => panic!("offline"),
        }
    }

    fn failed(d: Done) -> String {
        match d {
            Done::Failed(m) => m,
            Done::Ok(m) => panic!("ok: {m}"),
            Done::Offline => panic!("offline"),
        }
    }

    #[test]
    fn a_working_host_and_its_t3() {
        assert_eq!(
            ok(read(&out("same", "ok", "current"), T3::Need)),
            "Claude replies · T3 has the token"
        );
        assert_eq!(
            ok(read(&out("same", "ok", "none"), T3::Need)),
            "Claude replies"
        );
        assert_eq!(
            ok(read(&out("same", "missing", "none"), T3::Need)),
            "no claude CLI to ask"
        );
    }

    #[test]
    fn a_stale_t3_fails_only_when_it_should_have_the_token() {
        assert!(failed(read(&out("same", "ok", "stale"), T3::Need)).contains("older token"));
        let noted = ok(read(&out("same", "ok", "stale"), T3::Note));
        assert!(noted.contains("older token") && noted.contains("--restart-t3"));
        assert_eq!(
            ok(read(
                "subs-check: token same\nsubs-check: model ok\n",
                T3::Skip
            )),
            "Claude replies"
        );
        assert!(failed(read(
            "subs-check: token same\nsubs-check: model ok\n",
            T3::Need
        ))
        .contains("not confirmed"));
        assert!(failed(read(&out("same", "ok", "apikey"), T3::Need)).contains("ANTHROPIC_API_KEY"));
        assert!(failed(read(&out("same", "ok", "missing"), T3::Need)).contains("claude-shell.sh"));
    }

    #[test]
    fn token_and_model_failures_are_named() {
        assert!(failed(read(&out("differs", "ok", "current"), T3::Need)).contains("different"));
        assert!(failed(read("subs-check: token absent\n", T3::Need)).contains("no Claude"));
        assert!(failed(read(&out("same", "auth", "current"), T3::Note)).contains("rejected"));
        assert!(failed(read(&out("same", "limited", "current"), T3::Note)).contains("usage limit"));
        assert!(failed(read(&out("same", "failed", "current"), T3::Note)).contains("failed"));
    }

    #[test]
    fn unknown_or_echoed_text_is_never_shown() {
        let echoed = "sk-ant-oat01-SECRET\nsubs-check: token sk-ant-oat01-SECRET\n";
        let m = failed(read(echoed, T3::Need));
        assert!(!m.contains("sk-ant"), "{m}");
        let m = failed(read(
            &out("same", "sk-ant-oat01-SECRET", "current"),
            T3::Need,
        ));
        assert!(!m.contains("sk-ant"), "{m}");
        let m = failed(read(&out("same", "ok", "sk-ant-oat01-SECRET"), T3::Need));
        assert!(!m.contains("sk-ant"), "{m}");
    }
}
