//! CLI tests use isolated homes, fake credentials and stubbed provider/network tools.
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const FAKE: &str = "sk-ant-oat01-FAKE-integration-test-only";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "subs-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        let f = Self(p);
        fs::create_dir(f.0.join("bin")).unwrap();
        fs::create_dir(f.0.join("remote")).unwrap();
        for tool in [
            "aws",
            "curl",
            "ssh",
            "codex",
            "cursor-agent",
            "claude",
            "security",
        ] {
            f.stub(tool, "exit 1\n");
        }
        f
    }
    fn stub(&self, name: &str, body: &str) {
        let p = self.0.join("bin").join(name);
        fs::write(&p, format!("#!/bin/sh\nset -eu\n{body}")).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_subs"));
        c.env_clear()
            .env("HOME", &self.0)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env("XDG_CONFIG_HOME", self.0.join(".config"))
            .env("SUBS_HOME", self.0.join("state"))
            .env("CODEX_HOME", self.0.join(".codex"))
            .env("CLAUDE_CONFIG_DIR", self.0.join(".claude"))
            .env("SUBS_BIFROST_SSM_HOST", "")
            .env("TEST_REMOTE_HOME", self.0.join("remote"))
            .env("ANTHROPIC_API_KEY", "FAKE-API-KEY-DO-NOT-PRINT")
            .current_dir(&self.0);
        c
    }
    fn run(&self, args: &[&str], input: &str) -> Output {
        let mut c = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = c.wait_with_output().unwrap();
        for bytes in [&out.stdout, &out.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(FAKE), "setup-token leaked");
            assert!(
                !text.contains("FAKE-API-KEY-DO-NOT-PRINT"),
                "API key leaked"
            );
        }
        out
    }
    fn install(&self) {
        let out = self.run(&["login", "claude", "--token"], &format!("{FAKE}\n"));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("ANTHROPIC_API_KEY is set"));
    }
    fn env_path(&self) -> PathBuf {
        self.0.join(".config/subs/claude.env")
    }
    fn hosts(&self) {
        fs::write(self.0.join(".config/subs/hosts"), "fake-vm claude\n").unwrap();
    }
    fn json(&self) -> Value {
        let out = self.run(&["json"], "");
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn token_row(v: &Value) -> &Value {
    v.as_array()
        .unwrap()
        .iter()
        .find(|r| r["setup_token"].is_object())
        .unwrap()
}

#[test]
fn stdin_install_status_json_and_private_storage() {
    let f = Fixture::new();
    assert_eq!(token_row(&f.json())["setup_token"]["present"], false);
    f.install();
    let p = f.env_path();
    assert_eq!(
        fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fs::read_to_string(p)
        .unwrap()
        .contains("unset ANTHROPIC_API_KEY"));
    let v = f.json();
    let r = token_row(&v);
    assert_eq!(r["active"], true);
    assert_eq!(r["setup_token"]["state"], "unverified");
    assert!(r["setup_token"]["valid"].is_null());
    let installed = r["setup_token"]["installed_at"].as_u64().unwrap();
    assert_eq!(
        r["setup_token"]["estimated_expires_at"],
        installed + 365 * 86400
    );
    let out = f.run(&["status"], "");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("setup-token: unverified"));
    assert!(text.contains("estimated expiry in"));
    assert_eq!(fs::read_dir(f.0.join("state")).unwrap().count(), 0);
}

#[test]
fn bad_permissions_block_push_and_status_reports_why() {
    let f = Fixture::new();
    f.install();
    f.hosts();
    f.stub("ssh", "touch \"$HOME/ssh-called\"; exit 1\n");
    fs::set_permissions(f.env_path(), fs::Permissions::from_mode(0o644)).unwrap();
    let out = f.run(&["push", "claude"], "");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("chmod 600"));
    assert!(!f.0.join("ssh-called").exists());
    let v = f.json();
    assert_eq!(token_row(&v)["setup_token"]["state"], "unusable");
    assert_eq!(token_row(&v)["active"], false);
}

#[test]
fn mocked_ssh_installs_on_fake_vm_preserves_age_and_is_idempotent() {
    let f = Fixture::new();
    f.install();
    f.hosts();
    f.stub(
        "ssh",
        r#"test "${ANTHROPIC_API_KEY+x}" != x
printf '%s\n' "$@" > "$HOME/ssh-args"
for last do :; done
test "$last" = 'sh -s'
HOME="$TEST_REMOTE_HOME" XDG_CONFIG_HOME="$TEST_REMOTE_HOME/.config" exec /bin/sh -s
"#,
    );
    for _ in 0..2 {
        assert!(f.run(&["push", "claude"], "").status.success());
    }
    let remote = f.0.join("remote/.config/subs/claude.env");
    assert_eq!(fs::read(&remote).unwrap(), fs::read(f.env_path()).unwrap());
    assert_eq!(
        fs::metadata(remote).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let rc = fs::read_to_string(f.0.join("remote/.bashrc")).unwrap();
    assert_eq!(rc.matches("# subs Claude").count(), 1);
    let args = fs::read_to_string(f.0.join("ssh-args")).unwrap();
    assert!(args.contains("fake-vm"));
    assert!(!args.contains(FAKE));
    assert!(!f.0.join("remote/.claude/.credentials.json").exists());
}

#[test]
fn remote_failure_or_echo_cannot_leak_the_token() {
    let f = Fixture::new();
    f.install();
    f.hosts();
    f.stub("ssh", "cat >&2; exit 3\n");
    let out = f.run(&["push", "claude"], "");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("remote credential installation failed"));
    f.stub(
        "ssh",
        &format!("cat >/dev/null; printf 'subs: {FAKE}\\n'\n"),
    );
    assert!(f.run(&["push", "claude"], "").status.success());
}

#[test]
fn newer_remote_oauth_is_kept_but_managed_token_is_disabled() {
    let f = Fixture::new();
    f.install();
    f.hosts();
    let account = serde_json::json!({"emailAddress": "test@example.invalid"});
    let old = serde_json::json!({"claudeAiOauth": {"accessToken": "fake-old", "expiresAt": 1}});
    let new = serde_json::json!({"claudeAiOauth": {"accessToken": "fake-new", "expiresAt": 2}});
    fs::create_dir_all(f.0.join("state/claude")).unwrap();
    fs::write(
        f.0.join("state/claude/test@example.invalid.json"),
        serde_json::json!({"credentials": old, "account": account}).to_string(),
    )
    .unwrap();
    fs::create_dir_all(f.0.join("remote/.claude")).unwrap();
    let remote_creds = f.0.join("remote/.claude/.credentials.json");
    fs::write(&remote_creds, new.to_string()).unwrap();
    fs::write(
        f.0.join("remote/.claude.json"),
        serde_json::json!({"oauthAccount": account}).to_string(),
    )
    .unwrap();
    fs::create_dir_all(f.0.join("remote/.config/subs")).unwrap();
    let remote_env = f.0.join("remote/.config/subs/claude.env");
    fs::copy(f.env_path(), &remote_env).unwrap();
    assert!(f
        .run(&["use", "claude", "test@example.invalid", "--local"], "")
        .status
        .success());
    f.stub(
        "ssh",
        r#"for last do :; done
if [ "$last" = 'sh -s' ]; then
  HOME="$TEST_REMOTE_HOME" XDG_CONFIG_HOME="$TEST_REMOTE_HOME/.config" exec /bin/sh -s
fi
cat "$TEST_REMOTE_HOME/.claude/.credentials.json"
printf '\nSUBS_SPLIT\n'
cat "$TEST_REMOTE_HOME/.claude.json"
"#,
    );
    let out = f.run(&["push", "claude"], "");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("newer OAuth token kept"));
    assert_eq!(fs::read_to_string(remote_creds).unwrap(), new.to_string());
    assert!(fs::read_to_string(remote_env)
        .unwrap()
        .contains("unset CLAUDE_CODE_OAUTH_TOKEN"));
}

#[test]
fn oauth_login_is_isolated_and_explicit_switch_disables_setup_token() {
    let f = Fixture::new();
    f.install();
    let initial = fs::read(f.env_path()).unwrap();
    f.stub("claude", r#"test "${ANTHROPIC_API_KEY+x}" != x
test "${CLAUDE_CODE_OAUTH_TOKEN+x}" != x
test "$1 $2" = 'auth login'
case "$CLAUDE_CONFIG_DIR" in */.login-*) ;; *) exit 2;; esac
printf '%s\n' '{"claudeAiOauth":{"accessToken":"fake-oauth","expiresAt":1}}' > "$CLAUDE_CONFIG_DIR/.credentials.json"
printf '%s\n' '{"oauthAccount":{"emailAddress":"test@example.invalid"}}' > "$CLAUDE_CONFIG_DIR/.claude.json"
"#);
    let out = f
        .command()
        .args(["login", "claude"])
        .env("CLAUDE_CODE_OAUTH_TOKEN", FAKE)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(fs::read(f.env_path()).unwrap(), initial);
    assert!(f.0.join("state/claude/test@example.invalid.json").exists());
    assert!(f
        .run(&["use", "claude", "test@example.invalid", "--local"], "")
        .status
        .success());
    let env = fs::read_to_string(f.env_path()).unwrap();
    assert!(env.contains("unset CLAUDE_CODE_OAUTH_TOKEN"));
    assert!(!env.contains(FAKE));
    let v = f.json();
    let r = token_row(&v);
    assert_eq!(r["setup_token"]["present"], false);
    assert_eq!(r["email"], "test@example.invalid");
    assert_eq!(r["active"], true);
}

#[test]
fn invalid_input_and_repo_destination_fail_without_echoing_input() {
    let f = Fixture::new();
    for input in ["", "sk-ant-api03-fake", "sk-ant-oat01-fake'; touch nope"] {
        assert!(!f
            .run(&["login", "claude", "--token"], input)
            .status
            .success());
    }
    assert!(!f.env_path().exists());
    fs::write(f.0.join(".git"), "gitdir: fake").unwrap();
    assert!(!f
        .run(&["login", "claude", "--token"], FAKE)
        .status
        .success());
    assert!(!f.env_path().exists());
    assert!(!f.0.join("nope").exists());
}

#[test]
fn installer_refuses_symlinks_without_touching_the_target() {
    let f = Fixture::new();
    fs::create_dir_all(f.env_path().parent().unwrap()).unwrap();
    let target = f.0.join("keep");
    fs::write(&target, "preserve this").unwrap();
    std::os::unix::fs::symlink(&target, f.env_path()).unwrap();
    assert!(!f
        .run(&["login", "claude", "--token"], FAKE)
        .status
        .success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "preserve this");
    assert!(fs::symlink_metadata(f.env_path())
        .unwrap()
        .file_type()
        .is_symlink());
}
