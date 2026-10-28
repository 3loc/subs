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
}

/// Every saved or live account. The live one is re-saved so its copy stays current.
pub fn status() -> Vec<Row> {
    if crate::demo::on() {
        return crate::demo::rows();
    }
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
        for email in emails {
            let active = cur.as_deref() == Some(email.as_str());
            let blob = if active { live.clone() } else { rjson(&snap_path(p, &email)) };
            let password = acc[p.name()][email.as_str()]["password"].as_str().is_some_and(|s| !s.is_empty());
            todo.push((p, email, active, password, blob));
        }
    }
    thread::scope(|s| {
        let jobs: Vec<_> = todo
            .into_iter()
            .map(|(prov, email, active, password, blob)| {
                s.spawn(move || {
                    let usage = match &blob {
                        Some(b) => fetch(prov, b, active),
                        None => Err("not logged in".into()),
                    };
                    Row { prov, email, active, password, usage }
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().expect("fetch thread")).collect()
    })
}

fn fetch(p: Prov, blob: &Value, active: bool) -> Result<Usage, String> {
    if p.expires(blob).is_some_and(|t| t < now()) {
        return Err(if active {
            format!("token expired · run {} once, or sign in again with l", p.name())
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
            match &r.usage {
                Err(e) => o["err"] = json!(e),
                Ok(u) => {
                    o["plan"] = json!(u.plan);
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
