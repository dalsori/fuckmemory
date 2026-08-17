# fuckmemory

<p align="center">
  <b>One local memory, shared by every AI coding agent you use.</b><br/>
  <a href="https://github.com/dalsori/fuckmemory/actions/workflows/ci.yml">
    <img src="https://github.com/dalsori/fuckmemory/actions/workflows/ci.yml/badge.svg" alt="CI"/>
  </a>
  <a href="https://github.com/dalsori/fuckmemory/releases">
    <img src="https://img.shields.io/github/v/release/dalsori/fuckmemory" alt="version"/>
  </a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT"/></a>
  <img src="https://img.shields.io/badge/rust-1.85+-orange.svg" alt="Rust 1.85+"/>
</p>

Your agents forget everything between sessions, and they each forget separately.
Claude Code learns your deploy process on Monday; Codex re-derives it on Tuesday;
OpenCode asks you again on Wednesday. `fuckmemory` is a single temporal knowledge
graph on your machine that all of them read and write through MCP — no cloud, no
API keys, no LLM call in the write path, one 9 MB binary.

## Quick start

```bash
curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | bash
fuckmemory install --autosave
# restart your agents; from now on every prompt is stored and memories are
# injected back, automatically
```

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.ps1 | iex
```

Per-OS installs (Windows zip, Homebrew, crates.io), wiring agents, and every
feature have their own guide in the [docs](docs/) — one per task, in
[English](docs/en/index.md) and [español](docs/es/index.md). The README is the
one-glance summary; the docs are the detail.

## Highlights

- **Shared across agents, automatically.** One `install` wires every agent on
  the machine to the *same* store. Claude Code remembers something on Monday;
  OpenCode and Codex recall it on Tuesday.
- **Involuntary memory.** With `--autosave`, every prompt is kept and relevant
  memories are fed back into every prompt, whether or not the agent thought to
  call a tool.
- **Instant writes.** No LLM runs in the write path — a `remember` is an INSERT
  plus a static embedding, averaging **~5 ms** per prompt even under autosave.
- **Fused retrieval that stays in budget.** BM25 + embeddings + graph votes are
  ranked by Reciprocal Rank Fusion, deduped by MMR, and hard-capped at a token
  budget.
- **Memories point at files.** `remember` attaches the file a memory was learned
  against (path + line range + bounded snippet).
- **Time travel.** Every fact has two time axes; `--as-of` answers what you
  believed on any past date even after the fact was overwritten.
- **Resume interrupted work.** `fuckmemory task save|status|done` leaves a
  checkpoint so any agent can continue where a cut-short session stopped.
- **Owned by you.** One database, one model, one config directory; `uninstall`
  reverses every byte `install` wrote.

## Why another one

| | Mem0 | Zep / Graphiti | typical MCP memory servers | fuckmemory |
|---|---|---|---|---|
| Storage | vector + optional graph | temporal graph (Neo4j/FalkorDB) | JSON or SQLite | SQLite + temporal graph |
| Write cost | LLM call per write | LLM call per write | instant | instant |
| Runs offline | no | no | yes | yes |
| Time travel | no | yes | no | yes |
| Retrieval | vector | graph + search | keyword *or* vector | BM25 + vector + graph, fused |
| Installs into your agents | you wire it up | you wire it up | you wire it up | detects and wires itself |

**No LLM in the write path.** Mem0 and Zep spend hundreds of milliseconds to
seconds per write asking a model to extract entities from your text. But the
agent calling the tool *is already a model*. So the `remember` schema invites it
to hand over structured facts directly, and falls back to cheap lexical
extraction when it doesn't.

**Memory shouldn't be voluntary.** MCP tools only fire when the model decides to
call them. So the same store has a second, involuntary path through the agent's
hooks: every prompt is kept, and every prompt gets memories injected back.

**Facts have two time axes.** `valid_from`/`valid_to` say when something was true
in the world; `recorded_at`/`invalidated_at` say when you learned it. So "the
project uses npm" is not deleted when you switch to pnpm — it is closed out, and
`--as-of 2026-03-01` can still answer what you believed back then.

## The everyday commands

```bash
fuckmemory remember "deploys go out through fly.io"   # store a fact
fuckmemory recall "how do we deploy"                  # search memory
fuckmemory forget 42                                  # retract (soft)
fuckmemory timeline fly.io                            # history of an entity
fuckmemory explain "deploy"                           # why this ranking?
fuckmemory task status                                # resume interrupted work
```

## Autosave

```bash
fuckmemory install --autosave     # or toggle it in `fuckmemory tui`
```

Every prompt is kept verbatim as a searchable episode; only prompts that read
like durable knowledge become facts; acknowledgements are dropped; credentials
and named sensitive files are redacted before anything hits the disk. Relevant
memories are injected back into each prompt, capped at a token budget. At
session end the hook consolidates instead of storing. The whole round trip costs
**~5 ms per prompt**.

## How retrieval works

Three independent retrievers vote, and their **ranks** are fused with Reciprocal
Rank Fusion:

1. **BM25** (SQLite FTS5) — exact tokens: `--no-verify`, `src/db.rs`.
2. **Vector** — static embeddings, int8-quantized, brute-force SIMD scan. Catches
   paraphrase: "how do I ship this" → "deploys go out through fly.io".
3. **Graph** — facts that neither matched, but which hang off an entity that did.

Then MMR drops near-duplicates, because ten phrasings of "use pnpm" is the
failure mode that quietly eats a context window.

## Benchmarks

Reproducible with `fuckmemory bench` (or `./bench.sh`). 10,000 facts, release
build, AMD Ryzen 7 7445HS:

| metric | semantic on | semantic off |
|---|---|---|
| write (per `remember`) | **172 µs** | 141 µs |
| recall (per query) | 14.5 ms | 7.7 ms |
| recall, persisted index | 10.7 ms | — |
| recall, hot cache | 9.8 ms | — |

## Design decisions

These are deliberate trade-offs, not defects — the cost of the project's core
choice: **no LLM runs in the write path**, which is what makes writes instant,
offline and free.

- **Static embeddings have no contextual understanding.** A query like "how do
  I ship this to production" ranks the right memory second, not first. This is
  the price of instant, offline writes: contextual embeddings would need a model
  in the write path. BM25 and the graph carry most of the load for this reason.
- **Consolidation does not use an LLM.** It merges near-identical facts by
  similarity and closes contradictions that share a `src` + single-valued `rel`,
  but won't notice a contradiction between two differently-worded facts. That is
  the same trade-off: reasoning would cost a model call per consolidate.
- **Salience is marker-based.** A rule phrased without any of the markers is kept
  as an episode, not promoted to a fact. Honest by design — no model runs in the
  write path, so the only defensible answers are "looks like a rule" and "don't
  know", never a paraphrase.

## Honest limitations

- **A truly irrelevant query still returns one hit.** The vector relevance floor
  is relative to the best match, because measured cosines make an absolute
  threshold impossible: good matches land at 0.09–0.21 while an unrelated query
  still peaks at 0.12, and the ranges overlap.
- **Kimi Code** is detected but its MCP config format is unverified, so a snippet
  is printed rather than a guess written into a real config file.
- **Not every agent's hook carries context back.** Autosave reaches Claude Code,
  Codex, Gemini CLI, Antigravity, Qwen, Cursor, Copilot CLI and OpenCode; recall
  injection works through the ones whose hook channel accepts context. Cursor
  and Copilot CLI autosave every prompt but ignore injected context — a property
  of those agents, not a choice of ours.
- **Redaction is a safety net, not a guarantee.** It is the deliberate cost of a
  model-free write path: a credential that looks like an ordinary word ("the
  password is `hunter2`") or uses an unknown prefix can still reach the disk.
  It catches known formats, secret-ish keys, opaque blobs and any path matching
  an `[ignore]` glob — but the only way to close the gap would be an LLM
  reviewing every prompt, which would break the instant-write promise.

## Development

```bash
cargo test                  # 189 tests: 165 unit, 24 integration
cargo build --release
```

The integration tests drive the real binary as a subprocess — including a live
MCP handshake over stdio, eight concurrent writers, and eight processes racing
to migrate a fresh database. See [CONTRIBUTING.md](CONTRIBUTING.md) and the
[changelog](CHANGELOG.md). Report vulnerabilities via [SECURITY.md](SECURITY.md).

## License

MIT
