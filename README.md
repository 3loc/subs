# subs

See and switch ChatGPT/Codex, Claude, and Cursor subscription logins: who is active,
usage, time to reset, banked full resets. Switching a login also installs it on
all your machines.

![subs tui](docs/tui.gif)

## Install

Needs Rust (`cargo`), `curl`, and the `codex`, `claude`, and/or `cursor-agent` CLI. Linux or macOS.

```sh
git clone git@github.com:3loc/subs.git && cd subs && make install
```

Installs to `~/.local/bin/subs` (`make install PREFIX=/usr/local` for system-wide,
`make uninstall` to remove).
Use `make` for release binaries: it remaps the build user's home and checkout
paths to generic locations, including dependency error locations. No local
account data or host inventory is part of the build.

## Use

```sh
subs                                    # status of every account
subs tui                                # interactive view
subs login codex|claude|cursor [EMAIL]  # sign in; --use makes it active and pushes it
                                        # (signing in as the active account always does)
subs login claude EMAIL --token         # long-lived Claude token via the browser, checked
subs login claude EMAIL --token --use --restart-t3
                                        # all of it: sign in, push everywhere, restart T3,
                                        # and prove Claude answers on every machine
subs check claude                       # prove the token works here and on every host
subs label claude EMAIL                 # name the installed token's account
subs use codex|claude|cursor EMAIL      # make a saved login active and push it to your hosts
subs push codex|claude|cursor           # push the active login to your hosts again
subs update [codex|claude|cursor]       # latest CLI here, then on every host
subs add codex|claude|cursor EMAIL      # store the account's password
subs pass codex|claude|cursor EMAIL     # copy (or print) that password
subs reset codex                        # spend a banked full reset on the active login
subs json                               # status as JSON
subs rm codex|claude|cursor EMAIL       # forget an account
```

![subs](docs/status.gif)

TUI keys: `j`/`k` move, `enter` use (and push), `f` push again, `l` login, `n` new,
`t` long-lived Claude token for the account, `a` password, `p` copy password,
`d` delete, `r` refresh, `q` quit. On a `[token]` card, `l` creates a new token
for it; the new token goes out to your hosts once checked and installed.

## Switch Claude everywhere

One command signs in to a Claude account and puts it on every machine and every
T3 server:

```sh
subs login claude you@example.com --token --use --restart-t3
```

It creates a long-lived subscription token in the browser, checks it with one
small request, installs it here, pushes it to every host (bringing each host's
`claude` up to this machine's version), restarts the active T3 servers, and then
proves it took: on each machine, a fresh shell asks Claude for one short reply
with the new token, and each running T3 server must have been started with that
same token. Each host's line ends with `Claude replies · T3 has the token`, or
with what failed. The exit status is nonzero when a reachable machine fails.

This machine goes last. Run from a terminal, its T3 restarts and is checked
like the hosts. Run from inside T3 (an agent or a T3 terminal: `T3CODE_HOME` is
set), restarting now would end the command with it, so it checks Claude here,
schedules the restart for five seconds later, and exits. Once T3 is back:

```sh
subs check claude
```

`subs check claude` changes nothing. It makes the same check here and on every
host, one small request each, and reports T3 servers still running an older
token, a host with a different token, a rejected token, or a usage limit.
`--local` checks only this machine. Only the managed setup-token is checked: a
request with a shared OAuth login could refresh, and so rotate, it. A plain
`subs push claude` of a token checks each host too, but only notes a T3 that
still has the older token, since it was not asked to restart it.

### From an agent

Without a terminal, `--code-file=PATH` waits for the sign-in page's code to
appear in that file, reads it once and removes it. The path must not exist yet.

```sh
subs login claude you@example.com --token --code-file="$PWD/claude-code" \
  --use --restart-t3 > login.log 2>&1 &
# login.log shows the sign-in link; open it and sign in as that account.
# Put the code the page shows into place in one step:
cat /private/code-from-the-user > claude-code.part && mv claude-code.part claude-code
wait
```

The link goes to stderr, the code never does; nothing in the log is secret.

## Hosts

Switching a login also installs it on every machine in `~/.config/subs/hosts`,
over `ssh`. One host per line, `#` comments, and optionally the providers a host
takes:

```
desktop
laptop   claude
```

![subs use](docs/use.gif)

Offline hosts are skipped and get it on the next push. A host that already has
the same login with a newer token keeps it. `subs use --local` switches only this
machine.

A cleared live login is refused before any host is contacted or T3 is restarted.
`subs use` can restore a saved login even when that account is already selected
but its live credential was cleared. A revoked saved login still needs a fresh
browser sign-in; restoring the file cannot make a revoked token valid again.

T3 can retain a previous Claude token in its process environment. To restart
active T3 services after installing Claude credentials:

```sh
subs use claude you@example.com --restart-t3
subs push claude --restart-t3
subs login claude you@example.com --use --restart-t3
```

This is opt-in because it interrupts active T3 runs. Remote services restart
only after their credential installation succeeds; an inactive service stays
inactive. The source machine restarts last. For a setup-token outside T3 it
restarts at once and is checked; inside T3, and for OAuth logins, it is
scheduled five seconds after fanout finishes, so a push launched inside T3 can
finish first. Output distinguishes restarted, scheduled, skipped, and failed
restarts. A restart failure returns a
nonzero exit status even when the credentials were installed. This option is for CLI Claude fanout and
cannot be combined with `--local`; normal pushes and the TUI do not restart T3.

On Linux, this handles the `t3code.service` systemd user service. Its `ExecStart`
must source the managed `subs/claude-shell.sh` hook before executing T3; a missing
hook is reported as a failure rather than claiming a restart repaired the login.
Keep the service's existing executable, arguments and environment. For example,
the shell portion of its `ExecStart` can look like this (systemd doubles literal
dollar signs):

```ini
ExecStart=/bin/sh -c '. "$${XDG_CONFIG_HOME:-$$HOME/.config}/subs/claude-shell.sh"; exec /absolute/path/to/t3 serve --host 127.0.0.1 --port 3773 --no-browser'
```

On macOS, Python 3 and `launchctl` are needed. `subs` discovers running T3
servers from the receiving user's `~/Library/LaunchAgents/*.plist`; no machine
names or service labels are built in. It preserves each server's executable,
arguments and preferences, adding the managed hook if needed. Tokens are read
from private configuration at startup, never written into a plist. Changed jobs
are unloaded and reloaded, with bounded retries while launchd removes the old
job; later reloads use `kickstart`. Inactive jobs are left alone. A local reload
is scheduled through launchd after the same five-second delay. Platforms with
neither supported service manager are reported as skipped.

Checkpoint running agents before opting into a restart. Files and independent
jobs survive; commands owned by a T3 process may need to run again. A confirmed
service restart does not prove the app has finished database migrations; the
setup-token check proves a fresh request works and that T3 started with the
token. Check the T3 connection before resuming unfinished work. Do not repeat
completed tasks just because their earlier failure notices remain visible.

Data lives in `~/.local/share/subs` (`$SUBS_HOME` overrides): mode 0700,
passwords in plain text at 0600.

## CLI versions

Hosts mostly run the CLIs headless (T3, cron, ssh), where Claude's own updater
never runs, so they fall behind the versions new models need. Every push
therefore also brings the host's CLI up to at least this machine's version,
with the CLI's own updater: `claude install <version>` for a native Claude,
`claude update`, `codex update` or `cursor-agent update` otherwise. The push
line shows the host's version, or the change (`claude 2.1.278 → 2.1.325`).

`subs update` first updates the CLIs here to the latest, then brings every host
up to them, without touching logins:

```sh
subs update                    # claude, codex and cursor-agent
subs update claude --local     # just this machine
subs update claude --restart-t3
```

A host without the CLI is left without it. T3 keeps the CLI version it saw at
startup, so it still says "Update Claude Code" until restarted;
`--restart-t3` restarts active T3 user services after the update, as for a push.

## Claude setup-token

Normal OAuth access tokens are short-lived and should renew automatically; a
manual browser login every few hours is not the intended workflow. Copying the
same access/refresh credential to several machines can break that renewal when
one machine rotates the shared refresh token. Restoring an expired saved copy
does not fix a provider-rejected refresh token.

Use a long-lived subscription token when copying the normal OAuth login means
frequent reauthentication. `subs` creates one with Claude's own browser flow
and remembers which account you signed in with:

```sh
subs login claude you@example.com --token
```

For unattended agents across several machines, the complete flow is:

```sh
subs login claude you@example.com --token --use --restart-t3
```

This uses the same subscription and OAuth authorization, generating the
long-lived model-only token rather than sharing a rotating browser login. It
checks the new token, installs it privately, pushes it to configured hosts and
reloads supported active T3 services. With no T3 sessions to reload, omit
`--restart-t3`. If the token is already installed, use
`subs push claude --restart-t3` after checkpointing agents instead of logging in
again. No account, host inventory or credential belongs in this repository.

`subs` runs `claude setup-token` on a private terminal in a throwaway config
and shows only its sign-in link. Open it, sign in as that account, and paste
the code the page shows at the hidden prompt (a browser on the same machine
finishes by itself). The token never reaches your terminal, a log, or a command
line. Before it replaces anything, `subs` checks it with one small model request
through `claude -p --model haiku`; a failed check, a wrong code, or Ctrl-C
leaves the current login exactly as it was.

Without a terminal (over a pipe, or from an agent), `--browser` reads the code
as one line on stdin while the link goes to stderr. Plain stdin without
`--browser` still imports a token you already have, unchecked:

```sh
subs login claude you@example.com --token --browser < code-fifo
chmod 600 /absolute/path/to/private-token
subs login claude you@example.com --token < /absolute/path/to/private-token
```

An existing token pasted at the code prompt is checked and installed the same way.

Accounts are shown by email, with a small `[token]` or `[OAuth]` badge for
Claude's login method. The email is your label, not a provider-verified
identity: Claude issues these tokens for model requests only, so they cannot
report their account. The card says `user-labelled`; JSON has
`identity_source: "user_label"` and `identity_verified: null`. Without a
label, the card says `Claude subscription`. Add or correct the label of an
installed token without signing in again:

```sh
subs label claude you@example.com
```

The token and a saved OAuth login for the same email share one card; the OAuth
snapshot remains on disk. The label lives in `claude-account.json` (0600) beside
the env file, bound to that token and its installation time, so the env file
stays exactly what older versions read. Labels travel with token pushes;
relabelling keeps the token and its original installation time.

This installs `~/.config/subs/claude.env` atomically with mode 0600. If
`XDG_CONFIG_HOME` is set, the path is `$XDG_CONFIG_HOME/subs/claude.env`.
The file exports `CLAUDE_CODE_OAUTH_TOKEN` and unsets `ANTHROPIC_API_KEY`.
It is never saved in an account snapshot. Symlinks, loose permissions, and
storage inside a Git checkout are refused when loading it. Reinstalling
replaces the old token; do not edit the generated env file.

An idempotent hook sources the environment from `.profile`, `.bashrc`, and
`.zshrc` (under `ZDOTDIR` when set), plus existing `.bash_profile`/`.bash_login`.
It refuses to source the token if its permissions become unsafe. Open a new
shell, or load it into the current shell, then push using your existing hosts:

```sh
. "${XDG_CONFIG_HOME:-$HOME/.config}/subs/claude-shell.sh"
subs status
subs json
subs push claude
```

`subs login claude --token --use` combines installation and push; `--local`
suppresses the push. Token pushes use the same SSH transport and host list as
OAuth pushes. They preserve the original installation timestamp and leave
Claude's account JSON and settings alone. On each VM, open a fresh interactive
shell or source the same hook explicitly (also do this in noninteractive jobs):

```sh
. "${XDG_CONFIG_HOME:-$HOME/.config}/subs/claude-shell.sh"
test "${ANTHROPIC_API_KEY+x}" != x && echo 'API key unset'
test -n "${CLAUDE_CODE_OAUTH_TOKEN:+present}" && echo 'subscription token present'
claude
```

Use `/status` inside Claude to inspect the authentication method. Restart existing
Claude sessions; `subs` cannot change the environment of an already-running shell
or process. Keep the managed hook after any custom credential exports.

Status/TUI show token presence, permission/format failures, and estimated expiry.
JSON adds `setup_token` metadata to the token row (or the first Claude row when
absent). `valid` and `expires_at` remain null; `state` is `unverified` for a
usable local file, and `estimated_expires_at` is installation time plus 365 days.
This is an estimate, not verification: importing an older token or reinstalling
the same token can overestimate its remaining life. Remote pushes preserve the
estimate. A setup-token's account is only your label; its usage stays unknown.

[Claude's documentation](https://code.claude.com/docs/en/authentication#generate-a-long-lived-token)
describes these as one-year subscription tokens that can only make model requests.
Only the browser flow checks a new token, with one small model request; status
never makes one. They do not provide
Remote Control or claude.ai connectors, and Claude's `--bare` mode ignores them.

Normal `subs login claude` still saves an OAuth login in isolation. While a
setup-token is installed, it stays selected until you explicitly activate OAuth
with `subs use claude OTHER`, `subs login claude --use`, or, for the account the
token is labelled with, `subs use claude EMAIL --oauth`. Plain `subs use` (or
enter in the TUI) on that account keeps the working token. Switching removes
the managed token locally and on pushed hosts; source the hook again or open
new shells.
Claude login subprocesses strip both inherited token and API key variables;
`subs` warns when `ANTHROPIC_API_KEY` is present without printing its value.

Cursor uses `cursor-agent login`, isolated via `XDG_CONFIG_HOME` on Linux or
`HOME` on macOS. Auth lives in `~/.config/cursor/auth.json` on Linux
(`$XDG_CONFIG_HOME/cursor/auth.json` when set), or `~/.cursor/auth.json` on
macOS. Email comes from JWT claims or `cursor-agent status --format json`;
JWT expiry is checked when available. Plan and usage come from the same read-only DashboardService
queries as `cursor-agent`’s `/usage`: included usage percentage and on-demand
USD spend (converted from reported cents). Missing fields stay unknown.
All Cursor-specific login, credential, usage, and push code lives in
`src/prov/cursor.rs`; the shared commands dispatch to it. No API key is needed.
`cli-config.json` is configuration, not the login, and is not copied.
Push respects the remote Linux `XDG_CONFIG_HOME`, writes auth atomically with
mode 0600 in a 0700 parent directory, and keeps a newer token for the same
account. It repairs permissions even when auth is unchanged, without stopping
processes. If not signed in, run `NO_OPEN_BROWSER=1 cursor-agent login` and
open the printed browser URL; then run `subs push cursor`.

## Spending

Status cards (including the TUI) show spend when the provider reports an amount.
With a configured setup-token selected, saved OAuth logins with unavailable
usage are shown as inactive. They do not describe the active token's health.
`setup-token: configured` means the local token file passed format and permission
checks; it does not claim a provider verification. Account identity and usage
reporting are unsupported by this token. JSON uses `usage_status` values
`available`, `unsupported`, `inactive`, or `error`. Unsupported token usage and
inactive OAuth logins have no `err` field; saved-login diagnostics are available
as `inactive_reason`. The token metadata keeps the `unverified` state and unknown
validity until a real provider check is performed. Token file failures and
active-login failures remain errors.
Unavailable dollar spend and absent setup-tokens are omitted from the cards.
Saved accounts that need a token refresh show a muted status; accounts with
cleared credentials show the command to sign in again. Status never refreshes
tokens itself, since that can invalidate credentials copied to other machines.
`subs json` adds a
`spend` object with `used`, `cap`, `currency`, `period`, and `reason`. Unknown
amounts are null, never zero. Codex and Claude show the provider’s plan and
usage windows; subscription dollar spend is n/a because those APIs do not
expose it. Cursor reports on-demand charges separately from its flat subscription.
Providers without a login still appear, with unknown spend. Credits are labelled as credits, without converting them to dollars.
