# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
