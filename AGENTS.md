# subs: agent guide

`subs` sees and switches ChatGPT/Codex, Claude and Cursor subscription logins,
and fans the active one out to the user's other machines over ssh. This file is
for agents. Read "Use it" when the job is operating the installed `subs`, and
"Change it" when editing this repository. `README.md` is the user manual and
has the full behaviour; `subs help` is the command reference. `CLAUDE.md` is a
symlink to this file.

## Use it

### Look first, read-only

```sh
subs --version      # 0.3.0+ has `subs check` and --code-file
subs json           # status of every login, machine-readable: prefer it to `subs`
subs check claude   # prove the Claude token works here and on every host
cat ~/.config/subs/hosts   # the hosts a push reaches ($SUBS_HOSTS overrides)
```

`subs json` rows have `provider`, `email`, `active`, `usage_status`
(`available`, `observed`, `unsupported`, `inactive`, `error`), `windows`,
`resets` and `spend`. `password` is only a yes/no flag. A Claude setup-token
row has `setup_token`. The token cannot report usage itself: its `windows` are
what the last `subs check claude` saw (`usage_status: observed`, with
`observed_at`), or `unsupported` before any check. That is normal, as is
`valid: null`. An `inactive` saved OAuth login beside a working token is not a
problem.

Each model's limit is its own window (`Fable 7d`). A thread that says a model
is blocked may be quoting an old account's limit. Before switching models,
check the current one: `subs check claude --local` prints the live windows.

`subs check claude` changes nothing. It makes one small Haiku request per
machine and checks each running T3 server's environment. This is the proof
that Claude works; a file being present, an active service or
`claude auth status` proves too little.

### Do a task

| The user wants | Run |
|---|---|
| Sign in to a Claude account and put it on every machine and T3 | `subs login claude EMAIL --token --use --restart-t3` |
| Confirm it worked, now or later | `subs check claude` |
| A host was offline and is back | `subs push claude` (add `--restart-t3` if its T3 must take it) |
| Switch to a login that is already saved | `subs use codex\|claude\|cursor EMAIL` |
| Sign in to Codex or Cursor | `subs login codex\|cursor EMAIL --use` |
| Spend a banked Codex full reset | `subs reset codex` (refused unless a window is used up) |
| Bring the CLIs up to date everywhere | `subs update` (`--restart-t3` so T3 sees the new CLI) |
| Only this machine | add `--local` (not with `--restart-t3`) |

For Claude across several machines, always use `--token`. It makes a
long-lived setup-token in the browser, so every machine shares one credential
that never rotates. A copied OAuth login breaks as soon as one machine refreshes
it. Without `--use` the token is only installed here; `--use` pushes it.

A push also brings each host's CLI up to at least this machine's version.
Hosts in `~/.config/subs/hosts` may list the providers they take
(`laptop claude`). The file belongs to the user: ask before editing it. To add
a host the user names, first check that `ssh -o BatchMode=yes HOST true` works,
then add one line and run `subs push PROVIDER`.

### Sign in without a terminal

The browser sign-in needs the user; everything after it is automatic.

1. Make a private directory (`DIR=$(mktemp -d)`, mode 0700). Then start the
   login as a background command, with the code file at a path that does not
   exist yet:
   `subs login claude EMAIL --token --code-file="$DIR/claude-code" --use --restart-t3 > "$DIR/login.log" 2>&1 </dev/null`
2. Within a few seconds the log shows the `oauth/authorize` link. `subs` then
   waits up to 30 minutes for the code. Show the user the full link and the
   account to sign in as. `EMAIL` is only a label: Claude cannot report
   a token's account, so the user must sign in as that account.
3. Get the code through a private secret prompt that writes it to a file (in T3:
   `request_secret` with `delivery: "file"` and the link as its `url`). Never
   ask for the code in chat.
4. Put it into place in one step, without reading it:
   `cat "$SECRET_FILE" > "$DIR/claude-code.part" && mv "$DIR/claude-code.part" "$DIR/claude-code"`.
   `subs` reads it once and removes it.
5. Read the log when the command exits. Nothing in it is secret.

`--browser` is the alternative: it reads the code as one line on stdin. Plain
stdin without either flag imports an existing token, unchecked.

### T3 restarts end your turn

`--restart-t3` restarts every active T3 server, which interrupts every agent
running in them. That includes you if you run inside T3 (`T3CODE_HOME` is set).
Use it only when the user wants T3 to pick up the change. On remote hosts T3
restarts at once, and `subs` checks it after a token push.

Inside T3, `subs` checks this machine, schedules its own T3 restart for five
seconds after it exits, and the restart ends your turn. The login and fanout
always finish first, and the log is complete. Tell the user before
starting, and keep the log in a file. When they come back, read the log, then
run `subs check claude`; its exit status 0 means done. Do not repeat the login
or push just because your turn was cut short. Read the log first.

### After a switch, tell the threads

`subs` changes the machines, not what agents remember. A T3 thread that saw the
old account's "You've hit your Fable limit · resets …" keeps saying that model
is blocked and offers other models, even though its new process runs on the new
account. A switch is done only when the threads know:

- Look through unsettled threads on every T3 environment for pending questions
  or failures that cite a limit, `blocked until`, `resets` or a sign-in. Answer
  them with the account now in use and when its T3 restarted. Restate what the
  user originally asked for; do not choose for them.
- Resume, once, threads that the restart cut off mid-turn. Leave settled,
  finished and deliberately stopped ones alone.
- Prove a model with one tiny request to it, not with the 5h or 7d windows.

### Read the results

- Host lines are `✓` done, `○` offline, or `✗` failed. Offline hosts are skipped
  and catch up on the next push; offline is not a failure. A failed ssh
  authentication or host-key check is a failure.
- The exit status is nonzero when a reachable host failed. Exit 0 with some
  hosts offline is success; say which ones were offline. A host without a
  running T3 shows just `Claude replies`.
- A line looks like
  `✓ HOST  updated · claude 2.1.330 · T3 restarted · Claude replies · T3 has the token`:
  each step in order, then the check.
- Lines from `subs check claude`:
  - `Claude replies · T3 has the token`: working.
  - `T3 still runs an older token`: `subs push claude --restart-t3`.
  - `T3 runs without the token`: that T3's startup does not load
    `subs/claude-shell.sh` (see README, Hosts). A restart alone cannot fix it.
  - `has a different Claude token`: `subs push claude`.
  - `Claude rejected the token`: the token is revoked or expired; sign in again.
  - `usage limit is reached`: the token works, the account is out of quota.
    Ask the user before switching accounts.

### Rules

- Never print, log, or put on a command line a token, a credential file, a
  `claude.env`, a sign-in code or an environment dump. `subs` already masks
  them; do not work around it.
- `subs pass` prints the password itself when no clipboard is available. Only
  run it when the user asks for the password.
- Do not run `claude` with a shared OAuth login on several machines to "test"
  it. A request can refresh the token, which rotates the shared login and breaks
  the other copies. `subs check` only checks the setup-token for this reason.
- Do not log a machine out to troubleshoot, and do not `subs rm` an account
  unless the user asked.
- Switching accounts is the user's decision. Do not pick another account
  because one hit a limit.

### Where state lives

- `~/.local/share/subs` (`$SUBS_HOME`): saved logins and stored passwords, 0700.
- `~/.config/subs/` (`$XDG_CONFIG_HOME/subs`): `hosts`, plus the managed Claude token files
  (`claude.env`, `claude-account.json`, `claude-shell.sh`), 0600. Do not edit
  the generated files.

## Change it

### Build and check

```sh
cargo test --locked                    # unit tests plus tests/claude_token.rs
cargo test --locked --test claude_token NAME   # one CLI test
cargo clippy --locked --all-targets    # keep it warning-free
cargo fmt
make install                           # release build to ~/.local/bin/subs
```

Use `make` for release binaries: it remaps build paths out of the binary.
After installing, a live read-only check is `subs json` and `subs check claude`.
Do not try `--restart-t3`, `use`, `login` or `push` live unless the user asked:
they change real logins on real machines.

### Map

| File | What it does |
|---|---|
| `src/main.rs` | Argument parsing, `HELP`, command dispatch |
| `src/actions.rs` | `use`, `login`, `add`, `pass`, `rm`: the outcome of each command and TUI key |
| `src/prov.rs` | The `Prov` provider enum and its shared read/write/email/expiry |
| `src/prov/claude.rs` | Claude OAuth plus the setup-token: `Token`, the shell hook and env installer, `claude setup-token` driven on a pty, the token check |
| `src/prov/codex.rs`, `src/prov/cursor.rs` | Codex usage and resets; the whole Cursor provider, including its push |
| `src/push.rs` | ssh fanout, the per-provider remote installers, T3 restarts, `exec`/`exec_here`, the host-line output |
| `src/check.rs` | `subs check claude` and the proof after a token push |
| `src/update.rs` | CLI version catch-up after a push, and `subs update` |
| `src/t3_macos.py` | Embedded LaunchAgent reloader for T3 on macOS |
| `src/status.rs`, `src/view.rs`, `src/tui.rs` | Status rows, their rendering, the interactive TUI |
| `src/store.rs` | `$SUBS_HOME` layout, atomic 0600 writes |
| `src/pty.rs` | Minimal pty and screen model for driving `claude setup-token` |
| `src/demo.rs` | `SUBS_DEMO=1` fake data for the README gifs (`make gifs`) |
| `tests/claude_token.rs` | CLI tests against an isolated home with stubbed tools |

### Invariants

- **Secrets travel on stdin only.** A remote step is a POSIX `sh -s` script on
  ssh's stdin, with any credential in a heredoc (`push::heredoc`), never in argv
  or the environment. `push::exec(..., secret: true)` never shows remote stderr.
- **Remote output is allowlisted.** Read only fixed status lines (`subs:`,
  `subs-t3:`, `subs-cli:`, `subs-check:`, or a new prefix like them) and match
  them exactly, mapping each to your own text. A
  remote shell can echo its input, so never show what it printed.
- **The setup-token stays in `prov::claude::Token`.** Its `Debug` is redacted,
  and it never goes into JSON, snapshots or messages. Add a narrow method there
  (like `Token::heredoc`) rather than exposing the value.
- **No token refreshes.** `subs` never refreshes an OAuth token; that stays the
  real CLI's job against the live file.
- **Scripts run under `sh -s`**, so stdin is the rest of the script: nothing in
  them may read stdin (use `</dev/null`, or a pipe). They must work in dash
  and in macOS sh, and pass `shellcheck -s sh`.
- **Few dependencies.** The only crates are `ratatui`, `serde_json` and `libc`.
  Adding one needs a real reason.
- **Docs follow behaviour.** A user-visible change updates `README.md`, `HELP`
  in `src/main.rs`, and the version in `Cargo.toml`.

### Tests

`tests/claude_token.rs` runs the real binary in a throwaway `HOME`. Every
external tool is a stub in its `bin/`: ssh, claude, codex, cursor-agent, curl,
security, systemctl, systemd-run, launchctl, ps and uname. A test must never
reach this machine's real T3, service manager, keychain or network.
`Fixture::run` fails the test if the fake token or API key shows up in any
output.

Building blocks:

- `f.install()`: a managed token.
- `f.hosts()`: one host, `fake-vm`.
- `mock_t3` / `mock_macos_t3`: an ssh stub that runs the script with the
  fixture's `remote/` directory as `HOME`, plus service-manager stubs.
- `f.t3_server(token)`: a fake running T3 with that token in its environment.
- `fake_claude`: `claude setup-token` on a pty.
- `f.run_with(.., &[("T3CODE_HOME", ..)])`: the run-from-inside-T3 path.

Unit tests for parsers live next to them (`check::read`, `push::t3_result`,
`update::read`).

### Commits

Subjects are short, lowercase, and say what changed and why, for example
`claude: switch every machine and T3 in one command, and prove it took`.
The repository is public: no hostnames, emails, account names or credentials
in code, tests, docs or commit messages. Use `example.com`/`example.invalid`
and `fake-vm`.
