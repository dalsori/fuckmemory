# Task — resume interrupted work

An agent loses its context when a session ends — a token budget hits, the
machine reboots, or a different agent takes over. `task` records what the work
is doing *right now* so the next session can pick up exactly where the last one
stopped, instead of re-deriving the plan and re-touching files.

## The commands

```bash
# The agent that is about to be interrupted leaves a checkpoint:
fuckmemory task save "wired the opencode plugin, verifying install/uninstall" \
  --file src/install.rs --goal "add opencode autosave hooks"

# Whoever resumes (maybe a different agent, maybe tomorrow):
fuckmemory task status

# Close the task when it ships; the last state stays readable:
fuckmemory task done --note "verified, shipped"
```

`task remember` does `task save` and additionally stores the checkpoint as a
durable memory, for when you want `recall` to find it prominently.

## How it works

- The active checkpoint is a small JSON document in the database:
  the **goal**, the **current state**, the **files** being touched, and the
  open/updated timestamps.
- There is one active task at a time. Updating keeps the original goal and open
  date, so a long task keeps a single birth date.
- Every `task save` is also stored as a **searchable episode** with
  `kind=task`. So a resuming agent finds the checkpoint through `recall` even
  before it thinks to ask for `task status` — the machine never lets the thread
  go missing.
- `task done` closes the task; the checkpoint is kept in the database so a final
  `task status` (or `--as-of`) can still read it.

## Recommended workflow

1. When you start something non-trivial, `task save` once with the goal.
2. As you make progress, `task save` again with the updated state — the goal
   and open date are preserved automatically.
3. If the session is cut short, whoever continues runs `task status` first, and
   checks `recall` for any task episodes they might have missed.
4. When the work ships, `task done --note "..."` so the next task starts clean.

Next: [configuration](config.md).
