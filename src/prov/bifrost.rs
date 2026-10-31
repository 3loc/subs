//! Read-only governance accounting. Credentials are fetched anew, never saved.
use super::*;

pub const KEY: &str = "neocat-demo-dev-20261030";
const URL: &str = "https://bifrost.fansvine.com/api/governance/virtual-keys";

pub fn usage() -> Result<Usage, String> {
    let run = || -> Result<Usage, String> {
        // Same SSM path and operator identity used by Fansvine's Bifrost tooling.
        // Empty SUBS_BIFROST_SSM_HOST uses local AWS; default uses the existing host.
        let host = std::env::var("SUBS_BIFROST_SSM_HOST").unwrap_or_else(|_| "fansvine".into());
        let mut c = if host.is_empty() {
            Command::new("aws")
        } else {
            let mut c = Command::new("timeout");
            c.args([
                "20",
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-o",
                "RemoteCommand=none",
                "-T",
                "--",
                &host,
                "aws",
            ]);
            c
        };
        let out = c
            .args([
                "ssm",
                "get-parameters",
                "--names",
                "/bifrost/prod/ADMIN_USERNAME",
                "/bifrost/prod/ADMIN_PASSWORD",
                "--with-decryption",
                "--region",
                "us-east-1",
                "--output",
                "json",
                "--cli-connect-timeout",
                "4",
                "--cli-read-timeout",
                "6",
            ])
            .env("AWS_PAGER", "")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| "admin credential loader unavailable")?;
        if !out.status.success() {
            return Err("admin credentials not readable non-interactively".into());
        }
        let v: Value = serde_json::from_slice(&out.stdout)
            .map_err(|_| "admin credential response unreadable")?;
        let params = v["Parameters"]
            .as_array()
            .ok_or("admin credentials missing")?;
        let secret = |name: &str| {
            params
                .iter()
                .find(|p| p["Name"] == name)
                .and_then(|p| p["Value"].as_str())
                .filter(|s| !s.is_empty())
        };
        let user = secret("/bifrost/prod/ADMIN_USERNAME").ok_or("admin username not readable")?;
        let pass = secret("/bifrost/prod/ADMIN_PASSWORD").ok_or("admin password not readable")?;
        let auth = format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()));
        // Pagination avoids falsely reporting a missing key after the first page.
        let mut offset = 0;
        loop {
            let v = get(
                &format!("{URL}?limit=100&offset={offset}"),
                &[("Authorization", &auth)],
            )?;
            let keys = v["virtual_keys"]
                .as_array()
                .ok_or("virtual key response unreadable")?;
            if let Some(k) = keys.iter().find(|k| k["name"] == KEY) {
                // List responses can omit budget relations; detail hydrates them.
                let id = k["id"]
                    .as_str()
                    .filter(|s| {
                        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                    .ok_or("virtual key id missing or invalid")?;
                let detail = get(&format!("{URL}/{id}"), &[("Authorization", &auth)])?;
                return parse(&detail);
            }
            offset += keys.len();
            if keys.is_empty()
                || v["total_count"]
                    .as_u64()
                    .is_none_or(|total| offset as u64 >= total)
            {
                return Err("virtual key not deployed or not visible".into());
            }
            if offset >= 10000 {
                return Err("virtual key pagination limit reached".into());
            }
        }
    };
    run().map_err(|e| format!("unavailable: {e}"))
}

fn parse(v: &Value) -> Result<Usage, String> {
    let k = if v["virtual_key"].is_object() {
        &v["virtual_key"]
    } else {
        v
    };
    if k["name"] != KEY {
        return Err("virtual key detail mismatch".into());
    }
    let b = if k["budget"].is_object() {
        &k["budget"]
    } else {
        let budgets = k["budgets"]
            .as_array()
            .ok_or("virtual key budget not reported")?;
        if budgets.len() != 1 {
            return Err("expected one virtual key budget".into());
        }
        &budgets[0]
    };
    let amount = |field: &str| b[field].as_f64().filter(|n| n.is_finite() && *n >= 0.0);
    let used = amount("current_usage").ok_or("budget usage not reported")?;
    let cap = amount("max_limit")
        .filter(|n| *n > 0.0)
        .ok_or("budget cap not reported")?;
    let period = b["reset_duration"].as_str().map(String::from);
    Ok(Usage {
        plan: Some("OpenRouter dev virtual key".into()),
        spend: Spend {
            used: Some(used),
            cap: Some(cap),
            currency: Some("USD".into()),
            period,
            reason: None,
        },
        windows: vec![Win {
            label: "budget".into(),
            pct: Some(used / cap * 100.0),
            reset: None,
        }],
        resets: None,
        extra: (k["is_active"].as_bool() == Some(false)).then(|| "key inactive".into()),
        limited: used >= cap,
    })
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ABC[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_dollars_and_cap() {
        let u = parse(&json!({"virtual_key":{"name":KEY,"budget":{"current_usage":1.25,"max_limit":10,"reset_duration":"1M"}}})).unwrap();
        assert_eq!(u.spend.used, Some(1.25));
        assert_eq!(u.spend.cap, Some(10.0));
        assert_eq!(u.windows[0].pct, Some(12.5));
        assert!(!u.limited);
        assert_eq!(u.spend.currency.as_deref(), Some("USD"));
    }
    #[test]
    fn plural_budget_and_exhaustion() {
        let u =
            parse(&json!({"name":KEY,"budgets":[{"current_usage":10,"max_limit":10}]})).unwrap();
        assert!(u.limited);
    }
    #[test]
    fn missing_and_invalid_budget_are_unknown() {
        for b in [
            json!({}),
            json!({"max_limit":10}),
            json!({"max_limit":0,"current_usage":0}),
            json!({"max_limit":10,"current_usage":-1}),
        ] {
            assert!(parse(&json!({"name":KEY,"budget":b})).is_err());
        }
        assert!(
            parse(&json!({"name":"another-key","budget":{"current_usage":1,"max_limit":10}}))
                .is_err()
        );
    }
    #[test]
    fn encodes_basic_auth() {
        assert_eq!(base64(b"a:b"), "YTpi");
        assert_eq!(base64(b"a:"), "YTo=");
        assert_eq!(base64(b"a"), "YQ==");
    }
}
