use super::*;

pub(super) fn usage(blob: &Value) -> Result<Usage, String> {
    let o = &blob["credentials"]["claudeAiOauth"];
    let token = o["accessToken"].as_str().ok_or("no access token")?;
    let u = get("https://api.anthropic.com/api/oauth/usage", &[("Authorization", &format!("Bearer {token}")), BETA])?;
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
    Ok(Usage { spend: Spend::unknown("flat subscription; dollar spend not exposed"), plan: Some(plan), windows, resets: None, extra, limited })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let u = parse(&json!({"five_hour":{},"extra_usage":{"is_enabled":true}}), &json!({})).unwrap();
        assert!(u.windows[0].pct.is_none());
        assert_eq!(u.extra.as_deref(), Some("extra credits unknown"));
    }
}
