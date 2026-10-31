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

## Use

```sh
subs                                    # status of every account
subs tui                                # interactive view
subs login codex|claude|cursor [EMAIL]  # sign in; --use makes it active and pushes it
                                        # (signing in as the active account always does)
subs use codex|claude|cursor EMAIL      # make a saved login active and push it to your hosts
subs push codex|claude|cursor           # push the active login to your hosts again
subs add codex|claude|cursor EMAIL      # store the account's password
subs pass codex|claude|cursor EMAIL     # copy (or print) that password
subs json                               # status as JSON
subs rm codex|claude|cursor EMAIL       # forget an account
```

![subs](docs/status.gif)

TUI keys: `j`/`k` move, `enter` use (and push), `f` push again, `l` login, `n` new,
`a` password, `p` copy password, `d` delete, `r` refresh, `q` quit.

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

Data lives in `~/.local/share/subs` (`$SUBS_HOME` overrides): mode 0700,
passwords in plain text at 0600.

## Claude setup-token

Use a long-lived subscription token when copying the normal OAuth login means
frequent reauthentication. Generate it yourself using Claude's browser flow:

```sh
unset ANTHROPIC_API_KEY CLAUDE_CODE_OAUTH_TOKEN
claude setup-token
subs login claude --token
```

Paste the generated token at the hidden prompt and press Enter. The token is
never accepted as a command argument. For a raw token already in a private
file outside a repository, use stdin instead:

```sh
chmod 600 /absolute/path/to/private-token
subs login claude --token < /absolute/path/to/private-token
```

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
estimate. The account identity and usage of a setup-token remain unknown.

[Claude's documentation](https://code.claude.com/docs/en/authentication#generate-a-long-lived-token)
describes these as one-year subscription tokens that can only make model requests.
`subs` does not make a billable request to validate one. They do not provide
Remote Control or claude.ai connectors, and Claude's `--bare` mode ignores them.

Normal `subs login claude` still saves an OAuth login in isolation. While a
setup-token is installed, it stays selected until you explicitly activate OAuth
with `subs use claude EMAIL` or `subs login claude --use`. That removes the managed
token locally and on pushed hosts; source the hook again or open new shells.
Claude login subprocesses strip both inherited token and API key variables;
`subs` warns when `ANTHROPIC_API_KEY` is present without printing its value.

Cursor uses `cursor-agent login`, isolated via `XDG_CONFIG_HOME` on Linux or
`HOME` on macOS. Auth lives in `~/.config/cursor/auth.json` on Linux
(`$XDG_CONFIG_HOME/cursor/auth.json` when set), or `~/.cursor/auth.json` on
macOS. Email comes from JWT claims or `cursor-agent status --format json`;
JWT expiry is checked when available. Plan and usage come from the same read-only DashboardService
queries as `cursor-agent`’s `/usage`: included usage percentage and on-demand
USD spend (converted from reported cents). Missing fields stay unknown.
Push writes the default auth path on each host without stopping processes.

## Spending

Status cards (including the TUI) show spend per account. `subs json` adds a
`spend` object with `used`, `cap`, `currency`, `period`, and `reason`. Unknown
amounts are null, never zero. Codex and Claude show the provider’s plan and
usage windows; subscription dollar spend is n/a because those APIs do not
expose it. Cursor reports on-demand charges separately from its flat subscription.
Providers without a login still appear, with unknown spend. Credits are labelled as credits, without converting them to dollars.

Bifrost shows the OpenRouter dev virtual key `neocat-demo-dev-20261030`, with
its governance budget’s current USD usage, cap, and reset period. Its intended
cap is USD 10; the displayed cap always comes from the API. Missing keys,
budgets, or credentials show `unavailable` with a reason. It is read-only and
cannot be selected for login, password, switching, deletion, or push actions.

The Bifrost module reads `/bifrost/prod/ADMIN_USERNAME` and
`/bifrost/prod/ADMIN_PASSWORD` from AWS SSM in `us-east-1`, using the existing
`fansvine` SSH host and its AWS identity. Set `SUBS_BIFROST_SSM_HOST` to another
SSH alias, or an empty value to use local AWS credentials. SSH requires `timeout`
(GNU coreutils, also available on macOS). It then sends only GET requests to
`https://bifrost.fansvine.com/api/governance/virtual-keys` and the matching key’s
detail endpoint. Credentials travel through captured pipes and curl stdin;
no admin credentials or virtual key values enter status output or saved state.
Spend is fetched on demand with the existing refresh, without an accounting
cache or background service.
