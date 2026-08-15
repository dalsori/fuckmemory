# Autosave

With `--autosave`, every prompt you send is stored, and relevant memories are
injected back on every prompt — whether or not the model thought to ask.

```bash
fuckmemory install --autosave     # or toggle it in `fuckmemory tui`
```

This wires `fuckmemory hook prompt` into each agent's per-prompt hook. Each
agent uses its own channel: Claude Code, Codex, Qwen and Gemini use settings
JSON hooks; Cursor and Copilot CLI use flat `hooks.json`; Antigravity a
named-map `hooks.json`; OpenCode a plugin in `~/.config/opencode/plugins/`.

Recall injection (the "memories are handed back" half) works through the agents
whose hook channel can carry context back: Claude Code, Codex, Qwen, Gemini,
Antigravity and OpenCode. Cursor and Copilot CLI autosave every prompt but their
hooks ignore injected context, so autorecall there relies on the model calling
the MCP tools.

## What it does

- **Every prompt is kept**, verbatim, as a searchable episode. Nothing is lost.
- **Only prompts that read like durable knowledge become facts.** "never force
  push to main" and "prefiero pnpm en este repo" enter the graph as a constraint
  and a preference; "fix the flaky test in `src/db.rs`" stays an episode.
- **Acknowledgements are dropped.** "ok", "sí", "continue", `/compact`.
- **Credentials never reach the disk.** Tokens with known prefixes, assignments
  to secret-ish keys and opaque blobs are replaced with `[redacted]` first.
- **Sensitive files are ignored by name.** Add globs to `[ignore] paths`
  (`.env`, `*.pem`, `~/.aws/*`) and any prompt naming a matching file is
  redacted too — the globs match Windows paths as well.
- **Nothing is paraphrased.** No model runs in this path, so the stored text is
  exactly what you typed, minus redactions.

## Auto-recall

Before the prompt reaches the model, memory is searched and the results are
handed back as context, capped at `autorecall.budget_tokens`. Prompts with no
query signal ("ok", `/clear`) skip this.

At session end, the hook consolidates instead of storing: duplicates merge and
the indexes compact, at the one moment nobody is waiting on it.

The whole round trip costs a few milliseconds per prompt, thanks to the mmap'd
model cache.

## Toggle it

`fuckmemory tui`, or flip the settings in `$FUCKMEMORY_HOME/config.toml`:

```toml
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
```

See [config](config.md) for the full reference.

Next: [everyday commands](commands.md).
