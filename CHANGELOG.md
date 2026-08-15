# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Track the in-progress task.** `fuckmemory task save|status|done` records the
  goal, current state and files a task is touching, so an interrupted session
  (token budget, crash, or a different agent taking over) can resume exactly
  where the last one stopped. Every save is also stored as a searchable episode,
  so `recall` finds it even before someone asks for `task status`.
- **OpenCode autosave + autorecall.** OpenCode has no settings-file hook
  channel, so `install --autosave` now writes a plugin into
  `~/.config/opencode/plugins/` (or `.opencode/plugins/` per project) that runs
  the shared hook on every user message and injects the recalled context back.
- **Windows path redaction.** Ignore globs now match Windows backslash paths
  (`C:\proj\.env` catches `**/.env`), and `~` expansion accepts `~\` as well as
  `~/`.
- **Per-OS usage guides.** A detailed walkthrough (install, autosave, task
  resumption, config, troubleshooting) now lives in `docs/en/usage.md` and
  `docs/es/usage.md`; the README keeps the one-glance summary.
- **Recall shows provenance.** Every memory line now answers "who wrote this,
  and from what state of the repo?" — the writing agent (`[by claude-code]`),
  the short git HEAD at the moment it was learned (`[@ ba85d94]`, captured on
  write when inside a repo), and the fact's confidence when below 1.0
  (`[conf 0.70]`), alongside the existing date and retraction markers. An agent
  that is handed a memory can therefore challenge it, not just absorb it.
- **Published on crates.io.** `cargo install fuckmemory` now works; docs.rs
  builds the crate reference automatically.
- **Ignore sensitive paths.** `[ignore] paths = [".env", "*.pem", "~/.aws/*"]`
  (or `FUCKMEMORY_IGNORE_PATHS`, comma-separated) redacts any prompt that names
  a matching file — globs support `*`, `**` and `?`, with `~` home expansion.
  Runs after the token redaction, so a path that no token rule would catch is
  still scrubbed from what autosave stores.

- **`fuckmemory bench` measures write and recall latency.** Seeds a throwaway
  store with N facts, then reports median write (`remember`) and recall timings
  with and without the embedding model, plus the steady-state "hot cache" number
  a running MCP server pays. `./bench.sh` wraps it into a markdown table + ASCII
  chart annotated with the machine, so the README's numbers are reproducible.
- **Persisted vector index.** The int8 vector index is written to an mmap'd file
  keyed by the store version and scope set, so a one-shot process — an autosave
  hook, a CLI recall — opens the mapped index instead of re-reading every vector
  out of SQLite. At 100k facts this cuts a cold recall from ~164 ms to ~91 ms,
  matching the in-process cache of a long-lived MCP server.
- **Repository polish for reach.** A custom social-preview image (the link
  preview on GitHub and social networks), a `FUNDING.yml`, and issue/PR
  templates.

### Changed

- **Autosave no longer rebuilds the vector index every prompt.** The persisted
  mmap index was keyed by `PRAGMA data_version`, which changes on *any* write —
  including the `mark_hits` popularity counter every prompt runs — so the next
  prompt paid a full rebuild (~90 ms at 100k facts). A dedicated `index_version`
  now only moves when fact-vector content changes, so consecutive prompts reuse
  the mapped index while real fact writes still invalidate it.
- **Consolidate dedup is scoped to new work.** Duplicate-fact merging compared
  every live fact against every other (O(n²)) on every session-end. It now only
  compares facts derived from the episodes being drained, so an idle session
  does no cosine work at all.
- **Faster retrieval internals.** `topk` keeps a bounded heap instead of sorting
  all candidates (O(n log k), memory O(k)); vector loads borrow the blob instead
  of allocating per row; MMR measures each pair once instead of per-round; file
  references load in one query per recall instead of one per episode.
- **WAL growth is bounded.** Consolidate runs a passive checkpoint at session
  end, so a long-lived MCP server's `-wal` file stops growing without bound.
- **Round-trip export/import keeps everything.** Import now restores raw
  episodes, their file references, and fact provenance, not just the distilled
  facts; re-importing is idempotent.
- **Two correctness fixes.** `--as-of` cache keys now include the full timestamp
  (the old `t % 255` collided across ~17-day windows), and `reindex` stores the
  real model id so `doctor` stops reporting a permanent mismatch.

## [1.2.0] - 2026-08-15

## [1.1.2] - 2026-08-09

### Added

- **Memories point at files.** `remember` accepts `files` — a path plus an
  optional line range, or a ready-made snippet — and stores a bounded excerpt of
  the file a memory was learned against. Recall shows the path (backticked, with
  the line range) next to the fact, and `--debug` reveals the stored excerpt with
  its detected language. CLI: `fuckmemory remember … --file PATH[:FROM-TO]`.
- **Antigravity autosave + auto-recall.** `install --autosave` now also wires
  hooks into Antigravity CLI (`~/.gemini/config/hooks.json` and
  `.agents/hooks.json`, `PreInvocation`/`Stop` events, named-map shape). Its
  prompt event does not carry the prompt text, so `hook prompt` reads the most
  recent user request back from the conversation transcript, and context is
  injected as an `ephemeralMessage` step.
- **`fuckmemory update` self-updates.** One command checks the latest GitHub
  release, downloads the asset for your platform, and atomically replaces the
  running binary — no shell, no manual download, no knowing the version number.
  `fuckmemory update --check` reports without touching anything.
- **`fuckmemory doctor --fix` repairs the install.** The check now records every
  problem it finds, and `--fix` acts on them: rebuilds a missing/drifted FTS
  index, fetches the embedding model and builds the fast cache, re-embeds facts
  that have no vectors, consolidates pending episodes, and re-wires hooks into
  agents that have autosave enabled but no hook configured.
- **One-command install.** `curl … | bash` downloads the newest prebuilt
  binary, installs it to `~/.local/bin`, and wires every detected agent with
  autosave on — no Rust toolchain, no clone, no manual download. From a repo
  checkout the same script still builds from source. `--no-autosave` opts out.

### Changed

- **Multi-platform release builds.** GitHub Actions now builds prebuilt binaries
  for Linux (x86_64 and arm64), macOS (Intel and Apple Silicon) and Windows, and
  attaches them to every published release as `fuckmemory-<target>.tar.gz`/`.zip`.
  The README documents how to upgrade on each OS.

## [1.1.1] - 2026-08-09

### Added

- **Autosave reaches more agents.** `install --autosave` now also wires
  per-prompt hooks into Gemini CLI (`hooksConfig.enabled` on, `BeforeAgent`),
  Cursor (`hooks.json`) and GitHub Copilot CLI (`hooks/*.json`, `bash` keys).
- **Memory is shared between agents automatically.** `install` detects every
  coding agent on the machine and wires the same store into all of them — no
  manual MCP setup, no per-agent copies. Anything one agent remembers is
  instantly recallable by every other.
- **Autosave + auto-recall.** With `install --autosave` the store is no longer
  voluntary: every prompt is kept, durable knowledge is extracted, and relevant
  memories are injected back into every prompt — across agents, with no model
  call in the write path (~5 ms per message).
- **Fused retrieval** ranking BM25, static-int8 embeddings and graph-neighbor
  expansion with Reciprocal Rank Fusion and MMR dedup, hard-capped by a token
  budget so recall never eats a context window.
- **Bitemporal graph with time travel.** `valid_from/valid_to` and
  `recorded_at/invalidated_at` in SQLite; `--as-of` answers what you believed on
  any past date.
- **~1 ms cold start** with the mmap'd, verified f16 embedding cache, and a
  `tui` for settings, agents and memories without touching config files.

## [0.1.0] - 2026-08-03

Initial release.

### Added

- MCP server over stdio with four tools: `recall`, `remember`, `forget`, `timeline`.
- Bitemporal knowledge graph in SQLite (`valid_from`/`valid_to`, `recorded_at`/`invalidated_at`) with `--as-of` time travel.
- Fused retrieval: BM25 (SQLite FTS5) + int8-quantized static embeddings + graph neighbor expansion, ranked by Reciprocal Rank Fusion and deduped by MMR.
- `install`/`uninstall`: auto-detects Claude Code, OpenCode, Codex, Gemini CLI, Qwen Code, Cursor, Copilot CLI, VS Code and Kimi Code, and wires the MCP server plus instruction files in place.
- Autosave: per-prompt hook that stores every prompt verbatim, promotes only durable knowledge into the graph, redacts credentials, and injects recalled memories back (Claude Code today).
- `fast.rs`: mmap'd f16 embedding cache that replaces the 206 ms model load with ~1 ms cold start, verified against the reference tokenizer (worst-case cosine 0.999999).
- `tui`: interactive settings, agents and memories screens.
- `explain`, `doctor`, `stats`, `scopes`, `consolidate`, `prune`, `reindex`, `export`/`import` CLI utilities.
