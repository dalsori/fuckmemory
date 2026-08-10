# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`fuckmemory bench` measures write and recall latency.** Seeds a throwaway
  store with N facts, then reports median write (`remember`) and recall timings
  with and without the embedding model, plus the steady-state "hot cache" number
  a running MCP server pays. `./bench.sh` wraps it into a markdown table + ASCII
  chart annotated with the machine, so the README's numbers are reproducible.

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
