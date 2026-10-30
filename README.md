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
