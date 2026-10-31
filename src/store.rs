//! The state directory: accounts.json plus one saved credential per login.

use crate::prov::Prov;
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("/"))
}

/// $SUBS_HOME, else ${XDG_DATA_HOME:-~/.local/share}/subs.
pub fn root() -> PathBuf {
    env_path("SUBS_HOME").unwrap_or_else(|| {
        env_path("XDG_DATA_HOME")
            .unwrap_or_else(|| home().join(".local/share"))
            .join("subs")
    })
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn mkdir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

fn unique() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn mkdtemp(dir: &Path, prefix: &str) -> io::Result<PathBuf> {
    loop {
        let p = dir.join(format!("{prefix}{}", unique()));
        match DirBuilder::new().mode(0o700).create(&p) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            r => return r.map(|_| p),
        }
    }
}

pub fn rjson(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Atomic write through a 0600 temp file beside the target.
pub fn wjson(path: &Path, v: &Value) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    mkdir(dir)?;
    let tmp = dir.join(format!(".subs-{}", unique()));
    let res = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(v)?.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

pub fn accounts() -> Value {
    rjson(&root().join("accounts.json"))
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

pub fn save_accounts(a: &Value) -> io::Result<()> {
    wjson(&root().join("accounts.json"), a)
}

pub fn snap_path(p: Prov, email: &str) -> PathBuf {
    root().join(p.name()).join(format!("{email}.json"))
}

pub fn snaps(p: Prov) -> Vec<String> {
    let Ok(rd) = fs::read_dir(root().join(p.name())) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(String::from)
        })
        .filter(|n| !n.starts_with('.'))
        .collect()
}
