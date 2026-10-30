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
JWT expiry is checked when available. Cursor usage currently shows `usage n/a`.
Push writes the default auth path on each host without stopping processes.
