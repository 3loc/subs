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

#[test]
fn switching_accounts_preserves_saved_credentials_when_live_login_was_cleared() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("state/claude")).unwrap();
    fs::create_dir_all(f.0.join(".claude")).unwrap();
    let blob = |email: &str, token: &str| {
        serde_json::json!({
            "account": {"emailAddress": email},
            "credentials": {"claudeAiOauth": {"accessToken": token}}
        })
    };
    let old = blob("old@example.com", "synthetic-old");
    let new = blob("new@example.com", "synthetic-new");
    let saved = f.0.join("state/claude/old@example.com.json");
    fs::write(&saved, old.to_string()).unwrap();
    fs::write(
        f.0.join("state/claude/new@example.com.json"),
        new.to_string(),
    )
    .unwrap();
    fs::write(
        f.0.join(".claude/.claude.json"),
        serde_json::json!({"oauthAccount": old["account"]}).to_string(),
    )
    .unwrap();
    fs::write(
        f.0.join(".claude/.credentials.json"),
        blob("old@example.com", "")["credentials"].to_string(),
    )
    .unwrap();
    let out = f.run(&["use", "claude", "new@example.com", "--local"], "");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&saved).unwrap()).unwrap(),
        old
    );

    fs::write(&saved, blob("old@example.com", "").to_string()).unwrap();
    let current = fs::read(f.0.join(".claude/.credentials.json")).unwrap();
    let out = f.run(&["use", "claude", "old@example.com", "--local"], "");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("subs login claude old@example.com"));
    assert_eq!(
        fs::read(f.0.join(".claude/.credentials.json")).unwrap(),
        current
    );
}

#[test]
fn selecting_the_same_account_restores_its_cleared_live_credentials() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("state/claude")).unwrap();
    fs::create_dir_all(f.0.join(".claude")).unwrap();
    let saved = serde_json::json!({
        "account": {"emailAddress": "test@example.invalid"},
        "credentials": {"claudeAiOauth": {"accessToken": "synthetic-saved"}}
    });
    let snapshot = f.0.join("state/claude/test@example.invalid.json");
    fs::write(&snapshot, saved.to_string()).unwrap();
    fs::write(
        f.0.join(".claude/.claude.json"),
        serde_json::json!({"oauthAccount": saved["account"]}).to_string(),
    )
    .unwrap();
    let live = f.0.join(".claude/.credentials.json");
    fs::write(
        &live,
        serde_json::json!({"claudeAiOauth": {"accessToken": "", "refreshToken": "", "expiresAt": 0}}).to_string(),
    )
    .unwrap();
    let out = f.run(&["use", "claude", "test@example.invalid", "--local"], "");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("signed in again"));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(live).unwrap()).unwrap(),
        saved["credentials"]
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(snapshot).unwrap()).unwrap(),
        saved
    );
    let out = f.run(&["use", "claude", "test@example.invalid", "--local"], "");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("is already active"));
}

#[test]
fn cleared_live_credentials_cannot_be_pushed_or_restart_t3() {
    for with_hosts in [false, true] {
        let f = Fixture::new();
        fs::create_dir_all(f.0.join(".claude")).unwrap();
        fs::create_dir_all(f.0.join(".config/subs")).unwrap();
        if with_hosts {
            f.hosts();
        }
        fs::write(
            f.0.join(".claude/.credentials.json"),
            serde_json::json!({"claudeAiOauth": {"accessToken": "", "refreshToken": "", "expiresAt": 0}}).to_string(),
        )
        .unwrap();
        fs::write(
            f.0.join(".claude/.claude.json"),
            serde_json::json!({"oauthAccount": {"emailAddress": "test@example.invalid"}})
                .to_string(),
        )
        .unwrap();
        for tool in ["ssh", "systemctl", "systemd-run"] {
            f.stub(tool, "touch \"$HOME/side-effect\"; exit 0\n");
        }
        for args in [
            vec!["push", "claude"],
            vec!["push", "claude", "--restart-t3"],
        ] {
            let out = f.run(&args, "");
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr)
                .contains("refusing to push cleared claude credentials"));
            assert!(!f.0.join("side-effect").exists());
        }
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
    assert!(text.contains("configured"));
    assert!(text.contains("estimated expiry in"));
    assert_eq!(fs::read_dir(f.0.join("state")).unwrap().count(), 0);
}

#[test]
fn repeated_status_does_not_report_inactive_oauth_or_unsupported_token_usage_as_errors() {
    let f = Fixture::new();
    f.install();
    let token_before = fs::read(f.env_path()).unwrap();
    fs::create_dir_all(f.0.join(".claude")).unwrap();
    fs::create_dir_all(f.0.join("state/claude")).unwrap();
    let account = serde_json::json!({"emailAddress": "old@example.invalid"});
    let credentials = serde_json::json!({"claudeAiOauth": {"accessToken": "", "refreshToken": "", "expiresAt": 0}});
    fs::write(
        f.0.join(".claude/.credentials.json"),
        credentials.to_string(),
    )
    .unwrap();
    fs::write(
        f.0.join(".claude/.claude.json"),
        serde_json::json!({"oauthAccount": account}).to_string(),
    )
    .unwrap();
    let snapshot = f.0.join("state/claude/old@example.invalid.json");
    fs::write(
        &snapshot,
        serde_json::json!({"credentials": credentials, "account": account}).to_string(),
    )
    .unwrap();
    let snapshot_before = fs::read(&snapshot).unwrap();
    for _ in 0..2 {
        let out = f.run(&["status"], "");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success());
        assert!(text.contains("saved login · inactive"));
        assert!(text.contains("configured"));
        assert!(
            !text.contains("sign in required") && !text.contains("no verification request sent")
        );
        let v = f.json();
        let rows: Vec<_> = v
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["provider"] == "claude")
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().filter(|r| r["active"] == true).count(), 1);
        assert!(rows.iter().all(|r| r.get("err").is_none()));
        assert_eq!(rows[0]["usage_status"], "inactive");
        assert_eq!(rows[1]["usage_status"], "unsupported");
        assert!(rows[1]["setup_token"]["valid"].is_null());
        assert_eq!(fs::read(f.env_path()).unwrap(), token_before);
        assert_eq!(fs::read(&snapshot).unwrap(), snapshot_before);
    }
}

#[test]
fn token_email_label_merges_account_card_preserves_age_and_survives_push() {
    let f = Fixture::new();
    f.install();
    let original_env = fs::read(f.env_path()).unwrap();
    let before = token_row(&f.json())["setup_token"]["installed_at"].clone();
    let out = f.run(&["label", "claude", "account@example.invalid"], "");
    assert!(out.status.success());
    assert_eq!(fs::read(f.env_path()).unwrap(), original_env);
    fs::create_dir_all(f.0.join("state/claude")).unwrap();
    let snapshot = f.0.join("state/claude/account@example.invalid.json");
    fs::write(&snapshot, serde_json::json!({"account":{"emailAddress":"account@example.invalid"},"credentials":{"claudeAiOauth":{"accessToken":"","expiresAt":0}}}).to_string()).unwrap();
    let v = f.json();
    let claude: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["provider"] == "claude")
        .collect();
    assert_eq!(claude.len(), 1);
    assert_eq!(claude[0]["email"], "account@example.invalid");
    assert_eq!(claude[0]["active"], true);
    assert_eq!(claude[0]["setup_token"]["installed_at"], before);
    assert_eq!(claude[0]["setup_token"]["identity_source"], "user_label");
    assert!(claude[0]["setup_token"]["identity_verified"].is_null());
    assert!(snapshot.exists(), "OAuth snapshot must remain saved");
    let out = f.run(&["status"], "");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.matches("account@example.invalid").count(), 1);
    assert!(text.contains("[token]"));
    assert!(!text.contains("setup-token"));
    let env = fs::read(f.env_path()).unwrap();
    assert!(f
        .run(&["use", "claude", "account@example.invalid", "--local"], "")
        .status
        .success());
    assert_eq!(fs::read(f.env_path()).unwrap(), env);
    assert!(!f
        .run(&["label", "claude", "bad@example.invalid\necho unsafe"], "")
        .status
        .success());
    assert_eq!(fs::read(f.env_path()).unwrap(), env);
    f.hosts();
    f.stub("ssh", "HOME=\"$TEST_REMOTE_HOME\" XDG_CONFIG_HOME=\"$TEST_REMOTE_HOME/.config\" exec /bin/sh -s\n");
    assert!(f.run(&["push", "claude"], "").status.success());
    assert_eq!(
        fs::read(f.0.join("remote/.config/subs/claude.env")).unwrap(),
        env
    );
    let remote_label: Value = serde_json::from_slice(
        &fs::read(f.0.join("remote/.config/subs/claude-account.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(remote_label["email"], "account@example.invalid");
}

#[test]
fn token_login_accepts_explicit_email_without_exposing_secret() {
    let f = Fixture::new();
    let out = f.run(
        &[
            "login",
            "claude",
            "account@example.invalid",
            "--token",
            "--local",
        ],
        &format!("{FAKE}\n"),
    );
    assert!(out.status.success());
    assert_eq!(token_row(&f.json())["email"], "account@example.invalid");
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

fn mock_t3(f: &Fixture) {
    f.install();
    f.hosts();
    f.stub("ssh", "HOME=\"$TEST_REMOTE_HOME\" XDG_CONFIG_HOME=\"$TEST_REMOTE_HOME/.config\" exec /bin/sh -s\n");
    f.stub(
        "systemctl",
        r#"
printf '%s:%s\n' "$HOME" "$*" >> "$TEST_REMOTE_HOME/../t3-calls"
case "$*" in
  '--user is-active --quiet t3code.service') exit "${TEST_T3_INACTIVE:-0}" ;;
  '--user restart t3code.service')
    test -f "$HOME/.config/subs/claude.env"
    exit "${TEST_T3_RESTART_FAIL:-0}" ;;
  *) exit 2 ;;
esac
"#,
    );
    f.stub(
        "systemd-run",
        r#"
test -f "$TEST_REMOTE_HOME/.config/subs/claude.env"
printf 'scheduled:%s\n' "$*" >> "$TEST_REMOTE_HOME/../t3-calls"
"#,
    );
}

#[test]
fn t3_restart_is_opt_in_and_source_is_scheduled_after_remote_restart() {
    let f = Fixture::new();
    mock_t3(&f);
    assert!(f.run(&["push", "claude"], "").status.success());
    let log = f.0.join("t3-calls");
    assert!(!log.exists(), "normal pushes must not touch T3");
    let out = f.run(&["push", "claude", "--restart-t3"], "");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("T3 restarted"));
    assert!(text.contains("T3 restart scheduled"));
    let calls = fs::read_to_string(log).unwrap();
    assert!(
        calls.find("--user restart t3code.service").unwrap() < calls.find("scheduled:").unwrap()
    );
    assert!(calls.contains("--on-active=5s"));
    assert!(calls.contains("--user try-restart t3code.service"));
}

#[test]
fn t3_restart_failure_is_reported_after_credentials_are_installed() {
    let f = Fixture::new();
    mock_t3(&f);
    f.stub(
        "systemctl",
        r#"
case "$*" in
  '--user is-active --quiet t3code.service') exit 0 ;;
  *) exit 1 ;;
esac
"#,
    );
    let out = f.run(&["push", "claude", "--restart-t3"], "");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("credentials installed; T3 restart failed")
    );
    assert!(f.0.join("remote/.config/subs/claude.env").exists());
}

#[test]
fn selecting_the_active_oauth_account_can_restart_t3_and_schedule_failures_are_errors() {
    let f = Fixture::new();
    mock_t3(&f);
    fs::create_dir_all(f.0.join("state/claude")).unwrap();
    fs::write(f.0.join("state/claude/test@example.invalid.json"), serde_json::json!({
        "account": {"emailAddress": "test@example.invalid"},
        "credentials": {"claudeAiOauth": {"accessToken": "synthetic", "refreshToken": "synthetic-refresh"}}
    }).to_string()).unwrap();
    assert!(f
        .run(&["use", "claude", "test@example.invalid", "--local"], "")
        .status
        .success());
    let out = f.run(
        &["use", "claude", "test@example.invalid", "--restart-t3"],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("T3 restarted"));
    f.stub("systemd-run", "exit 1\n");
    let out = f.run(&["push", "claude", "--restart-t3"], "");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("local T3 restart could not be scheduled")
    );
}

#[test]
fn t3_restart_skips_inactive_services_and_failed_installations() {
    let f = Fixture::new();
    mock_t3(&f);
    f.stub("systemctl", "exit 3\n");
    let out = f.run(&["push", "claude", "--restart-t3"], "");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("T3 skipped"));
    assert!(!f.0.join("t3-calls").exists());

    mock_t3(&f);
    f.stub("ssh", "cat >/dev/null; exit 3\n");
    let out = f.run(&["push", "claude", "--restart-t3"], "");
    assert!(!out.status.success());
    let calls = fs::read_to_string(f.0.join("t3-calls")).unwrap_or_default();
    assert!(!calls.contains("--user restart t3code.service"));
}

#[test]
fn t3_restart_flag_rejects_other_providers_and_local_only_operations() {
    let f = Fixture::new();
    for args in [
        vec!["push", "codex", "--restart-t3"],
        vec!["use", "claude", "a@example.com", "--local", "--restart-t3"],
        vec!["login", "claude", "--restart-t3"],
    ] {
        let out = f.run(&args, "");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--restart-t3"));
    }
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

const SECOND: &str = "sk-ant-oat01-FAKE-second-test-only";
const CODE: &str = "FAKE-AUTH-CODE#FAKE-STATE";
const URL: &str = "https://claude.com/cai/oauth/authorize?code=true&state=FAKE";

/// A stand-in for `claude setup-token`: a terminal UI drawn with cursor moves,
/// the sign-in link in an OSC 8 hyperlink, then `token` drawn in two parts.
/// `check` is the body of `claude -p`, the check of the new token.
fn fake_claude(f: &Fixture, token: &str, check: &str) {
    let (a, b) = token.split_at(20);
    f.stub(
        "claude",
        &format!(
            r#"test "${{ANTHROPIC_API_KEY+x}}" != x
case "$*" in *sk-ant-*) exit 9;; esac
case "$1" in
setup-token)
  test "${{CLAUDE_CODE_OAUTH_TOKEN+x}}" != x
  case "$CLAUDE_CONFIG_DIR" in */state/claude/.login-token-*) ;; *) exit 2;; esac
  test -t 0 && test -t 1
  printf '\033[2GWelcome\033[10Gto\033[13GClaude\r\n'
  printf '\033]8;id=1;{URL}\007\033[38;5;246m{URL}\033[39m\033]8;;\007\r\n'
  printf '\033[2GPaste\033[8Gcode\033[13Ghere\033[18Gif\033[21Gprompted\033[30G>\r\n'
  IFS= read -r code || exit 0
  printf '%s' "$code" > "$HOME/code-seen"
  test "$code" = '{CODE}' || {{ printf 'OAuth error: invalid code\r\n'; exit 1; }}
  printf 'Your OAuth token (valid for 1 year):\033[38G\033[32m{a}'
  sleep 0.3
  printf '{b}\033[39m\r\n\r\nStore this token securely.\r\n'
  exec sleep 30
  ;;
-p)
  case "$CLAUDE_CONFIG_DIR" in */state/claude/.login-check-*) ;; *) exit 2;; esac
  case "$(cat)" in *ok*) ;; *) exit 5;; esac
  printf '%s\n' "$CLAUDE_CODE_OAUTH_TOKEN" > "$HOME/checked-with"
  {check}
  ;;
*) exit 1;;
esac
"#
        ),
    );
}

fn no_leftovers(f: &Fixture) {
    let left: Vec<_> = fs::read_dir(f.0.join("state/claude"))
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn browser_token_login_checks_before_installing_and_remembers_the_account() {
    let f = Fixture::new();
    fake_claude(&f, FAKE, "printf 'ok\\n'");
    let started = std::time::Instant::now();
    let out = f.run(
        &[
            "login",
            "claude",
            "account@example.invalid",
            "--token",
            "--browser",
            "--local",
        ],
        &format!("{CODE}\n"),
    );
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{stderr}");
    assert!(
        started.elapsed().as_secs() < 20,
        "setup-token was not ended"
    );
    assert!(stderr.contains(&format!("\n{URL}\n")), "{stderr}");
    assert!(stderr.contains("sign in as account@example.invalid"));
    assert!(stdout.contains("account@example.invalid [token] installed and checked"));
    assert!(stdout.contains("is your label"));
    for text in [&stdout, &stderr] {
        assert!(!text.contains("FAKE-AUTH-CODE") && !text.contains("sk-ant-"));
    }
    assert_eq!(fs::read_to_string(f.0.join("code-seen")).unwrap(), CODE);
    assert_eq!(
        fs::read_to_string(f.0.join("checked-with")).unwrap(),
        format!("{FAKE}\n")
    );
    assert!(fs::read_to_string(f.env_path()).unwrap().contains(FAKE));
    let v = f.json();
    let r = token_row(&v);
    assert_eq!(r["email"], "account@example.invalid");
    assert_eq!(r["active"], true);
    assert_eq!(r["setup_token"]["identity_source"], "user_label");
    assert!(r["setup_token"]["identity_verified"].is_null());
    let text = String::from_utf8_lossy(&f.run(&["status"], "").stdout).to_string();
    assert!(text.contains("[token]") && text.contains("user-labelled"));
    no_leftovers(&f);
}

#[test]
fn replacement_that_fails_its_check_or_sign_in_keeps_the_current_token() {
    let f = Fixture::new();
    let out = f.run(
        &["login", "claude", "current@example.invalid", "--token"],
        &format!("{FAKE}\n"),
    );
    assert!(out.status.success());
    let env = fs::read(f.env_path()).unwrap();
    let label = fs::read(f.0.join(".config/subs/claude-account.json")).unwrap();
    let args = [
        "login",
        "claude",
        "new@example.invalid",
        "--token",
        "--browser",
        "--local",
    ];
    // Rejected by Claude, and its message names the token: neither is shown.
    fake_claude(
        &f,
        SECOND,
        &format!("printf 'Invalid bearer token {SECOND}\\n' >&2; exit 1"),
    );
    let out = f.run(&args, &format!("{CODE}\n"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("failed its check (Invalid bearer token [redacted])"),
        "{stderr}"
    );
    assert!(stderr.contains("current login is unchanged"));
    assert!(!stderr.contains(SECOND));
    // A wrong code, input that is not a code, and no code at all: never checked.
    fs::remove_file(f.0.join("checked-with")).unwrap();
    fake_claude(&f, SECOND, "printf 'ok\\n'");
    for (input, says) in [
        ("WRONG-CODE#WRONG-STATE\n", "OAuth error: invalid code"),
        ("not a code!\n", "not the code from the sign-in page"),
        ("", "stdin closed before the code arrived"),
    ] {
        let out = f.run(&args, input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(stderr.contains(says), "{stderr}");
        assert!(!stderr.contains("WRONG-CODE") && !stderr.contains(SECOND));
    }
    assert!(!f.0.join("checked-with").exists());
    assert_eq!(fs::read(f.env_path()).unwrap(), env);
    assert_eq!(
        fs::read(f.0.join(".config/subs/claude-account.json")).unwrap(),
        label
    );
    assert_eq!(token_row(&f.json())["email"], "current@example.invalid");
    no_leftovers(&f);
}

#[test]
fn an_existing_token_pasted_for_the_code_is_checked_and_installed() {
    let f = Fixture::new();
    fake_claude(&f, SECOND, "printf 'ok\\n'");
    let started = std::time::Instant::now();
    let out = f.run(
        &[
            "login",
            "claude",
            "account@example.invalid",
            "--token",
            "--browser",
            "--local",
        ],
        &format!("{FAKE}\n"),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(started.elapsed().as_secs() < 20);
    assert!(
        !f.0.join("code-seen").exists(),
        "a token is never sent as a code"
    );
    assert_eq!(
        fs::read_to_string(f.0.join("checked-with")).unwrap(),
        format!("{FAKE}\n")
    );
    assert_eq!(token_row(&f.json())["email"], "account@example.invalid");
}

#[test]
fn selecting_a_token_labelled_account_keeps_the_token_until_oauth_is_explicit() {
    let f = Fixture::new();
    let email = "account@example.invalid";
    assert!(f
        .run(&["login", "claude", email, "--token"], &format!("{FAKE}\n"))
        .status
        .success());
    let env = fs::read(f.env_path()).unwrap();
    // An OAuth sign-in to the same account is saved, and says how to switch.
    f.stub(
        "claude",
        &format!(
            r#"test "$1 $2" = 'auth login'
printf '%s\n' '{{"claudeAiOauth":{{"accessToken":"fake-oauth","expiresAt":1}}}}' > "$CLAUDE_CONFIG_DIR/.credentials.json"
printf '%s\n' '{{"oauthAccount":{{"emailAddress":"{email}"}}}}' > "$CLAUDE_CONFIG_DIR/.claude.json"
"#
        ),
    );
    let out = f.run(&["login", "claude", email, "--local"], "");
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("subs use claude {email} --oauth"))
    );
    assert_eq!(fs::read(f.env_path()).unwrap(), env);
    let out = f.run(&["use", "claude", email, "--local"], "");
    assert!(String::from_utf8_lossy(&out.stdout).contains("is already active"));
    assert_eq!(fs::read(f.env_path()).unwrap(), env);
    let out = f.run(&["use", "claude", email, "--oauth", "--local"], "");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.contains(&format!("{email} [token] → {email} [OAuth]")),
        "{stdout}"
    );
    assert!(stdout.contains("subscription token removed from this machine"));
    let env = fs::read_to_string(f.env_path()).unwrap();
    assert!(env.contains("unset CLAUDE_CODE_OAUTH_TOKEN") && !env.contains(FAKE));
    assert!(!f.0.join(".config/subs/claude-account.json").exists());
    let v = f.json();
    assert_eq!(token_row(&v)["email"], email);
    assert_eq!(token_row(&v)["active"], true);
    assert_eq!(token_row(&v)["setup_token"]["present"], false);
}

#[test]
fn token_flags_are_only_accepted_where_they_mean_something() {
    let f = Fixture::new();
    for args in [
        vec!["login", "claude", "--browser"],
        vec!["login", "codex", "--token"],
        vec!["push", "claude", "--token"],
        vec!["use", "codex", "a@example.invalid", "--oauth"],
        vec!["login", "claude", "--oauth"],
    ] {
        let out = f.run(&args, "");
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("usage")
                || String::from_utf8_lossy(&out.stderr).contains("--oauth")
        );
    }
    assert!(!f.env_path().exists());
}
