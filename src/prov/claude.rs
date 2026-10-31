use super::*;
use crate::store;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

const YEAR: u64 = 365 * 86400;
const TOKEN_LINE: &str = "export CLAUDE_CODE_OAUTH_TOKEN='";
const STAMP: &str = "# subs installed_at=";

/// Only this type holds the setup-token. It never enters account snapshots or JSON.
pub(crate) struct Token {
    value: String,
    installed_at: Option<u64>,
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
    let token = Token {
        value: value.into(),
        installed_at,
    };
    // Never source arbitrary shell code, even from a private file.
    if body != env_body(Some(&token)) {
        return Err(
            "invalid managed Claude env file; reinstall with subs login claude --token".into(),
        );
    }
    Ok(Some(token))
}

pub(crate) fn token_configured() -> bool {
    !matches!(load(&token_path()), Ok(None))
}

pub(crate) fn load_token() -> Result<Option<Token>, String> {
    load(&token_path())
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
            json!({"present": true, "state": "unverified", "valid": null, "expires_at": null,
                "installed_at": t.installed_at, "estimated_expires_at": expiry,
                "estimate_elapsed": expiry.is_some_and(|s| (s as f64) < store::now())})
        }
    }
}

pub(crate) fn health_summary(h: &Value) -> String {
    let mut s = format!("setup-token: {}", h["state"].as_str().unwrap_or("unknown"));
    if let Some(e) = h["error"].as_str() {
        s += &format!(" · {e}");
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
if [ -L "$d/claude.env" ] || [ -d "$d/claude.env" ]; then exit 1; fi
t=$(mktemp "$d/.claude.env.XXXXXX")
trap 'rm -f "$t"' EXIT
{payload}chmod 600 "$t"
mv -f "$t" "$d/claude.env"
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

pub(crate) fn install_token() -> Result<String, String> {
    let value = if io::stdin().is_terminal() {
        crate::actions::read_secret_checked("Claude setup-token (hidden): ")?
    } else {
        let mut s = String::new();
        io::stdin()
            .lock()
            .take(4098)
            .read_line(&mut s)
            .map_err(|_| "cannot read setup-token from stdin")?;
        s.trim_end_matches(['\r', '\n']).into()
    };
    validate(&value)?;
    let token = Token {
        value,
        installed_at: Some(store::now() as u64),
    };
    install_env(Some(&token))?;
    Ok("claude: setup-token installed (0600, unverified) · open a new shell or source the managed claude-shell.sh; restart Claude sessions".into())
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
            usage: Err("unverified".into()),
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
        assert!(status.contains("setup-token: unverified"));
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
        let mut c = Command::new("shellcheck")
            .args(["-s", "sh", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(env_script(Some(&fake())).as_bytes())
            .unwrap();
        let out = c.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
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
