use super::*;

pub(super) fn usage(blob: &Value) -> Result<Usage, String> {
    let t = &blob["tokens"];
    let token = t["access_token"].as_str().ok_or("no access token")?;
    let u = get(
        "https://chatgpt.com/backend-api/wham/usage",
        &[
            ("Authorization", &format!("Bearer {token}")),
            ("ChatGPT-Account-Id", t["account_id"].as_str().unwrap_or("")),
        ],
    )?;
    parse(&u)
}

fn parse(u: &Value) -> Result<Usage, String> {
    let rl = &u["rate_limit"];
    let windows = ["primary_window", "secondary_window"]
        .iter()
        .map(|k| &rl[*k])
        .filter(|w| w.is_object())
        .map(|w| Win {
            label: span(w["limit_window_seconds"].as_f64()),
            pct: w["used_percent"].as_f64(),
            reset: w["reset_at"].as_f64(),
        })
        .collect();
    let cr = &u["credits"];
    let extra = if cr["unlimited"].as_bool() == Some(true) {
        Some("credits ∞".to_string())
    } else if cr["has_credits"].as_bool() == Some(true) {
        let bal = cr["balance"]
            .as_f64()
            .or_else(|| cr["balance"].as_str()?.parse().ok());
        Some(
            bal.map(|b| format!("credits {b:.0}"))
                .unwrap_or_else(|| "credits unknown".into()),
        )
    } else {
        None
    };
    Ok(Usage {
        spend: Spend::unknown("flat subscription; dollar spend not exposed"),
        plan: u["plan_type"].as_str().map(String::from),
        windows,
        resets: u["rate_limit_reset_credits"]["available_count"].as_i64(),
        extra,
        limited: rl["limit_reached"].as_bool() == Some(true),
    })
}

/// Spend one banked full reset on the live login, through Codex's own
/// app-server. Nothing is spent when no window is used up.
pub(crate) fn reset() -> Result<&'static str, String> {
    let mut c = Command::new("codex")
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("codex: {e}"))?;
    let r = rpc(&mut c);
    drop(c.stdin.take());
    let _ = c.kill();
    let _ = c.wait();
    match r?["outcome"].as_str() {
        Some("reset") | Some("alreadyRedeemed") => Ok("usage reset"),
        Some("nothingToReset") => Err("nothing to reset: no usage window is used up".into()),
        Some("noCredit") => Err("no full resets left".into()),
        _ => Err("codex did not confirm the reset".into()),
    }
}

/// initialize, then consume a reset credit; the consume result.
fn rpc(c: &mut std::process::Child) -> Result<Value, String> {
    use std::io::{BufRead, BufReader};
    let mut stdin = c.stdin.take().ok_or("codex app-server: no stdin")?;
    let out = BufReader::new(c.stdout.take().ok_or("codex app-server: no stdout")?);
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v["id"].is_number() && tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    let mut send = |v: Value| writeln!(stdin, "{v}").map_err(|e| format!("codex app-server: {e}"));
    let wait = |id: u64| -> Result<Value, String> {
        loop {
            let v = rx
                .recv_timeout(std::time::Duration::from_secs(60))
                .map_err(|_| "codex app-server did not answer".to_string())?;
            if v["id"] == id {
                if let Some(e) = v.get("error") {
                    return Err(format!(
                        "codex: {}",
                        e["message"].as_str().unwrap_or("error")
                    ));
                }
                return Ok(v["result"].clone());
            }
        }
    };
    send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"clientInfo": {"name": "subs", "version": env!("CARGO_PKG_VERSION")}}}))?;
    wait(1)?;
    send(json!({"jsonrpc": "2.0", "method": "initialized"}))?;
    // One key per attempt: a retried request cannot spend a second credit.
    send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimitResetCredit/consume",
        "params": {"idempotencyKey": idempotency_key()?}}),
    )?;
    wait(2)
}

/// A random UUID (v4) for the reset request.
fn idempotency_key() -> Result<String, String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subscription_windows_are_not_dollars() {
        let u = parse(&json!({"plan_type":"pro", "rate_limit":{"primary_window":{"limit_window_seconds":18000,"used_percent":25,"reset_at":200},"secondary_window":{"limit_window_seconds":604800,"used_percent":12}},"credits":{"has_credits":true,"balance":"4.5"}})).unwrap();
        assert_eq!(u.plan.as_deref(), Some("pro"));
        assert_eq!(u.windows[0].pct, Some(25.0));
        assert_eq!(u.windows[1].label, "7d");
        assert!(u.spend.used.is_none());
        assert!(u.spend.reason.is_some());
    }
    #[test]
    fn reset_keys_are_fresh_uuids() {
        let (a, b) = (idempotency_key().unwrap(), idempotency_key().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
    }
    #[test]
    fn missing_usage_is_not_zero() {
        let u = parse(&json!({"rate_limit":{"primary_window":{}}, "credits":{"has_credits":true}}))
            .unwrap();
        assert_eq!(u.windows[0].pct, None);
        assert_eq!(u.extra.as_deref(), Some("credits unknown"));
        assert!(u.plan.is_none());
    }
}
