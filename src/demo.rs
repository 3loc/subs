//! SUBS_DEMO=1: made-up accounts and hosts, for the README recordings.
//! Nothing is read or written and no host is contacted.

use crate::prov::{Prov, Usage, Win};
use crate::push::Done;
use crate::status::Row;
use crate::store::now;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

pub fn on() -> bool {
    std::env::var_os("SUBS_DEMO").is_some_and(|v| !v.is_empty() && v != "0")
}

fn win(label: &str, used: f64, hours: f64) -> Win {
    Win {
        label: label.into(),
        pct: Some(used),
        reset: Some(now() + hours * 3600.0),
    }
}

fn row(
    prov: Prov,
    email: &str,
    active: bool,
    plan: &str,
    windows: Vec<Win>,
    resets: Option<i64>,
) -> Row {
    let usage = Usage {
        spend: crate::prov::Spend::unknown("flat subscription; dollar spend not exposed"),
        plan: Some(plan.into()),
        windows,
        resets,
        extra: None,
        limited: false,
    };
    Row {
        prov,
        email: email.into(),
        active,
        password: true,
        token: None,
        usage: Ok(usage),
    }
}

/// The demo's active Codex login, switched by `use`.
pub static ACTIVE: Mutex<&str> = Mutex::new("ana@example.com");

pub fn switch(email: &str) -> String {
    let mut a = ACTIVE.lock().unwrap();
    let from = *a;
    *a = ["ana@example.com", "ben@example.com", "dev@example.com"]
        .into_iter()
        .find(|e| *e == email)
        .unwrap_or(from);
    format!("codex: {from} → {email}")
}

pub fn rows() -> Vec<Row> {
    thread::sleep(Duration::from_millis(900));
    let active = *ACTIVE.lock().unwrap();
    let on = |e: &str| e == active;
    vec![
        row(
            Prov::Codex,
            "ana@example.com",
            on("ana@example.com"),
            "pro",
            vec![win("7d", 88.0, 30.0)],
            Some(0),
        ),
        row(
            Prov::Codex,
            "ben@example.com",
            on("ben@example.com"),
            "plus",
            vec![win("7d", 12.0, 140.0)],
            Some(2),
        ),
        row(
            Prov::Codex,
            "dev@example.com",
            false,
            "free",
            vec![win("30d", 41.0, 400.0)],
            None,
        ),
        row(
            Prov::Claude,
            "ana@example.com",
            true,
            "max 20x",
            vec![win("5h", 37.0, 2.6), win("7d", 23.0, 61.0)],
            None,
        ),
    ]
}

pub const HOSTS: [&str; 6] = [
    "laptop", "desktop", "buildbox", "gpu-01", "gpu-02", "studio",
];

/// Pretend to push: each host answers after a while, one is asleep.
pub fn start(tx: Sender<(usize, Done)>) {
    let plan = [
        (700, "updated"),
        (1300, "updated"),
        (1900, "updated · restart 1 codex session"),
        (2400, "unchanged"),
        (2900, "updated"),
        (3600, ""),
    ];
    for (i, (ms, msg)) in plan.into_iter().enumerate() {
        let tx = tx.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(ms));
            let _ = tx.send((
                i,
                if msg.is_empty() {
                    Done::Offline
                } else {
                    Done::Ok(msg.into())
                },
            ));
        });
    }
}
