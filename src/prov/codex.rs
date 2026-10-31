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
    fn missing_usage_is_not_zero() {
        let u = parse(&json!({"rate_limit":{"primary_window":{}}, "credits":{"has_credits":true}}))
            .unwrap();
        assert_eq!(u.windows[0].pct, None);
        assert_eq!(u.extra.as_deref(), Some("credits unknown"));
        assert!(u.plan.is_none());
    }
}
