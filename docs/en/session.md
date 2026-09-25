# Session — resume the work, across agents

Claude Code works on a feature all morning, the machine reboots, and OpenCode
opens in the same project. The facts travel on their own (`recall` injects
them), but the *narrative* — the goal, the decisions taken, the files touched,
what was still in flight — used to be lost with the conversation. A **session**
is the container that keeps that narrative, and the first prompt of the fresh
conversation is handed it back automatically.

A session is keyed per project, groups the episodes and facts of a stretch of
work, and is shared by every agent: the agent's own conversation id is what
writes into it, but *whose* conversation it was doesn't matter.

## The commands

```bash
fuckmemory session start release --goal "ship 1.3"   # optional: a name
fuckmemory session list                                # what have we been doing
fuckmemory session show release                        # the full narrative
fuckmemory session end release                         # close it explicitly
```

## How it works

- **Automatic.** You don't have to start a session. With autosave on, the first
  prompt of a conversation creates one (`work-2026-08-26`), and every stored
  prompt after that is tagged into it. `session start <name>` is only for when
  you want a human handle and a goal.
- **Idle-closed.** A session stays open while it is active; once it has been
  quiet longer than the idle window (`[session] idle_hours`, default 24), the
  next prompt starts a fresh one. `session end` closes one early.
- **Cross-agent handoff.** When a *different* agent's conversation starts in a
  project whose session still holds work, the first real prompt is injected the
  accumulated context — name, goal, facts learned, recent episodes, and the
  active `task` checkpoint — before the ordinary recalled memories. Claude ends,
  OpenCode begins, and it reads "here is where the work is" without being told.
- **Model-free.** The context is a bounded render of what is already stored, so
  the "handoff" costs nothing to build and can never eat a context window.

## Heartbeats — who is still working

Every `hook prompt` touches a per-agent, per-scope heartbeat (`heartbeats` table, 3 min window). `session list` renders it herdr-style:

```
NAME                 STATE  OPENED     LAST        EPS  agents
work-2026-09-25      open   2026-09-25 2026-09-25    2  claude-code,opencode

agents (last seen):
  ● opencode       working 2026-09-25
  ● claude-code    working 2026-09-25
```

`● working` means seen <3 min ago; `○ idle` otherwise. No server, just SQLite.

## Recommended workflow

1. When you start something non-trivial, `session start <name> --goal "..."`.
2. Work in whatever agent you like. `session list` shows the sessions of a
   project and which agents wrote into each, plus the heartbeat footer.
3. When another agent (or a fresh conversation) picks the work up, the context
   is handed back automatically on the first prompt. `session show <name>` gives
   the same narrative on demand.
4. When the work ships, `session end <name>` so the next stretch starts clean.

The handoff can be turned off (`[autorecall] session = false`) and the idle
window tuned (`[session] idle_hours = 24`).

Next: [configuration](config.md).