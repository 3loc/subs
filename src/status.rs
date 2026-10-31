//! One row per known account, usage fetched in parallel.

use crate::prov::{Prov, Usage, ALL};
use crate::store::{self, now, rjson, snap_path, wjson};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::thread;

pub struct Row {
    pub prov: Prov,
    pub email: String,
    pub active: bool,
    pub password: bool,
    pub usage: Result<Usage, String>,
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
        if let (Some(c), Some(b)) = (&cur, &live) {
            let _ = wjson(&snap_path(p, c), b);
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
    todo.push((
        Prov::Bifrost,
        crate::prov::bifrost::KEY.into(),
        false,
        false,
        Some(Value::Null),
    ));
    let mut rows: Vec<Row> = thread::scope(|s| {
        let jobs: Vec<_> = todo
            .into_iter()
            .map(|(prov, email, active, password, blob)| {
                s.spawn(move || {
                    let usage = match &blob {
                        Some(b) => fetch(prov, b, active),
                        None => Err("not logged in".into()),
                    };
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
            usage: Err(
                "setup-token account and usage unavailable (no verification request sent)".into(),
            ),
            token: Some(token),
        });
    } else if let Some(r) = rows.iter_mut().find(|r| r.prov == Prov::Claude) {
        r.token = Some(token);
    }
    rows
}

fn fetch(p: Prov, blob: &Value, active: bool) -> Result<Usage, String> {
    if p.expires(blob).is_some_and(|t| t < now()) {
        return Err(if active {
            format!(
                "token expired · run {} once, or sign in again with l",
                p.name()
            )
        } else {
            "token expired · refreshes once in use".into()
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
                Err(e) => {
                    o["err"] = json!(e);
                    o["spend"] = crate::prov::Spend::unknown(e).json();
                },
                Ok(u) => {
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
    fn unavailable_spend_is_null_with_reason() {
        let rows = [Row {
            prov: Prov::Bifrost,
            email: crate::prov::bifrost::KEY.into(),
            active: false,
            password: false,
            token: None,
            usage: Err("unavailable: virtual key not deployed or not visible".into()),
        }];
        let v = to_json(&rows);
        assert!(v[0]["spend"]["used"].is_null());
        assert!(v[0]["spend"]["cap"].is_null());
        assert_eq!(
            v[0]["spend"]["reason"],
            rows[0].usage.as_ref().err().unwrap().as_str()
        );
        assert!(Prov::parse("bifrost").is_none());
    }
}
