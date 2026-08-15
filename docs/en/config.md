# Configuration

Settings live in `$FUCKMEMORY_HOME/config.toml` (default
`~/.local/share/fuckmemory/config.toml`), written by `fuckmemory tui` and safe
to edit by hand.

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

## Environment overrides

Every setting has a `FUCKMEMORY_*` variable that beats the file, so a variable
set for one shell always wins:

| Variable | Default | Meaning |
|---|---|---|
| `FUCKMEMORY_HOME` | `~/.local/share/fuckmemory` | database, settings, model |
| `FUCKMEMORY_MODEL` | `minishlab/potion-retrieval-32M` | HF repo id or local folder |
| `FUCKMEMORY_BUDGET` | `1200` | default recall token budget |
| `FUCKMEMORY_SEMANTIC` | on | `0` for BM25 + graph only, no model |
| `FUCKMEMORY_FAST` | on | `0` to always load the real model |
| `FUCKMEMORY_AUTOSAVE` | off | `1` to store every prompt |
| `FUCKMEMORY_AUTORECALL` | off | `1` to inject memories into every prompt |
| `FUCKMEMORY_REDACT` | on | `0` to store prompts unfiltered |
| `FUCKMEMORY_IGNORE_PATHS` | — | comma-separated globs of files to redact |

## The model

The default model is **125 MB** on disk (32M parameters in f32).
`FUCKMEMORY_MODEL=minishlab/potion-base-8M` is ~4x smaller and loads ~4x
faster, at some retrieval quality. Run `fuckmemory reindex` after switching —
vectors from two different models are not comparable.

`fast = true` uses a precompiled mmap cache so a cold invocation costs ~1 ms
instead of ~206 ms. `fuckmemory model cache --force` rebuilds it, and
`fuckmemory model verify` re-checks it against the real model.

## Scopes

Memories are namespaced. A project scope is keyed to the repository root, so
subdirectories share it. The `global` scope holds facts about *you* that travel
between projects — a recall reads the current project **and** `global` together.

```bash
fuckmemory remember --scope global "no emoji in commit messages, ever"
fuckmemory recall "commit" --scope global
```

## The settings screen

`fuckmemory tui` is the interactive way to flip these: autosave, auto-recall,
semantic search, the fast cache, token budgets, and a pane listing your actual
memories (with `/` to search and `x` to retract). Settings pinned by a
`FUCKMEMORY_*` variable are marked `env` and cannot be changed from the screen,
because the variable would keep winning.

Next: [maintenance](maintenance.md).
