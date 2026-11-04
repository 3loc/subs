use super::*;
use crate::store;
use std::fs;
use std::io::{self, BufRead, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

const YEAR: u64 = 365 * 86400;
const TOKEN_LINE: &str = "export CLAUDE_CODE_OAUTH_TOKEN='";
const STAMP: &str = "# subs installed_at=";
/// Sidecar naming the token's account, as the user said it is.
const LABEL: &str = "claude-account.json";

/// Only this type holds the setup-token. It never enters account snapshots or JSON.
pub(crate) struct Token {
    value: String,
    installed_at: Option<u64>,
    account_email: Option<String>,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token([REDACTED])")
    }
}

pub(crate) fn token_path() -> PathBuf {
    env_path("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("subs/claude.env")
}

pub(crate) fn warn_api_key() {
    if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        eprintln!("subs: warning: ANTHROPIC_API_KEY is set; ignored for managed Claude commands. Unset it in your shell to use your subscription.");
    }
}

/// Browser OAuth login must not inherit either an API key or a setup-token.
pub(crate) fn login_command() -> Command {
    let mut c = Command::new("claude");
    c.env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN");
    c
}

fn validate(value: &str) -> Result<(), String> {
    if !value.starts_with("sk-ant-oat01-")
        || value.len() <= "sk-ant-oat01-".len()
        || value.len() > 4096
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err("expected a Claude setup-token, not an API key or shell assignment".into());
    }
    Ok(())
}

fn outside_repo(path: &Path) -> Result<(), String> {
    let dir = path.parent().ok_or("missing Claude config directory")?;
    let dir = fs::canonicalize(dir).map_err(|_| "cannot resolve Claude config directory")?;
    if dir.ancestors().any(|p| p.join(".git").exists()) {
        return Err("Claude token file must be outside a git repository".into());
    }
    Ok(())
}

/// Inspect the opened file before reading any secret. Reject symlinks and loose modes.
fn load(path: &Path) -> Result<Option<Token>, String> {
    let before = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot inspect Claude token file".into()),
        Ok(m) => m,
    };
    if !before.is_file() {
        return Err("Claude token file must be a regular file, not a symlink".into());
    }
    outside_repo(path)?;
    let f = fs::File::open(path).map_err(|_| "cannot open Claude token file")?;
    let m = f
        .metadata()
        .map_err(|_| "cannot inspect Claude token file")?;
    if !m.is_file() || m.dev() != before.dev() || m.ino() != before.ino() {
        return Err("Claude token file changed while opening it".into());
    }
    if m.permissions().mode() & 0o7777 & !0o600 != 0 {
        return Err("Claude token file permissions are unsafe; run chmod 600 on claude.env".into());
    }
    let mut body = String::new();
    f.take(8193)
        .read_to_string(&mut body)
        .map_err(|_| "cannot read Claude token file")?;
    if body.len() > 8192 {
        return Err("Claude token file is too large".into());
    }
    if body == env_body(None) {
        return Ok(None);
    }
    let value = body
        .lines()
        .find_map(|l| {
            l.strip_prefix(TOKEN_LINE)
                .and_then(|s| s.strip_suffix('\''))
        })
        .ok_or("invalid managed Claude env file; reinstall with subs login claude --token")?;
    validate(value)?;
    let installed_at = body
        .lines()
        .find_map(|l| l.strip_prefix(STAMP).and_then(|s| s.parse().ok()));
    let mut token = Token {
        value: value.into(),
        installed_at,
        account_email: None,
    };
    // Never source arbitrary shell code, even from a private file.
    if body != env_body(Some(&token)) {
        return Err(
            "invalid managed Claude env file; reinstall with subs login claude --token".into(),
        );
    }
    // The account label lives beside the env file, so that file stays exactly
    // what older versions write. It only counts for the token it was made for.
    let label_path = path.with_file_name(LABEL);
    if fs::symlink_metadata(&label_path)
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o077 == 0 && m.len() <= 1024)
    {
        if let Some(label) = store::rjson(&label_path) {
            if label["token_id"] == token_id(&token.value)
                && label["installed_at"] == json!(token.installed_at)
            {
                token.account_email = label["email"]
                    .as_str()
                    .filter(|s| validate_email(s).is_ok())
                    .map(str::to_owned);
            }
        }
    }
    Ok(Some(token))
}

/// An opaque association tag, not an authentication or signature check.
fn token_id(value: &str) -> String {
    format!(
        "{:016x}",
        value
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x100000001b3))
    )
}

pub(crate) fn token_configured() -> bool {
    !matches!(load(&token_path()), Ok(None))
}

pub(crate) fn load_token() -> Result<Option<Token>, String> {
    load(&token_path())
}

/// The account the installed token is labelled with, if any.
pub(crate) fn token_email() -> Option<String> {
    load_token().ok().flatten()?.account_email
}

pub(crate) fn health() -> Value {
    health_at(&token_path())
}

fn health_at(path: &Path) -> Value {
    match load(path) {
        Ok(None) => json!({"present": false, "state": "absent", "valid": null, "expires_at": null}),
        Err(e) => {
            json!({"present": true, "state": "unusable", "valid": null, "expires_at": null, "error": e})
        }
        Ok(Some(t)) => {
            let expiry = t.installed_at.and_then(|s| s.checked_add(YEAR));
            // A model-only token cannot name its account: the email is what the user said.
            let source = t.account_email.as_ref().map(|_| "user_label");
            json!({"present": true, "state": "unverified", "valid": null, "expires_at": null,
                "installed_at": t.installed_at, "estimated_expires_at": expiry,
                "account_email": t.account_email, "identity_source": source,
                "identity_verified": null,
                "estimate_elapsed": expiry.is_some_and(|s| (s as f64) < store::now())})
        }
    }
}

pub(crate) fn health_summary(h: &Value) -> String {
    // "Configured" describes local file checks, not a provider verification.
    // Keep the machine-readable state and validity unknown until actually checked.
    let state = match h["state"].as_str() {
        Some("unverified") => "configured",
        Some(s) => s,
        None => "unknown",
    };
    let mut s = state.to_string();
    if let Some(e) = h["error"].as_str() {
        s += &format!(" · {e}");
    }
    if h["identity_source"] == "user_label" {
        s += " · user-labelled";
    }
    if let Some(t) = h["estimated_expires_at"].as_f64() {
        s += &if h["estimate_elapsed"] == true {
            " · estimated expiry passed".into()
        } else {
            format!(" · estimated expiry in {}", crate::status::until(t))
        };
    }
    s
}

/// Disable shell tracing before the assignment; never echo the value.
fn env_body(token: Option<&Token>) -> String {
    let mut s = String::from("# Managed by subs; source this file, do not edit.\ncase $- in *v*) set +v; _subs_verbose=1;; *) _subs_verbose=0;; esac\ncase $- in *x*) set +x; _subs_trace=1;; *) _subs_trace=0;; esac\nunset ANTHROPIC_API_KEY\n");
    match token {
        Some(t) => {
            if let Some(stamp) = t.installed_at {
                s += &format!("{STAMP}{stamp}\n");
            }
            s += &format!("{TOKEN_LINE}{}'\n", t.value);
        }
        None => s += "unset CLAUDE_CODE_OAUTH_TOKEN\n",
    }
    s +=
        "if [ \"$_subs_trace\" = 1 ]; then unset _subs_trace; set -x; else unset _subs_trace; fi\n";
    s += "if [ \"$_subs_verbose\" = 1 ]; then unset _subs_verbose; set -v; else unset _subs_verbose; fi\n";
    s
}

/// Shared local/remote installer, carried on stdin by the existing SSH transport.
/// No account JSON or Claude settings are modified for setup-token authentication.
pub(crate) fn env_script(token: Option<&Token>) -> String {
    let label_payload = match token.and_then(|t| t.account_email.as_ref().map(|e| (t, e))) {
        Some((t, email)) => {
            let label = json!({"email": email, "installed_at": t.installed_at, "token_id": token_id(&t.value)});
            format!(
                "t=$(mktemp \"$d/.claude-account.XXXXXX\")\n{}chmod 600 \"$t\"\nmv -f \"$t\" \"$d/{LABEL}\"\n",
                crate::push::heredoc("$t", &label.to_string())
            )
        }
        None => format!("rm -f \"$d/{LABEL}\"\n"),
    };
    let hook = r#"# subs Claude subscription environment
if [ "${ANTHROPIC_API_KEY+x}" = x ]; then
  printf '%s\n' 'subs: warning: ignoring ANTHROPIC_API_KEY for Claude subscription use' >&2
fi
unset ANTHROPIC_API_KEY
_subs_env="${XDG_CONFIG_HOME:-$HOME/.config}/subs/claude.env"
if [ -f "$_subs_env" ] && [ ! -L "$_subs_env" ]; then
  _subs_mode=$(stat -c %a "$_subs_env" 2>/dev/null || stat -f %Lp "$_subs_env" 2>/dev/null)
  case $_subs_mode in
    600|400)
      # The managed path depends on the receiving user's HOME/XDG_CONFIG_HOME.
      # shellcheck disable=SC1090
      . "$_subs_env"
      ;;
    *) unset CLAUDE_CODE_OAUTH_TOKEN; printf '%s\n' 'subs: refusing Claude env file: chmod 600 required' >&2 ;;
  esac
else
  unset CLAUDE_CODE_OAUTH_TOKEN
fi
unset _subs_env _subs_mode
"#;
    format!(
        r#"set +xv
set -eu
unset ANTHROPIC_API_KEY
umask 077
d="${{XDG_CONFIG_HOME:-$HOME/.config}}/subs"
mkdir -p "$d"
chmod 700 "$d"
# Refuse to install credentials anywhere inside a checkout, including worktrees.
p=$(cd "$d" && pwd -P)
while [ "$p" != / ]; do
  if [ -e "$p/.git" ]; then printf '%s\n' 'Claude token file must be outside a git repository' >&2; exit 1; fi
  p=${{p%/*}}; [ -n "$p" ] || p=/
done
for f in claude.env {LABEL}; do
  if [ -L "$d/$f" ] || [ -d "$d/$f" ]; then exit 1; fi
done
t=$(mktemp "$d/.claude.env.XXXXXX")
trap 'rm -f "$t"' EXIT
{payload}chmod 600 "$t"
mv -f "$t" "$d/claude.env"
{label_payload}
[ ! -d "$d/claude-shell.sh" ]
t=$(mktemp "$d/.claude-shell.XXXXXX")
{hook_payload}chmod 600 "$t"
mv -f "$t" "$d/claude-shell.sh"
# Expand HOME/XDG when a shell starts, not when installing the hook.
# shellcheck disable=SC2016
line='[ ! -r "${{XDG_CONFIG_HOME:-$HOME/.config}}/subs/claude-shell.sh" ] || . "${{XDG_CONFIG_HOME:-$HOME/.config}}/subs/claude-shell.sh" # subs Claude'
for f in "$HOME/.profile" "$HOME/.bashrc" "${{ZDOTDIR:-$HOME}}/.zshrc"; do
  if ! grep -Fqx "$line" "$f" 2>/dev/null; then printf '\n%s\n' "$line" >> "$f"; fi
done
# Bash skips .profile when either of these exists.
for f in "$HOME/.bash_profile" "$HOME/.bash_login"; do
  if [ -f "$f" ] && ! grep -Fqx "$line" "$f"; then printf '\n%s\n' "$line" >> "$f"; fi
done
"#,
        payload = crate::push::heredoc("$t", env_body(token).trim_end()),
        hook_payload = crate::push::heredoc("$t", hook)
    )
}

pub(crate) fn install_env(token: Option<&Token>) -> Result<(), String> {
    let mut child = Command::new("sh")
        .arg("-s")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start Claude env installer")?;
    let result = child
        .stdin
        .take()
        .ok_or("cannot open installer stdin")?
        .write_all(env_script(token).as_bytes());
    let status = child
        .wait()
        .map_err(|_| "cannot wait for Claude env installer")?;
    if result.is_err() || !status.success() {
        return Err("Claude env installation failed; check config is writable, outside a repository, and not a symlink".into());
    }
    Ok(())
}

fn validate_email(email: &str) -> Result<(), String> {
    if email.len() > 254
        || email.bytes().filter(|b| *b == b'@').count() != 1
        || email.starts_with('@')
        || email.ends_with('@')
        || !email
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@._+-".contains(&b))
    {
        return Err("expected an account email (letters, numbers, @, ., _, +, -)".into());
    }
    Ok(())
}

pub(crate) fn label_token(email: &str) -> Result<String, String> {
    validate_email(email)?;
    let mut token = load_token()?.ok_or("no Claude subscription token to label")?;
    token.account_email = Some(email.to_owned());
    install_env(Some(&token))?;
    Ok(format!(
        "claude: subscription token labelled {email} (user-provided identity)"
    ))
}

const RELOAD: &str =
    "open a new shell or source the managed claude-shell.sh; restart Claude sessions";

fn install_new(value: String, email: Option<&str>) -> Result<(), String> {
    install_env(Some(&Token {
        value,
        installed_at: Some(store::now() as u64),
        account_email: email.map(str::to_owned),
    }))
}

/// Import an existing token, one line on stdin, as scripts always have.
pub(crate) fn install_token(email: Option<&str>) -> Result<String, String> {
    if let Some(email) = email {
        validate_email(email)?;
    }
    let mut s = String::new();
    io::stdin()
        .lock()
        .take(4098)
        .read_line(&mut s)
        .map_err(|_| "cannot read setup-token from stdin")?;
    let value: String = s.trim_end_matches(['\r', '\n']).into();
    validate(&value)?;
    install_new(value, email)?;
    Ok(format!(
        "claude: setup-token installed (0600, unverified) · {RELOAD}"
    ))
}

/// Hosts that serve Claude's OAuth consent page.
const AUTH_HOSTS: [&str; 4] = [
    "claude.ai",
    "claude.com",
    "console.anthropic.com",
    "platform.claude.com",
];
const TOKEN_PREFIX: &str = "sk-ant-oat01-";
/// Wide enough that no link or token is ever wrapped.
const COLS: u16 = 4096;
const ROWS: u16 = 50;
const TICK: Duration = Duration::from_millis(100);

enum Input {
    Code(String),
    Fail(String),
}

/// Create a long-lived token with Claude's own `claude setup-token` browser flow.
/// It runs on a private terminal in a throwaway config: only the sign-in link is
/// shown, and the token replaces nothing until it has answered a real request.
/// The code from the sign-in page is read hidden when `prompt`, else as one line
/// on stdin. A model-only token cannot name its account: `email` is the label.
pub(crate) fn login_token(email: Option<&str>, prompt: bool) -> Result<String, String> {
    if let Some(email) = email {
        validate_email(email)?;
    }
    let value = scratch(".login-token-", |dir| setup_token(dir, email, prompt))?;
    eprintln!("claude: checking the new token with one small request");
    scratch(".login-check-", |dir| check(dir, &value))?;
    install_new(value, email)?;
    Ok(match email {
        Some(e) => format!("claude: {e} [token] installed and checked · {e} is your label; Claude does not report a token's account · {RELOAD}"),
        None => format!("claude: subscription token installed and checked · name its account with subs label claude EMAIL · {RELOAD}"),
    })
}

/// A private directory for one Claude run, removed afterwards. Logins sweep
/// `.login-*` leftovers of a run that was killed hard.
fn scratch<T>(prefix: &str, f: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let dir = store::root().join("claude");
    store::mkdir(&dir).map_err(|e| e.to_string())?;
    store::sweep_logins(&dir);
    let tmp = store::mkdtemp(&dir, prefix).map_err(|e| e.to_string())?;
    let r = f(&tmp);
    let _ = fs::remove_dir_all(&tmp);
    r
}

fn setup_token(dir: &Path, email: Option<&str>, prompt: bool) -> Result<String, String> {
    let mut cmd = login_command();
    cmd.arg("setup-token")
        .env("CLAUDE_CONFIG_DIR", dir)
        .current_dir(dir);
    let mut term = crate::pty::Term::spawn(cmd, COLS, ROWS)
        .map_err(|_| "cannot run claude setup-token; is Claude Code installed?")?;
    let mut screen = crate::pty::Screen::new(ROWS.into());
    let start = Instant::now();
    let url = loop {
        let alive = pump(&term, &mut screen);
        if let Some(t) = token_on(&screen, !alive) {
            return Ok(t);
        }
        if let Some(u) = auth_url(&screen, !alive) {
            break u;
        }
        if !alive || start.elapsed() > Duration::from_secs(90) {
            return Err(format!(
                "no sign-in link from claude setup-token{}",
                said(&screen, None)
            ));
        }
    };
    let who = email.map(|e| format!(" as {e}")).unwrap_or_default();
    eprintln!("claude: open this link and sign in{who} to create a long-lived subscription token:\n\n{url}\n");
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = if prompt {
        eprintln!(
            "then paste the code the page shows; a browser on this machine finishes by itself"
        );
        let stop = stop.clone();
        Some(thread::spawn(move || {
            let r = crate::actions::read_secret_until("code (hidden): ", &|| {
                stop.load(Ordering::Relaxed)
            });
            let _ = tx.send(match r {
                Ok(Some(code)) => Input::Code(code),
                Ok(None) => return,
                Err(e) => Input::Fail(e),
            });
        }))
    } else {
        eprintln!("then send the code the page shows as one line on stdin");
        thread::spawn(move || {
            let mut s = String::new();
            let read = io::stdin().lock().take(4096).read_line(&mut s);
            let _ = tx.send(match read {
                Ok(n) if n > 0 => Input::Code(s),
                _ => Input::Fail(
                    "stdin closed before the code arrived; the current login is unchanged".into(),
                ),
            });
        });
        None
    };
    let r = wait_token(&mut term, &mut screen, &rx);
    stop.store(true, Ordering::Relaxed);
    if let Some(r) = reader {
        let _ = r.join();
    }
    r
}

fn wait_token(
    term: &mut crate::pty::Term,
    screen: &mut crate::pty::Screen,
    rx: &mpsc::Receiver<Input>,
) -> Result<String, String> {
    let mut code: Option<String> = None;
    // The sign-in itself is the user's time; after the code, Claude's.
    let mut deadline = Instant::now() + Duration::from_secs(30 * 60);
    loop {
        let alive = pump(term, screen);
        if let Some(t) = token_on(screen, !alive) {
            return Ok(t);
        }
        if !alive || Instant::now() > deadline {
            let why = if alive { "timed out" } else { "ended" };
            return Err(format!(
                "claude setup-token {why} without a token; the current login is unchanged{}",
                said(screen, code.as_deref())
            ));
        }
        match rx.try_recv() {
            Ok(Input::Fail(e)) => return Err(e),
            Ok(Input::Code(c)) => {
                let c = c.trim().to_string();
                // An existing token pasted here is taken as it is.
                if validate(&c).is_ok() {
                    return Ok(c);
                }
                if !(8..=2048).contains(&c.len())
                    || !c
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"#-_.~".contains(&b))
                {
                    return Err("that is not the code from the sign-in page; the current login is unchanged".into());
                }
                let sent = term.send(c.as_bytes()).and_then(|_| {
                    thread::sleep(Duration::from_millis(200));
                    term.send(b"\r")
                });
                sent.map_err(|_| "claude setup-token stopped reading input")?;
                eprintln!("claude: code sent, waiting for the token");
                code = Some(c);
                deadline = Instant::now() + Duration::from_secs(120);
            }
            Err(_) => {}
        }
    }
}

/// Feed whatever output there is to the screen. False once the program is gone.
fn pump(term: &crate::pty::Term, screen: &mut crate::pty::Screen) -> bool {
    match term.output.recv_timeout(TICK) {
        Ok(b) => screen.feed(&b),
        Err(mpsc::RecvTimeoutError::Timeout) => return true,
        Err(mpsc::RecvTimeoutError::Disconnected) => return false,
    }
    loop {
        match term.output.try_recv() {
            Ok(b) => screen.feed(&b),
            Err(mpsc::TryRecvError::Empty) => return true,
            Err(mpsc::TryRecvError::Disconnected) => return false,
        }
    }
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Each `needle` on screen with the run of characters after it that `keep`
/// accepts, once fully drawn: something follows it, the cursor has left it,
/// or the program has ended.
fn drawn(
    screen: &crate::pty::Screen,
    needle: &str,
    keep: fn(char) -> bool,
    ended: bool,
) -> Vec<String> {
    let (row, col) = screen.cursor();
    let mut found = vec![];
    for (i, line) in screen.lines().iter().enumerate() {
        let mut from = 0;
        while let Some(at) = line[from..].find(needle).map(|a| a + from) {
            from = at + needle.len();
            if line[..at].chars().next_back().is_some_and(token_char) {
                continue;
            }
            let end = line[at..].find(|c| !keep(c)).map_or(line.len(), |n| at + n);
            if ended || end < line.len() || i != row || col > line[..end].chars().count() {
                found.push(line[at..end].to_string());
            }
        }
    }
    found
}

fn token_on(screen: &crate::pty::Screen, ended: bool) -> Option<String> {
    drawn(screen, TOKEN_PREFIX, token_char, ended)
        .into_iter()
        .find(|t| validate(t).is_ok())
}

fn auth_url(screen: &crate::pty::Screen, ended: bool) -> Option<String> {
    drawn(screen, "https://", |c| c.is_ascii_graphic(), ended)
        .into_iter()
        .find(|u| {
            let rest = &u["https://".len()..];
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            AUTH_HOSTS.contains(&host) && path.contains("oauth/authorize") && !u.contains("sk-ant-")
        })
}

/// The last few lines Claude drew, for an error. Nothing else it prints is
/// shown, and long code- or token-like words are cut out of these too.
fn said(screen: &crate::pty::Screen, code: Option<&str>) -> String {
    let lines: Vec<String> = screen
        .lines()
        .iter()
        .map(|l| redact(l, code))
        .filter(|l| !l.is_empty())
        .collect();
    let last = lines[lines.len().saturating_sub(3)..].join(" · ");
    if last.is_empty() {
        return String::new();
    }
    format!(" · claude: {}", last.chars().take(240).collect::<String>())
}

fn redact(text: &str, secret: Option<&str>) -> String {
    let text = match secret.filter(|s| !s.is_empty()) {
        Some(s) => text.replace(s, "[redacted]"),
        None => text.to_string(),
    };
    text.split_whitespace()
        .map(|w| {
            let secretish = w.contains("sk-ant-")
                || (w.len() >= 20 && w.chars().all(|c| token_char(c) || "#.~+/=:?&%".contains(c)));
            if secretish {
                "[redacted]"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One small real request with the new token through Claude's own CLI, in a
/// throwaway config, before the token replaces anything. Only a short,
/// redacted reason of a failure is shown.
fn check(dir: &Path, token: &str) -> Result<(), String> {
    let mut c = login_command();
    c.args(["-p", "--model", "haiku"])
        .env("CLAUDE_CODE_OAUTH_TOKEN", token)
        .env("CLAUDE_CONFIG_DIR", dir)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Anything that would send the request somewhere other than the subscription.
    for k in [
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
    ] {
        c.env_remove(k);
    }
    let fail = |why: &str| {
        format!("the new token failed its check ({why}); nothing was installed, the current login is unchanged")
    };
    let mut child = c.spawn().map_err(|_| fail("cannot run claude"))?;
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(b"Reply with the single word ok.\n");
    }
    fn drain(p: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
        thread::spawn(move || {
            let mut s = String::new();
            if let Some(p) = p {
                let _ = p.take(65536).read_to_string(&mut s);
            }
            s
        })
    }
    let pipes = [drain(child.stdout.take()), drain(child.stderr.take())];
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(TICK),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let [out, err] = pipes.map(|h| h.join().unwrap_or_default());
    match status {
        None => Err(fail("timed out")),
        Some(s) if s.success() && !out.trim().is_empty() => Ok(()),
        Some(s) => {
            let last = [&err, &out]
                .iter()
                .find_map(|t| t.lines().rev().find(|l| !l.trim().is_empty()))
                .map(|l| redact(l, Some(token)).chars().take(100).collect::<String>());
            Err(fail(&last.unwrap_or_else(|| match s.code() {
                Some(n) => format!("exit {n}"),
                None => "killed".into(),
            })))
        }
    }
}

pub(super) fn usage(blob: &Value) -> Result<Usage, String> {
    let o = &blob["credentials"]["claudeAiOauth"];
    let token = o["accessToken"].as_str().ok_or("no access token")?;
    let u = get(
        "https://api.anthropic.com/api/oauth/usage",
        &[("Authorization", &format!("Bearer {token}")), BETA],
    )?;
    parse(&u, o)
}

fn parse(u: &Value, o: &Value) -> Result<Usage, String> {
    let windows: Vec<Win> = [("five_hour", "5h"), ("seven_day", "7d")]
        .iter()
        .filter(|(k, _)| u[*k].is_object())
        .map(|(k, label)| Win {
            label: label.to_string(),
            pct: u[*k]["utilization"].as_f64(),
            reset: u[*k]["resets_at"].as_str().and_then(iso),
        })
        .collect();
    let x = &u["extra_usage"];
    let extra = (x["is_enabled"].as_bool() == Some(true)).then(|| {
        match (x["used_credits"].as_f64(), x["monthly_limit"].as_f64()) {
            (Some(used), Some(limit)) => format!("extra credits {used:.0}/{limit:.0}"),
            _ => "extra credits unknown".into(),
        }
    });
    let tier = o["rateLimitTier"].as_str().unwrap_or("");
    let mut plan = o["subscriptionType"].as_str().unwrap_or("?").to_string();
    if let Some(m) = ["20x", "5x"].iter().find(|m| tier.ends_with(*m)) {
        plan = format!("{plan} {m}");
    }
    let limited = windows.iter().any(|w| w.pct.unwrap_or(0.0) >= 100.0);
    Ok(Usage {
        spend: Spend::unknown("flat subscription; dollar spend not exposed"),
        plan: Some(plan),
        windows,
        resets: None,
        extra,
        limited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "sk-ant-oat01-FAKE-test-only";

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            Self(store::mkdtemp(&std::env::temp_dir(), "subs-claude-").unwrap())
        }
        fn file(&self, body: &str, mode: u32) -> PathBuf {
            let p = self.0.join("claude.env");
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
            p
        }
        fn command(&self, executable: &str) -> Command {
            let mut c = Command::new(executable);
            c.env("HOME", &self.0)
                .env("XDG_CONFIG_HOME", self.0.join(".config"))
                .env_remove("ZDOTDIR")
                .env_remove("BASH_ENV")
                .env_remove("ENV")
                .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
                .env_remove("ANTHROPIC_API_KEY");
            c
        }
        fn install(&self, token: Option<&Token>) {
            let mut c = self
                .command("sh")
                .arg("-s")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            c.stdin
                .take()
                .unwrap()
                .write_all(env_script(token).as_bytes())
                .unwrap();
            let out = c.wait_with_output().unwrap();
            assert!(out.status.success(), "installer failed");
            assert!(!String::from_utf8_lossy(&out.stderr).contains(FAKE));
            assert!(out.stdout.is_empty());
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake() -> Token {
        Token {
            value: FAKE.into(),
            installed_at: Some(1_000_000),
            account_email: None,
        }
    }

    #[test]
    fn token_permissions_and_symlinks_are_checked_before_use() {
        let tmp = Temp::new();
        for mode in [0o644, 0o640, 0o666, 0o700, 0o1600] {
            let p = tmp.file(&env_body(Some(&fake())), mode);
            assert!(load(&p).unwrap_err().contains("permissions"));
            assert_eq!(health_at(&p)["state"], "unusable");
        }
        for mode in [0o600, 0o400] {
            let p = tmp.file(&env_body(Some(&fake())), mode);
            assert!(load(&p).unwrap().is_some());
        }
        let link = tmp.0.join("link");
        std::os::unix::fs::symlink(tmp.0.join("claude.env"), &link).unwrap();
        assert!(load(&link).unwrap_err().contains("symlink"));
    }

    #[test]
    fn token_rejects_api_keys_injection_and_repository_storage() {
        for s in [
            "",
            "sk-ant-api03-FAKE",
            "sk-ant-oat01-",
            "sk-ant-oat01-FAKE'; touch /tmp/no",
            "sk-ant-oat01-FAKE\nextra",
        ] {
            let err = validate(s).unwrap_err();
            assert!(!err.contains("FAKE"));
        }
        let tmp = Temp::new();
        let p = tmp.file(&(env_body(Some(&fake())) + "echo bad\n"), 0o600);
        assert!(load(&p).is_err());
        tmp.file(&env_body(Some(&fake())), 0o600);
        fs::write(tmp.0.join(".git"), "gitdir: fake-worktree").unwrap();
        assert!(load(&p).unwrap_err().contains("outside"));
    }

    #[test]
    fn token_status_json_debug_and_errors_never_contain_the_secret() {
        let tmp = Temp::new();
        let p = tmp.file(&env_body(Some(&fake())), 0o600);
        let h = health_at(&p);
        assert_eq!(h["state"], "unverified");
        assert!(h["valid"].is_null());
        assert!(h["expires_at"].is_null());
        assert_eq!(h["estimated_expires_at"], 1_000_000 + YEAR);
        assert_eq!(h["estimate_elapsed"], true);
        let row = crate::status::Row {
            prov: Prov::Claude,
            email: String::new(),
            active: true,
            password: false,
            usage: crate::status::UsageState::Unsupported,
            token: Some(h),
        };
        let rows = [row];
        let json = crate::status::to_json(&rows).to_string();
        assert!(json.contains("estimated_expires_at"));
        let status = crate::view::body(&rows, None, 76)
            .lines
            .iter()
            .map(|l| crate::view::ansi(l, false))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(status.contains("configured"));
        assert!(status.contains("estimated expiry passed"));
        for output in [json, status, format!("{:?}", fake())] {
            assert!(!output.contains(FAKE));
        }
        let p = tmp.file("sk-ant-oat01-FAKE-secret-malformed", 0o600);
        assert!(!load(&p).unwrap_err().contains("FAKE"));
    }

    #[test]
    fn absent_disabled_and_unknown_expiry_are_explicit() {
        let tmp = Temp::new();
        assert_eq!(health_at(&tmp.0.join("missing"))["present"], false);
        let p = tmp.file(&env_body(None), 0o600);
        assert_eq!(health_at(&p)["state"], "absent");
        let t = Token {
            value: FAKE.into(),
            installed_at: None,
            account_email: None,
        };
        let p = tmp.file(&env_body(Some(&t)), 0o600);
        assert!(health_at(&p)["estimated_expires_at"].is_null());
    }

    #[test]
    fn oauth_login_removes_api_key_and_inherited_setup_token() {
        let c = login_command();
        for name in ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"] {
            assert!(c.get_envs().any(|(k, v)| k == name && v.is_none()));
        }
    }

    #[test]
    fn installer_is_private_idempotent_and_shells_load_without_leaks() {
        let tmp = Temp::new();
        fs::write(tmp.0.join(".bash_profile"), "# existing profile\n").unwrap();
        for _ in 0..2 {
            tmp.install(Some(&fake()));
        }
        let p = tmp.0.join(".config/subs/claude.env");
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load(&p).unwrap().unwrap().installed_at, fake().installed_at);
        for rc in [".profile", ".bashrc", ".zshrc", ".bash_profile"] {
            assert_eq!(
                fs::read_to_string(tmp.0.join(rc))
                    .unwrap()
                    .matches("# subs Claude")
                    .count(),
                1
            );
        }
        // The secret is consumed by the shell but neither stdout nor xtrace may show it.
        for shell in ["sh", "bash", "zsh"] {
            if Command::new(shell).arg("--version").output().is_err() {
                continue;
            }
            let out = tmp.command(shell).args(["-xvc", ". \"$HOME/.profile\"; test -n \"${CLAUDE_CODE_OAUTH_TOKEN:+present}\"; test \"${ANTHROPIC_API_KEY+x}\" != x"])
                .env("ANTHROPIC_API_KEY", "FAKE-API-KEY").output().unwrap();
            assert!(out.status.success(), "shell {shell} failed");
            assert!(!String::from_utf8_lossy(&out.stderr).contains(FAKE));
            assert!(!String::from_utf8_lossy(&out.stderr).contains("FAKE-API-KEY"));
            assert!(out.stdout.is_empty());
        }
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        let out = tmp
            .command("sh")
            .args([
                "-c",
                ". \"$HOME/.profile\"; test \"${CLAUDE_CODE_OAUTH_TOKEN+x}\" != x",
            ])
            .env("CLAUDE_CODE_OAUTH_TOKEN", "stale")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("chmod 600"));
        tmp.install(None);
        assert!(load(&p).unwrap().is_none());
        let out = tmp.command("sh").args(["-c", ". \"$HOME/.profile\"; test \"${CLAUDE_CODE_OAUTH_TOKEN+x}\" != x; test \"${ANTHROPIC_API_KEY+x}\" != x"])
            .env("CLAUDE_CODE_OAUTH_TOKEN", "stale").env("ANTHROPIC_API_KEY", "fake").output().unwrap();
        assert!(out.status.success());
        assert_eq!(fs::read_dir(p.parent().unwrap()).unwrap().count(), 2);
    }

    #[test]
    fn generated_installer_passes_shellcheck() {
        // Optional for contributors without shellcheck; required and run in our validation.
        if Command::new("shellcheck")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let labelled = Token {
            account_email: Some("a@example.invalid".into()),
            ..fake()
        };
        for token in [fake(), labelled] {
            let mut c = Command::new("shellcheck")
                .args(["-s", "sh", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            c.stdin
                .take()
                .unwrap()
                .write_all(env_script(Some(&token)).as_bytes())
                .unwrap();
            let out = c.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        let tmp = Temp::new();
        tmp.install(Some(&fake()));
        let out = Command::new("shellcheck")
            .args(["-s", "sh"])
            .arg(tmp.0.join(".config/subs/claude.env"))
            .arg(tmp.0.join(".config/subs/claude-shell.sh"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn screen(chunks: &[&str]) -> crate::pty::Screen {
        let mut s = crate::pty::Screen::new(10);
        for c in chunks {
            s.feed(c.as_bytes());
        }
        s
    }

    #[test]
    fn token_is_taken_off_the_screen_only_once_fully_drawn() {
        let mut s =
            screen(&["Your OAuth token (valid for 1 year):\x1b[38G\x1b[32msk-ant-oat01-ABC"]);
        assert!(token_on(&s, false).is_none(), "still being drawn");
        s.feed(b"DEF\x1b[39m\r\n");
        assert_eq!(token_on(&s, false).as_deref(), Some("sk-ant-oat01-ABCDEF"));
        // A renderer may skip cells that already show the right character.
        let s = screen(&["xxxxxxxxxxxxxxxxQ\r", "sk-ant-oat01-ABC\x1b[1CEF\x1b[K\r\n"]);
        assert_eq!(token_on(&s, false).as_deref(), Some("sk-ant-oat01-ABCQEF"));
        let s = screen(&["sk-ant-api03-ABCDEF\r\nxsk-ant-oat01-ABC\r\n"]);
        assert!(token_on(&s, true).is_none());
    }

    #[test]
    fn only_a_complete_claude_authorize_link_is_shown() {
        let url = "https://claude.com/cai/oauth/authorize?code=true&state=S";
        let s = screen(&[&format!("\x1b]8;id=1;{url}\x07{url}")]);
        assert!(auth_url(&s, false).is_none(), "still being drawn");
        let s = screen(&[&format!("see https://example.invalid/oauth/authorize?x\r\n\x1b]8;id=1;{url}\x07{url}\x1b]8;;\x07\r\n")]);
        assert_eq!(auth_url(&s, false).as_deref(), Some(url));
    }

    #[test]
    fn error_text_never_carries_tokens_or_codes() {
        let s = screen(&[
            "Paste code here if prompted > c0de#5tate\r\n",
            "OAuth error: sk-ant-oat01-SECRET rejected abcdefghijklmnopqrstuvwxyz0123#state\r\n",
        ]);
        let text = said(&s, Some("c0de#5tate"));
        assert!(text.contains("OAuth error:") && text.contains("rejected"));
        for secret in ["c0de", "SECRET", "abcdefghijklmnop"] {
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn labelled_installer_keeps_env_bytes_and_refuses_a_redirected_label() {
        let tmp = Temp::new();
        let mut t = fake();
        t.account_email = Some("a@example.invalid".into());
        tmp.install(Some(&t));
        let env = tmp.0.join(".config/subs/claude.env");
        assert_eq!(fs::read_to_string(&env).unwrap(), env_body(Some(&fake())));
        assert_eq!(load(&env).unwrap().unwrap().account_email, t.account_email);
        // An unlabelled token clears the label; one made for another token is ignored.
        let other = || Token {
            value: format!("{FAKE}-other"),
            ..fake()
        };
        tmp.install(Some(&other()));
        let label = tmp.0.join(".config/subs").join(LABEL);
        assert!(!label.exists());
        tmp.install(Some(&t));
        fs::write(&env, env_body(Some(&other()))).unwrap();
        assert!(load(&env).unwrap().unwrap().account_email.is_none());
        // Never write through a symlink or into a directory in its place.
        fs::remove_file(&label).unwrap();
        fs::create_dir(tmp.0.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(tmp.0.join("elsewhere"), &label).unwrap();
        let mut c = tmp
            .command("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(env_script(Some(&t)).as_bytes())
            .unwrap();
        assert!(!c.wait().unwrap().success());
        assert_eq!(fs::read_dir(tmp.0.join("elsewhere")).unwrap().count(), 0);
    }

    #[test]
    fn subscription_plan_and_windows() {
        let u = parse(&json!({"five_hour":{"utilization":37,"resets_at":"2026-10-30T12:00:00Z"},"seven_day":{"utilization":100},"extra_usage":{"is_enabled":true,"used_credits":123,"monthly_limit":1000}}), &json!({"subscriptionType":"max","rateLimitTier":"default_claude_max_20x"})).unwrap();
        assert_eq!(u.plan.as_deref(), Some("max 20x"));
        assert_eq!(u.windows[0].pct, Some(37.0));
        assert!(u.windows[0].reset.is_some());
        assert!(u.limited);
        assert!(u.spend.used.is_none());
        assert_eq!(u.extra.as_deref(), Some("extra credits 123/1000"));
    }
    #[test]
    fn unknown_extra_credits_stay_unknown() {
        let u = parse(
            &json!({"five_hour":{},"extra_usage":{"is_enabled":true}}),
            &json!({}),
        )
        .unwrap();
        assert!(u.windows[0].pct.is_none());
        assert_eq!(u.extra.as_deref(), Some("extra credits unknown"));
    }
}
