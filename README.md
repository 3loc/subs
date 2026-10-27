# subs

See and switch ChatGPT/Codex and Claude subscription logins: who is active,
usage, time to reset, banked full resets.

## Install

Needs Rust (`cargo`), `curl`, and the `codex` and/or `claude` CLI. Linux or macOS.

```sh
git clone git@github.com:3loc/subs.git && cd subs && make install
```

Installs to `~/.local/bin/subs` (`make install PREFIX=/usr/local` for system-wide,
`make uninstall` to remove).

## Use

```sh
subs                              # status of every account
subs tui                          # interactive view
subs login codex|claude [EMAIL]   # device/paste-code login; --use also activates it
subs use codex|claude EMAIL       # make a saved login the active one
subs add codex|claude EMAIL       # store the account's password
subs pass codex|claude EMAIL      # copy (or print) that password
subs json                         # status as JSON
subs rm codex|claude EMAIL        # forget an account
```

TUI keys: `j`/`k` move, `enter` use, `l` login, `n` new, `a` password, `p` copy
password, `d` delete, `r` refresh, `q` quit.

Data lives in `~/.local/share/subs` (`$SUBS_HOME` overrides): mode 0700,
passwords in plain text at 0600.
