# subs

See and switch ChatGPT/Codex and Claude subscription logins. One Python 3
file, stdlib only, no install step beyond putting it on `PATH`.

```
$ subs
codex
* work@example.com   free    30d   4%  29.4d   resets 2  credits 51959
  me@example.com     pro     5h   12%  2h13m  7d   40%   3.2d   resets 0
claude
* me@example.com     max20x  5h    2%  4h18m  7d    0%   4.7d
```

`*` is the account the CLI is logged in as right now. Each window is
`length  used%  time-to-reset`. `resets N` is Codex's banked full resets
(redeem them in the ChatGPT usage menu). Plan, usage and resets come from the
provider on every run, never from the token.

## Use

```
subs                          status of every account
subs tui                      interactive view (j/k, enter use, l login, n new,
                              a password, p copy, d delete, r refresh, q quit)
subs login codex|claude [EMAIL] [--use]
subs use   codex|claude EMAIL
subs add   codex|claude EMAIL store email + password (prompt, or stdin)
subs pass  codex|claude EMAIL copy password to clipboard, else print it
subs rm    codex|claude EMAIL
subs json                     status as JSON
```

`login` asks for the account's password if none is stored yet, copies it to
the clipboard, then runs the real sign-in. Codex uses its device-code flow
(`codex login --device-auth`: open the URL, type the code). Claude has no
device-code flow; `claude auth login --email EMAIL` is its headless
equivalent (open the URL, paste the code it shows back into the terminal).
Add `--use` to switch to the new login straight away.

## How it stays out of trouble

- **Logins run in a throwaway `CODEX_HOME` / `CLAUDE_CONFIG_DIR`.** Signing in
  to a new account never touches the one in use. `codex logout` / a fresh
  `codex login` over the live file revokes the old login server-side, and
  every running session and every machine holding a copy then fails with
  "you have since logged out or signed in to another account".
- **`use` is a file swap.** It saves the live credential first, writes the
  chosen one, and tells you how many running `codex`/`claude` processes still
  hold the old login. Restart them; they don't re-read a different account.
- **subs never refreshes a token.** A refresh rotates the token and kills
  every other copy of it. The real CLI refreshes the live one as it runs; an
  inactive login whose token has expired shows `token expired` until you
  `use` it and run the CLI once.

## Files

`${SUBS_HOME:-${XDG_DATA_HOME:-~/.local/share}/subs}`, mode 0700:

| Path | What |
|---|---|
| `accounts.json` | `{provider: {email: {"password": …}}}`, plaintext, 0600 |
| `codex/EMAIL.json` | a saved `~/.codex/auth.json` |
| `claude/EMAIL.json` | a saved Claude credential + `oauthAccount` |

Treat the directory like `~/.ssh`. Passwords are plaintext at rest, the same
protection the CLIs give their own tokens. Don't point `SUBS_HOME` into a
synced or version-controlled tree.

Honours `CODEX_HOME` and `CLAUDE_CONFIG_DIR`. On macOS the default Claude
login lives in the keychain (`Claude Code-credentials`); `subs` reads and
writes it there, and signs new Claude accounts in place (restoring the
previous one) because the keychain entry is tied to the config dir.

## Endpoints (undocumented; may change)

- `GET chatgpt.com/backend-api/wham/usage`: plan, windows, credits, resets
- `GET api.anthropic.com/api/oauth/usage`: 5h / 7d utilization and reset times
- `GET api.anthropic.com/api/oauth/profile`: email for a fresh Claude login
