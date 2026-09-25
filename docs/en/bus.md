# Bus — active messaging between agents

The memory graph is passive: you have to `recall`. The bus is active: one agent `send`s, the other gets it on the next prompt without asking.

Scoped per project, stored in SQLite (`bus_messages` + `bus_cursors`), no server, no cloud.

## Commands

```bash
fuckmemory bus send "review the deploy before merging" --to codex       # directed
fuckmemory bus send "ship it when green" --channel ops                   # broadcast
fuckmemory bus send "short ping" --ttl 60                                # expires in 60s
fuckmemory bus poll --agent opencode --limit 10                          # unread for opencode (marks seen)
fuckmemory bus list --channel ops --limit 20                             # recent, does not mark seen
fuckmemory inbox --agent opencode                                        # alias for bus poll
fuckmemory inbox --channel ops
```

Flags:
- `--to <agent>` — `claude-code`, `codex`, `opencode`, `cursor`, etc. Omit for broadcast.
- `--channel <name>` — default `general`. Normalized: lowercased, non-alphanum → `-`, max 32 chars. `My Channel!` → `my-channel`.
- `--ttl <seconds>` — message expires after N seconds. `0` expires immediately.
- `--scope <path|global>` — project scope, default current project.
- `--from <agent>` — override sender (auto-detected otherwise).

## How it works

- **Per-agent cursor.** `bus_cursors(scope_id, agent, last_seen_id)` tracks the last message each agent has seen. `poll`/`inbox` return only messages with `id > last_seen` where `to_agent IS NULL OR to_agent = you`, then advance the cursor.
- **Injection.** Every `hook prompt` (when worth recalling) does a `poll` for the prompt's agent (max 5 messages, 400 token budget) and injects as `## Inbox` before recall. Second prompt won't repeat them.
- **TTL.** `created_at + ttl_ms <= now` are pruned on every send/poll, so expired messages never reach an inbox.
- **Concurrency.** SQLite WAL + `INSERT` is lock-free for readers; concurrent `bus send` from 8 agents never loses a message.

## Expected workflow

1. Claude plans, finds a gotcha: `fuckmemory bus send "staging replica lags 2m, don't assert immediately" --to opencode --channel ops`
2. You switch to OpenCode, type anything: hook injects the inbox, OpenCode reads it without calling a tool.
3. `bus list` is the audit trail (newest first), `bus poll` is the inbox (oldest first, marks seen).

## MCP

Two extra tools the agent sees:

- `bus_send { body, channel?, to_agent?, ttl_seconds?, scope? }`
- `bus_poll { channel?, limit?, scope? }` — same cursor logic, the caller is the poller via `client_name()`.

Next: [plugins](plugins.md) or [session](session.md).
