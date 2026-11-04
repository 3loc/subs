//! One row per known account, usage fetched in parallel.

use crate::prov::{Prov, Usage, ALL};
use crate::store::{self, now, rjson, snap_path, wjson};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::thread;

pub enum UsageState {
    Available(Usage),
    Unsupported,
    InactiveOAuth(String),
    Error(String),
}

impl UsageState {
    pub fn reported(&self) -> Option<&Usage> {
        match self {
            Self::Available(u) => Some(u),
            _ => None,
        }
    }
}

pub struct Row {
    pub prov: Prov,
    pub email: String,
    pub active: bool,
    pub password: bool,
    pub usage: UsageState,
    pub token: Option<Value>,
}

/// Every saved or live account. The live one is re-saved so its copy stays current.
pub fn status() -> Vec<Row> {
    if crate::demo::on() {
        return crate::demo::rows();
    }
    let token = crate::prov::claude::health();
    let has_token = token["present"] == true;
    let acc = store::accounts();
    let mut todo = vec![];
    for p in ALL {
        let live = p.read(None);
        let cur = live.as_ref().and_then(|b| p.email(b));
        // A cleared credential must not overwrite the saved copy.
        if let (Some(c), Some(b)) = (&cur, &live) {
            if p.has_token(b) {
                let _ = wjson(&snap_path(p, c), b);
            }
        }
        let mut emails: BTreeSet<String> = store::snaps(p).into_iter().collect();
        if let Some(m) = acc[p.name()].as_object() {
            emails.extend(m.keys().cloned());
        }
        emails.extend(cur.clone());
        if emails.is_empty() && !(p == Prov::Claude && has_token) {
            todo.push((p, String::new(), false, false, None));
        }
        for email in emails {
            let current = cur.as_deref() == Some(email.as_str());
            let active = current && !(p == Prov::Claude && has_token);
            let blob = if current {
                live.clone()
            } else {
                rjson(&snap_path(p, &email))
            };
            let password = acc[p.name()][email.as_str()]["password"]
                .as_str()
                .is_some_and(|s| !s.is_empty());
            todo.push((p, email, active, password, blob));
        }
    }
    let mut rows: Vec<Row> = thread::scope(|s| {
        let jobs: Vec<_> = todo
            .into_iter()
            .map(|(prov, email, active, password, blob)| {
                s.spawn(move || {
                    let result = match &blob {
                        Some(b) => fetch(prov, b, active),
                        None => Err("not logged in".into()),
                    };
                    let usage =
                        classify_usage(result, prov == Prov::Claude && has_token && !active);
                    Row {
                        prov,
                        email,
                        active,
                        password,
                        usage,
                        token: None,
                    }
                })
            })
            .collect();
        jobs.into_iter()
            .map(|j| j.join().expect("fetch thread"))
            .collect()
    });
    if has_token {
        rows.push(Row {
            prov: Prov::Claude,
            email: String::new(),
            active: token["state"] == "unverified",
            password: false,
            usage: if token["state"] == "unverified" {
                UsageState::Unsupported
            } else {
                UsageState::Error(
                    token["error"]
                        .as_str()
                        .unwrap_or("setup-token is unusable")
                        .into(),
                )
            },
            token: Some(token),
        });
    } else if let Some(r) = rows.iter_mut().find(|r| r.prov == Prov::Claude) {
        r.token = Some(token);
    }
    rows
}

fn classify_usage(result: Result<Usage, String>, inactive_oauth: bool) -> UsageState {
    match result {
        Ok(u) => UsageState::Available(u),
        Err(e) if inactive_oauth => UsageState::InactiveOAuth(e),
        Err(e) => UsageState::Error(e),
    }
}

fn fetch(p: Prov, blob: &Value, active: bool) -> Result<Usage, String> {
    if !p.has_token(blob) {
        return Err(format!(
            "sign in required · subs login {} {}",
            p.name(),
            p.email(blob).unwrap_or_default()
        ));
    }
    if p.expires(blob).is_some_and(|t| t < now()) {
        return Err(if active {
            format!("session expired · run {} to refresh", p.name())
        } else {
            "saved · select to refresh usage".into()
        });
    }
    p.usage(blob)
}

/// Time until `t`: 41m, 2h 14m, 3d 4h.
pub fn until(t: f64) -> String {
    let s = (t - now()).max(0.0) as u64;
    let (big, small) = match s {
        86400.. => ((s / 86400, "d"), (s % 86400 / 3600, "h")),
        3600.. => ((s / 3600, "h"), (s % 3600 / 60, "m")),
        _ => return format!("{}m", s / 60),
    };
    match small.0 {
        0 => format!("{}{}", big.0, big.1),
        n => format!("{}{} {n}{}", big.0, big.1, small.1),
    }
}

pub fn to_json(rows: &[Row]) -> Value {
    rows.iter()
        .map(|r| {
            let mut o = json!({"provider": r.prov.name(), "email": r.email, "active": r.active, "password": r.password});
            if let Some(token) = &r.token { o["setup_token"] = token.clone(); }
            match &r.usage {
                UsageState::Error(e) => {
                    o["usage_status"] = json!("error");
                    o["err"] = json!(e);
                    o["spend"] = crate::prov::Spend::unknown(e).json();
                },
                UsageState::Unsupported => {
                    o["usage_status"] = json!("unsupported");
                    o["windows"] = Value::Null;
                    o["spend"] = crate::prov::Spend::unknown("token does not support usage reporting").json();
                },
                UsageState::InactiveOAuth(reason) => {
                    o["usage_status"] = json!("inactive");
                    o["inactive_reason"] = json!(reason);
                    o["windows"] = Value::Null;
                    o["spend"] = crate::prov::Spend::unknown("saved OAuth login is inactive").json();
                },
                UsageState::Available(u) => {
                    o["usage_status"] = json!("available");
                    o["plan"] = json!(u.plan);
                    o["spend"] = u.spend.json();
                    o["windows"] = u.windows.iter().map(|w| json!([w.label, w.pct, w.reset])).collect();
                    o["resets"] = json!(u.resets);
                    o["extra"] = json!(u.extra);
                    o["limited"] = json!(u.limited);
                }
            }
            o
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_saved_login_is_distinct_from_active_session_failure() {
        let blob =
            json!({"credentials": {"claudeAiOauth": {"accessToken": "synthetic", "expiresAt": 1}}});
        assert_eq!(
            fetch(Prov::Claude, &blob, false).err().unwrap(),
            "saved · select to refresh usage"
        );
        assert_eq!(
            fetch(Prov::Claude, &blob, true).err().unwrap(),
            "session expired · run claude to refresh"
        );
    }
    #[test]
    fn cleared_claude_credential_is_signed_out_not_expired() {
        let blob = json!({"credentials": {"claudeAiOauth": {"accessToken": "", "refreshToken": "", "expiresAt": 0}}, "account": {"emailAddress": "a@example.com"}});
        let e = fetch(Prov::Claude, &blob, false).err().unwrap();
        assert_eq!(e, "sign in required · subs login claude a@example.com");
        assert!(!Prov::Claude.has_token(&blob));
    }
    #[test]
    fn unavailable_spend_is_null_with_reason() {
        let rows = [Row {
            prov: Prov::Codex,
            email: "a@example.com".into(),
            active: false,
            password: false,
            token: None,
            usage: UsageState::Error("not logged in".into()),
        }];
        let v = to_json(&rows);
        assert!(v[0]["spend"]["used"].is_null());
        assert!(v[0]["spend"]["cap"].is_null());
        assert_eq!(v[0]["spend"]["reason"], "not logged in");
    }
}
