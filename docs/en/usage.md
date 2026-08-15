# Usage guide

Everything you need to install, use, and maintain `fuckmemory`, per operating
system. The README gives the one-glance summary; this is the detail.

## Install

`fuckmemory` is a single static binary. Nothing runs in the background and
nothing is sent anywhere: your memories live in one local directory.

### Linux / macOS

**One-liner (no Rust toolchain, no clone)** — installs the newest prebuilt
binary and wires every detected agent, autosave included:

```bash
curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | bash
```

Pass `--no-autosave` to skip the per-prompt hooks. On macOS and Linux you can
also use Homebrew:

```bash
brew tap dalsori/fuckmemory
brew install fuckmemory
```

Or grab a prebuilt binary from the
[releases page](https://github.com/dalsori/fuckmemory/releases). Assets are
named `fuckmemory-<target>.tar.gz`:

| Target | When |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux x86_64 |
| `aarch64-unknown-linux-gnu` | Linux ARM64 (e.g. Raspberry Pi) |
| `x86_64-apple-darwin` | Intel Macs |
| `aarch64-apple-darwin` | Apple Silicon Macs |

Then run `fuckmemory install` to wire your agents.

### Windows

Open **PowerShell** and download the zip for
`x86_64-pc-windows-msvc` from the
[releases page](https://github.com/dalsori/fuckmemory/releases):

```powershell
$target = "x86_64-pc-windows-msvc"
$ver = "<latest-version>"
Invoke-WebRequest `
  "https://github.com/dalsori/fuckmemory/releases/download/v$ver/fuckmemory-$target.zip" `
  -OutFile "$env:TEMP\fuckmemory.zip"
Expand-Archive "$env:TEMP\fuckmemory.zip" "$env:LOCALAPPDATA\fuckmemory" -Force
```

Add `%LOCALAPPDATA%\fuckmemory` to your PATH, or reference
`fuckmemory.exe` by its full path when installing:

```powershell
fuckmemory install --command "$env:LOCALAPPDATA\fuckmemory\fuckmemory.exe" --autosave
```

### Any OS, from source or crates.io

With Rust 1.85+:

```bash
cargo install fuckmemory
fuckmemory install
```

Or from a clone:

```bash
git clone https://github.com/dalsori/fuckmemory && cd fuckmemory
./install.sh
```

`install` is idempotent, backs up every file it edits, and `fuckmemory uninstall`
reverses it. Use `--dry-run` to see the plan first.

## Wire your agents

`fuckmemory install` detects the coding agents on this machine and registers
itself as an MCP server in each one. It also writes an instruction block telling
the agent when to call the tools.

```bash
fuckmemory install            # register MCP everywhere
fuckmemory install --autosave # also wire the per-prompt hooks
fuckmemory agents             # see what was detected and where it is configured
```

Restart your agents after installing so they pick up the MCP server.

## Autosave

With `--autosave`, every prompt you send is stored, and relevant memories are
injected back on every prompt — whether or not the model thought to ask. This
works through per-prompt hooks in Claude Code, Codex, Gemini CLI, Antigravity,
Qwen, Cursor, Copilot CLI and OpenCode.

```bash
fuckmemory install --autosave
```

Toggle it later with `fuckmemory tui`, or flip the settings in
`$FUCKMEMORY_HOME/config.toml`.

## Everyday commands

```bash
fuckmemory remember "deploys go out through fly.io"          # store a fact
fuckmemory recall "how do we deploy"                          # search memory
fuckmemory recall "deploy" --raw                              # include raw notes
fuckmemory forget 42                                          # retract (soft)
fuckmemory timeline fly.io                                    # history of an entity
fuckmemory explain "deploy"                                   # why this ranking?
```

Facts are stored once and shared by every agent on the machine: Claude Code
learns your deploy process, and OpenCode recalls it next session.

## Resuming interrupted work

When a session is cut short — a token budget hits, the machine reboots, or a
different agent takes over — leave a checkpoint so the next session picks up
where you stopped:

```bash
fuckmemory task save "wired the opencode plugin, verifying install/uninstall" \
  --file src/install.rs --goal "add opencode autosave hooks"
fuckmemory task status   # whoever resumes reads this
fuckmemory task done --note "shipped"
```

Every `task save` is also stored as a searchable episode, so `recall` finds it
even before someone asks for `task status`.

## Configuration

Settings live in `$FUCKMEMORY_HOME/config.toml` (default
`~/.local/share/fuckmemory/config.toml`, override with `FUCKMEMORY_HOME`):

```toml
model = "minishlab/potion-retrieval-32M"
budget_tokens = 1200
semantic = true
fast = true

[autosave]
enabled = true
min_chars = 12
facts = true
scope = "project"
redact = true

[autorecall]
enabled = true
limit = 6
budget_tokens = 600

[ignore]
paths = [".env", "*.pem", "~/.aws/*"]
```

Every setting has an environment override (`FUCKMEMORY_*`), documented in the
README. Variables always beat the file.

## Keeping it tidy

A store that is never consolidated decays into an append-only log. With
autosave on, the session-end hook consolidates for you. Otherwise run it from a
cron:

```bash
fuckmemory consolidate && fuckmemory prune --days 90
```

## Upgrading

Your memories live in `$FUCKMEMORY_HOME`, which the binary never touches.
Upgrading only replaces the executable:

```bash
fuckmemory update                # download the newest release and swap itself
fuckmemory update --check        # report what's available, change nothing
fuckmemory doctor                # confirm the store and wiring are intact
```

## Uninstall

```bash
fuckmemory uninstall
```

This reverses every byte `install` wrote. Your data under `$FUCKMEMORY_HOME` is
left alone; delete that directory too if you want it gone entirely.

## What it stores where

| Agent | MCP config | Autosave hooks |
|---|---|---|
| Claude Code | `~/.claude.json`, `./.mcp.json` | `./.claude/settings.json` |
| OpenAI Codex CLI | `~/.codex/config.toml` | `./.codex/hooks.json` |
| Gemini CLI | `~/.gemini/settings.json` | `./.gemini/settings.json` |
| Antigravity CLI | `~/.gemini/config/mcp_config.json`, `./.agents/mcp_config.json` | `~/.gemini/config/hooks.json`, `./.agents/hooks.json` |
| OpenCode | `~/.config/opencode/opencode.json[c]` | `~/.config/opencode/plugins/fuckmemory.js`, `./.opencode/plugins/fuckmemory.js` |
| Qwen Code | `~/.qwen/settings.json` | `./.qwen/settings.json` |
| Cursor | `~/.cursor/mcp.json` | `./.cursor/hooks.json` |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` | `~/.copilot/hooks/fuckmemory.json`, `./.github/hooks/fuckmemory.json` |
| VS Code | `./.vscode/mcp.json` | — |
| Kimi Code | prints a snippet — format unverified | — |

## Troubleshooting

- **The hook says "unknown hook event"** — your binary is newer than your
  agent's config, or older. Re-run `fuckmemory install --autosave` to rewrite
  the hooks, and `fuckmemory update` to make sure you are current.
- **`doctor` reports a model mismatch** — vectors were stored with one model and
  you switched. Run `fuckmemory reindex` to re-embed everything.
- **Agents don't pick up memories** — restart them after `install`, and check
  `fuckmemory agents` shows them wired.
- **Autosave slows a prompt down** — it should not: the whole round trip is a few
  milliseconds thanks to the mmap'd model cache. Run `fuckmemory bench` to see
  your own numbers.
