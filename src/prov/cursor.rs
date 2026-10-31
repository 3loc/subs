use super::*;

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
