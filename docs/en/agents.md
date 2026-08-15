# Agents

`fuckmemory install` detects the coding agents on this machine and registers
itself as an MCP server in each one. It also writes an instruction block telling
the agent when to call the tools.

```bash
fuckmemory install            # register MCP everywhere
fuckmemory install --autosave # also wire the per-prompt hooks
fuckmemory agents             # see what was detected and where it is configured
```

Restart your agents after installing so they pick up the MCP server.

## What gets written where

| Agent | MCP config | Instructions | Autosave hooks |
|---|---|---|---|
| Claude Code | `~/.claude.json`, `./.mcp.json` | `CLAUDE.md` | `./.claude/settings.json` |
| OpenAI Codex CLI | `~/.codex/config.toml` | `AGENTS.md` | `./.codex/hooks.json` |
| Gemini CLI | `~/.gemini/settings.json` | `AGENTS.md` | `./.gemini/settings.json` |
| Antigravity CLI | `~/.gemini/config/mcp_config.json`, `./.agents/mcp_config.json` | `AGENTS.md` | `~/.gemini/config/hooks.json`, `./.agents/hooks.json` |
| OpenCode | `~/.config/opencode/opencode.json[c]` | `AGENTS.md` | `~/.config/opencode/plugins/fuckmemory.js`, `./.opencode/plugins/fuckmemory.js` |
| Qwen Code | `~/.qwen/settings.json` | `AGENTS.md` | `./.qwen/settings.json` |
| Cursor | `~/.cursor/mcp.json` | `AGENTS.md` | `./.cursor/hooks.json` |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` | `AGENTS.md` | `~/.copilot/hooks/fuckmemory.json`, `./.github/hooks/fuckmemory.json` |
| VS Code | `./.vscode/mcp.json` | `AGENTS.md` | — |
| Kimi Code | prints a snippet — format unverified | `AGENTS.md` | — |

The instruction block lives between `<!-- fuckmemory:begin -->` and
`<!-- fuckmemory:end -->` markers, so re-running `install` replaces it in place
and never duplicates or clobbers what you wrote around it.

Everything the tool writes lives in one directory you can delete:
`~/.local/share/fuckmemory` (override with `FUCKMEMORY_HOME`).

## The four MCP tools the agent sees

- **`recall`** — search memory before assuming anything.
- **`remember`** — store something that will matter next session.
- **`forget`** — retract something wrong or obsolete (soft by default).
- **`timeline`** — how knowledge about one entity changed over time.

Next: [turn on autosave](autosave.md).
