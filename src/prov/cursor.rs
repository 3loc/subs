//! Cursor CLI subscription authentication, account usage and host installation.
use super::*;
use std::os::unix::fs::PermissionsExt;

// Cursor auth ignores CURSOR_CONFIG_DIR. Linux uses XDG; macOS uses HOME.
fn auth_path(dir: Option<&Path>) -> PathBuf {
    if MAC {
        dir.map(Path::to_path_buf)
            .unwrap_or_else(home)
            .join(".cursor/auth.json")
    } else {
        dir.map(Path::to_path_buf)
            .or_else(|| env_path("XDG_CONFIG_HOME"))
            .unwrap_or_else(|| home().join(".config"))
            .join("cursor/auth.json")
    }
}

fn command(dir: Option<&Path>) -> Command {
    let mut c = Command::new("cursor-agent");
    c.env_remove("CURSOR_API_KEY")
        .env_remove("CURSOR_API_KEY_FILE");
    if let Some(d) = dir {
        c.env(if MAC { "HOME" } else { "XDG_CONFIG_HOME" }, d);
    }
    c
}

pub(super) fn read(dir: Option<&Path>) -> Option<Value> {
    let cred = rjson(&auth_path(dir))?;
    cred["accessToken"].as_str()?;
    let email = cred["accessToken"]
        .as_str()
        .and_then(jwt)
        .and_then(|v| v["email"].as_str().map(String::from))
        .or_else(|| {
            let out = command(dir)
                .args(["status", "--format", "json"])
                .stderr(Stdio::null())
                .output()
                .ok()?;
            let v: Value = serde_json::from_slice(&out.stdout).ok()?;
            v["userInfo"]["email"].as_str().map(String::from)
        });
    Some(json!({"credentials": cred, "account": {"email": email}}))
}

pub(super) fn write(blob: &Value) -> Result<(), String> {
    let path = auth_path(None);
    let dir = path.parent().ok_or("invalid Cursor auth path")?;
    crate::store::mkdir(dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    wjson(&path, &blob["credentials"]).map_err(|e| e.to_string())
}

pub(super) fn email(blob: &Value) -> Option<String> {
    blob["credentials"]["accessToken"]
        .as_str()
        .and_then(jwt)
        .and_then(|v| v["email"].as_str().map(String::from))
        .or_else(|| blob["account"]["email"].as_str().map(String::from))
}

pub(super) fn expires(blob: &Value) -> Option<f64> {
    jwt(blob["credentials"]["accessToken"].as_str()?)?["exp"].as_f64()
}

pub(super) fn login(dir: Option<&Path>) -> bool {
    command(dir)
        .arg("login")
        .status()
        .is_ok_and(|s| s.success())
}

pub(crate) fn push(host: &str, blob: &Value) -> crate::push::Done {
    // Keep a newer credential for the same account; never emit remote contents.
    let read = r#"set -e
if [ "$(uname -s)" = Darwin ]; then d="$HOME/.cursor"; else d="${XDG_CONFIG_HOME:-$HOME/.config}/cursor"; fi
if [ -d "$d" ]; then chmod 700 "$d"; fi
if [ -f "$d/auth.json" ]; then chmod 600 "$d/auth.json" && cat "$d/auth.json"; fi
"#;
    let have = match crate::push::ssh(host, read, "") {
        Ok(out) => out,
        Err(d) => return d,
    };
    if let Ok(cred) = serde_json::from_str::<Value>(&have) {
        let theirs = json!({"credentials": cred});
        if keep_theirs(&theirs, blob) {
            return crate::push::Done::Ok("has a newer token, kept".into());
        }
    }
    crate::push::send_script(host, &push_script(blob))
}

fn keep_theirs(theirs: &Value, ours: &Value) -> bool {
    email(theirs).is_some()
        && email(theirs) == email(ours)
        && expires(theirs)
            .zip(expires(ours))
            .is_some_and(|(t, o)| t > o)
}

fn push_script(blob: &Value) -> String {
    format!(
        r#"set -eu
umask 077
if [ "$(uname -s)" = Darwin ]; then d="$HOME/.cursor"; else d="${{XDG_CONFIG_HOME:-$HOME/.config}}/cursor"; fi
mkdir -p "$d"
chmod 700 "$d"
t=$(mktemp "$d/.auth.json.subs.XXXXXX")
trap 'rm -f "$t"' EXIT
{payload}chmod 600 "$t"
if [ -f "$d/auth.json" ] && cmp -s "$t" "$d/auth.json"; then
  chmod 600 "$d/auth.json"
  echo "subs: unchanged"; exit 0
fi
mv -f "$t" "$d/auth.json"
echo "subs: updated"
"#,
        payload = crate::push::heredoc(
            "$t",
            &serde_json::to_string_pretty(&blob["credentials"]).unwrap_or_default()
        )
    )
}

pub(super) fn usage(blob: &Value) -> Result<Usage, String> {
    let token = blob["credentials"]["accessToken"]
        .as_str()
        .ok_or("no access token")?;
    // These are the same read-only DashboardService RPCs used by cursor-agent
    // /usage. Connect unary queries use POST, without changing account state.
    let (usage, plan) = std::thread::scope(|s| {
        let plan = s.spawn(|| rpc("GetPlanInfo", token));
        (
            rpc("GetCurrentPeriodUsage", token),
            plan.join()
                .unwrap_or_else(|_| Err("plan request failed".into())),
        )
    });
    let mut u = parse(
        usage.as_ref().unwrap_or(&Value::Null),
        plan.as_ref().unwrap_or(&Value::Null),
    );
    if let Err(e) = usage {
        u.spend.reason = Some(format!("Cursor usage unavailable: {e}"));
    }
    Ok(u)
}

fn rpc(method: &str, token: &str) -> Result<Value, String> {
    let url = format!("https://api2.cursor.sh/aiserver.v1.DashboardService/{method}");
    let mut c = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "15",
            "-H",
            "@-",
            "--data",
            "{}",
            "-w",
            "\n%{http_code}",
            &url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "curl unavailable")?;
    if let Some(mut stdin) = c.stdin.take() {
        let _ = write!(stdin, "Authorization: Bearer {token}\nContent-Type: application/json\nConnect-Protocol-Version: 1\n");
    }
    let out = c.wait_with_output().map_err(|_| "request failed")?;
    if !out.status.success() {
        return Err("request failed or timed out".into());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').ok_or("unreadable response")?;
    if code != "200" {
        return Err(format!("http {}", code.parse::<u16>().unwrap_or(0)));
    }
    serde_json::from_str(body).map_err(|_| "unreadable response".into())
}

pub(super) fn parse(v: &Value, plan: &Value) -> Usage {
    let num = |v: &Value| v.as_f64().filter(|n| n.is_finite() && *n >= 0.0);
    let reset = v["billingCycleEnd"]
        .as_f64()
        .or_else(|| v["billingCycleEnd"].as_str()?.parse().ok())
        .filter(|n| *n > 0.0)
        .map(|ms| ms / 1000.0);
    let p = &v["planUsage"];
    let pct = num(&p["totalPercentUsed"]).or_else(|| {
        let limit = num(&p["limit"])?;
        (limit > 0.0)
            .then(|| num(&p["includedSpend"]).map(|n| n / limit * 100.0))
            .flatten()
    });
    let spend = &v["spendLimitUsage"];
    let used = num(&spend["individualUsed"]).map(|n| n / 100.0);
    let cap = num(&spend["individualLimit"]).map(|n| n / 100.0);
    Usage {
        plan: plan["planInfo"]["planName"].as_str().map(String::from),
        windows: if p.is_object() {
            vec![Win {
                label: "monthly".into(),
                pct,
                reset,
            }]
        } else {
            vec![]
        },
        resets: None,
        extra: None,
        limited: pct.is_some_and(|p| p >= 100.0),
        spend: Spend {
            used,
            cap,
            currency: used.map(|_| "USD".into()),
            period: used.map(|_| "billing cycle (on-demand)".into()),
            reason: Some(
                if used.is_some() {
                    "on-demand only; excludes flat subscription"
                } else {
                    "Cursor did not report dollar usage"
                }
                .into(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_identity_expiry_and_usage() {
        // Synthetic JWT: {"email":"a@b.c","exp":200}
        let blob =
            json!({"credentials": {"accessToken": "x.eyJlbWFpbCI6ImFAYi5jIiwiZXhwIjoyMDB9.y"}});
        assert_eq!(Prov::parse("cursor"), Some(Prov::Cursor));
        assert_eq!(Prov::Cursor.email(&blob).as_deref(), Some("a@b.c"));
        assert_eq!(Prov::Cursor.expires(&blob), Some(200.0));
        let opaque = json!({"credentials": {"accessToken": "opaque"}, "account": {"email": "saved@example.com"}});
        assert_eq!(
            Prov::Cursor.email(&opaque).as_deref(),
            Some("saved@example.com")
        );
        assert_eq!(Prov::Cursor.expires(&opaque), None);
        let older =
            json!({"credentials": {"accessToken": "x.eyJlbWFpbCI6ImFAYi5jIiwiZXhwIjoxMDB9.y"}});
        assert!(keep_theirs(&blob, &older));
        assert!(!keep_theirs(&older, &blob));
        assert!(!keep_theirs(&opaque, &blob));
        assert!(!keep_theirs(&blob, &blob));
        let usage = parse(&json!({}), &json!({}));
        assert!(usage.windows.is_empty());
        assert!(usage.spend.used.is_none());
        assert!(!usage.limited);
    }

    #[test]
    fn cursor_login_and_read_share_an_isolated_auth_path() {
        let dir = Path::new("/tmp/subs-cursor-test");
        assert_eq!(
            auth_path(Some(dir)),
            dir.join(if MAC {
                ".cursor/auth.json"
            } else {
                "cursor/auth.json"
            })
        );
        let cmd = command(Some(dir));
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == if MAC { "HOME" } else { "XDG_CONFIG_HOME" }
                && v == Some(dir.as_os_str())));
        // Reading a JWT credential needs no CLI invocation or network request.
        let tmp = crate::store::mkdtemp(&std::env::temp_dir(), "subs-cursor-read-").unwrap();
        let cred = json!({"accessToken": "x.eyJlbWFpbCI6ImFAYi5jIiwiZXhwIjoyMDB9.y", "refreshToken": "synthetic"});
        wjson(&auth_path(Some(&tmp)), &cred).unwrap();
        let blob = Prov::Cursor.read(Some(&tmp)).unwrap();
        assert_eq!(blob["credentials"], cred);
        assert_eq!(Prov::Cursor.email(&blob).as_deref(), Some("a@b.c"));
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn cursor_script_installs_only_auth_atomically_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = crate::store::mkdtemp(&std::env::temp_dir(), "subs-cursor-push-").unwrap();
        let blob = json!({"credentials": {"accessToken": "synthetic", "refreshToken": "synthetic"}, "account": {"email": "a@x"}});
        let script = push_script(&blob);
        assert!(!script.contains("pkill"));
        let dir = tmp.join(if MAC { ".cursor" } else { "xdg/cursor" });
        std::fs::create_dir_all(&dir).unwrap();
        for expected in ["subs: updated", "subs: unchanged"] {
            // Repair permissions even when the credential is unchanged.
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            if dir.join("auth.json").exists() {
                std::fs::set_permissions(
                    dir.join("auth.json"),
                    std::fs::Permissions::from_mode(0o644),
                )
                .unwrap();
            }
            let mut child = Command::new("sh")
                .arg("-s")
                .env("HOME", &tmp)
                .env("XDG_CONFIG_HOME", tmp.join("xdg"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), expected);
        }
        let path = tmp.join(if crate::prov::MAC {
            ".cursor/auth.json"
        } else {
            "xdg/cursor/auth.json"
        });
        assert_eq!(crate::store::rjson(&path).unwrap(), blob["credentials"]);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn plan_without_invented_spend() {
        let u = parse(&json!({}), &json!({"planInfo":{"planName":"Pro"}}));
        assert_eq!(u.plan.as_deref(), Some("Pro"));
        assert!(u.spend.used.is_none());
        assert!(u.windows.is_empty());
    }
    #[test]
    fn usage_percentage_and_on_demand_cents() {
        let u = parse(
            &json!({"planUsage":{"totalPercentUsed":35},"billingCycleEnd":"1793361600000","spendLimitUsage":{"individualUsed":125,"individualLimit":1000}}),
            &json!({"planInfo":{"planName":"Pro"}}),
        );
        assert_eq!(u.windows[0].pct, Some(35.0));
        assert_eq!(u.windows[0].reset, Some(1793361600.0));
        assert_eq!(u.spend.used, Some(1.25));
        assert_eq!(u.spend.cap, Some(10.0));
    }
}
