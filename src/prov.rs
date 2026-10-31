//! Providers. Each reads and writes its live credential "blob", names the
//! account in it, and asks the provider - not the token - for plan and usage.

pub mod bifrost;
pub(crate) mod claude;
mod codex;
pub(crate) mod cursor;

use crate::store::{env_path, home, rjson, wjson};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prov {
    Codex,
    Claude,
    Cursor,
    Bifrost,
}

pub const ALL: [Prov; 3] = [Prov::Codex, Prov::Claude, Prov::Cursor];
pub const DISPLAY: [Prov; 4] = [Prov::Codex, Prov::Claude, Prov::Cursor, Prov::Bifrost];
pub const MAC: bool = cfg!(target_os = "macos");
const KEYCHAIN: &str = "Claude Code-credentials";
const BETA: (&str, &str) = ("anthropic-beta", "oauth-2025-04-20");

pub struct Win {
    pub label: String,
    pub pct: Option<f64>,
    pub reset: Option<f64>,
}

/// Dollar accounting stays separate from subscription usage percentages.
pub struct Spend {
    pub used: Option<f64>,
    pub cap: Option<f64>,
    pub currency: Option<String>,
    pub period: Option<String>,
    pub reason: Option<String>,
}

impl Spend {
    pub fn unknown(reason: &str) -> Self {
        Self {
            used: None,
            cap: None,
            currency: None,
            period: None,
            reason: Some(reason.into()),
        }
    }

    pub fn summary(&self) -> String {
        match (self.used, self.cap) {
            (Some(used), Some(cap)) => format!(
                "{} {used:.4} / {cap:.2}{}",
                self.currency.as_deref().unwrap_or("?"),
                self.period
                    .as_ref()
                    .map(|p| format!(" · {p}"))
                    .unwrap_or_default()
            ),
            (Some(used), None) => format!(
                "{} {used:.4} · cap unknown{}",
                self.currency.as_deref().unwrap_or("?"),
                self.period
                    .as_ref()
                    .map(|p| format!(" · {p}"))
                    .unwrap_or_default()
            ),
            _ => format!("n/a · {}", self.reason.as_deref().unwrap_or("not reported")),
        }
    }

    pub fn json(&self) -> Value {
        json!({"used": self.used, "cap": self.cap, "currency": self.currency, "period": self.period, "reason": self.reason})
    }
}

pub struct Usage {
    pub spend: Spend,
    pub plan: Option<String>,
    pub windows: Vec<Win>,
    pub resets: Option<i64>,
    pub extra: Option<String>,
    pub limited: bool,
}

fn codex_home() -> PathBuf {
    env_path("CODEX_HOME").unwrap_or_else(|| home().join(".codex"))
}

fn claude_cfg() -> PathBuf {
    env_path("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home().join(".claude"))
}

/// ~/.claude.json, or $CLAUDE_CONFIG_DIR/.claude.json when that is set.
fn claude_state(cfg: Option<&Path>) -> PathBuf {
    match cfg {
        Some(c) => c.join(".claude.json"),
        None if env_path("CLAUDE_CONFIG_DIR").is_some() => claude_cfg().join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

/// The default macOS Claude login lives in the keychain, not a file.
pub fn claude_keychain(cfg: Option<&Path>) -> bool {
    MAC && cfg.is_none() && env_path("CLAUDE_CONFIG_DIR").is_none()
}

impl Prov {
    pub fn name(self) -> &'static str {
        match self {
            Prov::Codex => "codex",
            Prov::Claude => "claude",
            Prov::Cursor => "cursor",
            Prov::Bifrost => "bifrost",
        }
    }

    pub fn parse(s: &str) -> Option<Prov> {
        ALL.into_iter().find(|p| p.name() == s)
    }

    /// The live credential, or the one a login left in `dir`.
    pub fn read(self, dir: Option<&Path>) -> Option<Value> {
        match self {
            Prov::Bifrost => None,
            Prov::Cursor => cursor::read(dir),
            Prov::Codex => rjson(
                &dir.map(Path::to_path_buf)
                    .unwrap_or_else(codex_home)
                    .join("auth.json"),
            ),
            Prov::Claude => {
                // Claude reads the keychain first and falls back to the file,
                // which is also where `subs push` leaves a Mac's login.
                let file = || {
                    rjson(
                        &dir.map(Path::to_path_buf)
                            .unwrap_or_else(claude_cfg)
                            .join(".credentials.json"),
                    )
                };
                let cred = if claude_keychain(dir) {
                    keychain().or_else(file)?
                } else {
                    file()?
                };
                let mut acct = rjson(&claude_state(dir))
                    .map(|s| s["oauthAccount"].clone())
                    .filter(|a| a.is_object());
                if acct.is_none() && dir.is_some() {
                    acct = profile(&cred); // a fresh login may not have written it yet
                }
                Some(json!({"credentials": cred, "account": acct}))
            }
        }
    }

    pub fn write(self, blob: &Value) -> Result<(), String> {
        let io = |e: std::io::Error| e.to_string();
        match self {
            Prov::Bifrost => Err("Bifrost is read-only".into()),
            Prov::Cursor => cursor::write(blob),
            Prov::Codex => wjson(&codex_home().join("auth.json"), blob).map_err(io),
            Prov::Claude => {
                if claude_keychain(None) {
                    let user = std::env::var("USER").unwrap_or_default();
                    let cred = blob["credentials"].to_string();
                    let ok = Command::new("security")
                        .args([
                            "add-generic-password",
                            "-U",
                            "-a",
                            &user,
                            "-s",
                            KEYCHAIN,
                            "-w",
                            &cred,
                        ])
                        .status()
                        .is_ok_and(|s| s.success());
                    if !ok {
                        return Err("keychain write failed".into());
                    }
                } else {
                    wjson(
                        &claude_cfg().join(".credentials.json"),
                        &blob["credentials"],
                    )
                    .map_err(io)?;
                }
                let path = claude_state(None);
                let mut st = rjson(&path)
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                st["oauthAccount"] = blob["account"].clone();
                wjson(&path, &st).map_err(io)
            }
        }
    }

    pub fn email(self, blob: &Value) -> Option<String> {
        match self {
            Prov::Bifrost => None,
            Prov::Cursor => cursor::email(blob),
            Prov::Codex => jwt(blob["tokens"]["id_token"].as_str()?)?["email"]
                .as_str()
                .map(String::from),
            Prov::Claude => blob["account"]["emailAddress"].as_str().map(String::from),
        }
    }

    pub fn expires(self, blob: &Value) -> Option<f64> {
        match self {
            Prov::Bifrost => None,
            Prov::Cursor => cursor::expires(blob),
            Prov::Codex => jwt(blob["tokens"]["access_token"].as_str()?)?["exp"].as_f64(),
            Prov::Claude => blob["credentials"]["claudeAiOauth"]["expiresAt"]
                .as_f64()
                .map(|ms| ms / 1000.0),
        }
    }

    /// Run the real CLI's login, into `dir` when given.
    pub fn login(self, email: Option<&str>, dir: Option<&Path>) -> bool {
        let mut c = match self {
            Prov::Bifrost => return false,
            Prov::Cursor => return cursor::login(dir),
            Prov::Codex => {
                let mut c = Command::new("codex");
                c.args([
                    "login",
                    "-c",
                    "cli_auth_credentials_store=\"file\"",
                    "--device-auth",
                ]);
                c
            }
            Prov::Claude => {
                let mut c = claude::login_command();
                c.args(["auth", "login"]);
                if let Some(e) = email {
                    c.args(["--email", e]);
                }
                c
            }
        };
        if let Some(d) = dir {
            match self {
                Prov::Codex => {
                    c.env("CODEX_HOME", d);
                }
                Prov::Claude => {
                    c.env("CLAUDE_CONFIG_DIR", d);
                }
                Prov::Cursor | Prov::Bifrost => {}
            }
        }
        c.status().is_ok_and(|s| s.success())
    }

    pub fn usage(self, blob: &Value) -> Result<Usage, String> {
        match self {
            Prov::Cursor => cursor::usage(blob),
            Prov::Bifrost => bifrost::usage(),
            Prov::Codex => codex::usage(blob),
            Prov::Claude => claude::usage(blob),
        }
    }
}

fn keychain() -> Option<Value> {
    let o = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN, "-w"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| serde_json::from_slice(&o.stdout).ok())
        .flatten()
}

/// The account behind a Claude token, shaped like ~/.claude.json's oauthAccount.
fn profile(cred: &Value) -> Option<Value> {
    let token = cred["claudeAiOauth"]["accessToken"].as_str()?;
    let p = get(
        "https://api.anthropic.com/api/oauth/profile",
        &[("Authorization", &format!("Bearer {token}")), BETA],
    )
    .ok()?;
    let (a, o) = (&p["account"], &p["organization"]);
    Some(json!({
        "accountUuid": a["uuid"], "emailAddress": a["email"], "displayName": a["display_name"],
        "organizationUuid": o["uuid"], "organizationName": o["name"], "billingType": o["billing_type"],
    }))
}

/// GET a JSON endpoint through curl. Headers go in on stdin, so tokens never
/// show up in `ps`.
fn get(url: &str, headers: &[(&str, &str)]) -> Result<Value, String> {
    let mut c = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "15",
            "-H",
            "@-",
            "-w",
            "\n%{http_code}",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    let mut h = String::from("User-Agent: subs/2\n");
    for (k, v) in headers {
        h += &format!("{k}: {v}\n");
    }
    if let Some(mut stdin) = c.stdin.take() {
        let _ = stdin.write_all(h.as_bytes());
    }
    let out = c.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().trim_start_matches("curl: ");
        let err = err.split_once(") ").map_or(err, |(_, rest)| rest);
        return Err(err.chars().take(48).collect());
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let (body, code) = s.rsplit_once('\n').unwrap_or(("", &s));
    match code.trim().parse::<u16>().unwrap_or(0) {
        c @ (401 | 403) => Err(format!("auth rejected ({c})")),
        c if c >= 400 || c == 0 => Err(format!("http {c}")),
        _ => serde_json::from_str(body).map_err(|_| "unreadable response".into()),
    }
}

fn span(sec: Option<f64>) -> String {
    match sec.map(|s| s as i64) {
        Some(s) if s > 0 && s % 86400 == 0 => format!("{}d", s / 86400),
        Some(s) if s > 0 => format!("{}h", (s as f64 / 3600.0).round()),
        _ => "?".into(),
    }
}

fn jwt(token: &str) -> Option<Value> {
    serde_json::from_slice(&b64url(token.split('.').nth(1)?)?).ok()
}

fn b64url(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// RFC 3339 timestamp to epoch seconds.
pub fn iso(s: &str) -> Option<f64> {
    let n = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, se) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let mut rest = s.get(19..)?;
    let mut frac = 0.0;
    if let Some(r) = rest.strip_prefix('.') {
        let end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        frac = format!("0.{}", &r[..end]).parse().unwrap_or(0.0);
        rest = &r[end..];
    }
    let off = match rest {
        "" | "Z" | "z" => 0,
        r => {
            let r = r.replace(':', "");
            let sign = if r.starts_with('-') { -1 } else { 1 };
            let hh: i64 = r.get(1..3)?.parse().ok()?;
            let mm: i64 = r.get(3..5).and_then(|m| m.parse().ok()).unwrap_or(0);
            sign * (hh * 3600 + mm * 60)
        }
    };
    Some((days(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - off) as f64 + frac)
}

/// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parses_offsets_and_fractions() {
        assert_eq!(iso("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(iso("2026-10-27T15:00:00+00:00"), Some(1_793_113_200.0));
        assert_eq!(iso("2026-10-27T17:00:00+02:00"), Some(1_793_113_200.0));
        assert_eq!(iso("2026-10-27T15:00:00.5Z"), Some(1_793_113_200.5));
        assert_eq!(iso("garbage"), None);
    }

    #[test]
    fn jwt_reads_payload() {
        // {"email":"a@b.c"} base64url, unpadded
        let t = format!("x.{}.y", "eyJlbWFpbCI6ImFAYi5jIn0");
        assert_eq!(jwt(&t).unwrap()["email"], "a@b.c");
    }

    #[test]
    fn span_names_windows() {
        assert_eq!(span(Some(18000.0)), "5h");
        assert_eq!(span(Some(604800.0)), "7d");
        assert_eq!(span(None), "?");
    }
}
