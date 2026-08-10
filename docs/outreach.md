# Outreach

Copy-paste-ready posts for announcing fuckmemory. All numbers below are real:
measured on the current release (see the Benchmarks section of the README).

## Hacker News — Show HN

> **Title:** Show HN: fuckmemory – one local memory for every AI coding agent
>
> Your coding agents forget everything between sessions, and each forgets
> separately. Claude Code learns your deploy process on Monday; Codex re-derives
> it on Tuesday; Cursor asks you again on Wednesday.
>
> fuckmemory is one local temporal knowledge graph that every agent on your
> machine reads and writes through MCP. `curl … | bash` and it detects Claude
> Code, Codex, Gemini, Antigravity, Qwen, Cursor and Copilot CLI, wires them to
> the same store, and turns on autosave — every prompt you send is stored, and
> relevant memories are injected back into every prompt.
>
> The part I think is actually new:
> - **No LLM in the write path.** The agent calling `remember` *is already a
>   model*, so the schema invites structured facts instead of paying Mem0/Zep to
>   run a second model to extract entities. A write is an INSERT plus a static
>   embedding: ~172 µs.
> - **Memory is involuntary, not a habit.** Tools only fire when the model
>   decides to call them, and agents forget to call `remember` exactly when
>   they're deep in something worth remembering. The hook path keeps every
>   prompt whether or not the model thought to ask.
> - **Time travel for free.** Facts have two time axes (valid_from/valid_to +
>   recorded_at/invalidated_at). "we use pnpm" isn't deleted when you switch —
>   it's closed out, and `--as-of 2026-03-01` still answers what you believed.
> - **Memories point at files.** Store a snippet with the memory, and recall
>   shows you *where* the convention lives (`Makefile`:1–6), not just that it
>   exists.
>
> It's 9 MB, one SQLite file, no cloud, no API keys. `fuckmemory bench` will
> reproduce the timings on your own machine. Rust, MIT.
>
> https://github.com/dalsori/fuckmemory

## Reddit

### r/rust

> **Title:** A local memory layer shared by every coding agent you use — in Rust
>
> Mem0 and Zep charge you an LLM call per write to extract entities from text.
> The insight here: the agent calling `remember` is already a model, so fuckmemory
> just asks it for structured facts. A write is an INSERT plus a static embedding
> (~172 µs), and retrieval fuses BM25 + int8 embeddings + graph neighbors with RRF.
>
> The mmap'd embedding cache is worth a look on its own — it reimplements the
> Bert tokenizer so a cold start is ~1 ms instead of 206 ms, verified to cosine
> 0.999999 against the reference model before it's trusted.
>
> `cargo install fuckmemory` (once it's on crates.io) or the one-liner. MIT.
> https://github.com/dalsori/fuckmemory

### r/LocalLLaMA

> **Title:** Agents forget between sessions — I built a local shared memory for all of them
>
> Every agent on your machine (Claude Code, Codex, Gemini, Antigravity, Qwen,
> Cursor, Copilot CLI) reads and writes one local knowledge graph through MCP.
> No cloud, no API key, no LLM call in the write path.
>
> What's interesting vs Mem0/Zep:
> - writes are ~172 µs (INSERT + static embedding), not hundreds of ms of a model call
> - memory is involuntary: hooks keep every prompt, not just what the model remembered to store
> - facts carry two time axes, so "what did I believe last March?" is answerable
> - recall fuses BM25 + embeddings + graph expansion, capped at a token budget
>
> `fuckmemory bench` reproduces the numbers on your box. MIT, Rust.
> https://github.com/dalsori/fuckmemory

### r/ClaudeAI (or r/Cursor)

> **Title:** Claude Code forgets everything between sessions — fixed with a local shared memory
>
> I kept re-explaining my repo to Claude Code every Monday. fuckmemory is a
> single local store that every agent writes to and reads from: `remember` is
> instant (no LLM call), and with autosave every prompt you send is kept and
> relevant memories are injected back automatically.
>
> One-liner install, wires into 7 agents, one SQLite file, no cloud.
> https://github.com/dalsori/fuckmemory

## X / Twitter

> **Thread idea (3 tweets):**
>
> 1. Claude Code learns your deploy process Monday. Codex re-learns it Tuesday.
>    Cursor asks you Wednesday. I built one local memory every agent reads and
>    writes through MCP. No cloud, no API key. 9 MB. 🧵
>
> 2. The write path has no LLM call — the agent calling `remember` is already a
>    model. A write is an INSERT + static embedding (~172 µs). Facts have two
>    time axes so "what did I believe in March?" is answerable after an overwrite.
>    Autosave keeps every prompt, involuntary, and injects memories back.
>
> 3. `curl -fsSL …install.sh | bash` → detects Claude Code, Codex, Gemini,
>    Antigravity, Qwen, Cursor, Copilot CLI, wires them all to the same store,
>    autosave on. Rust, MIT.
>    https://github.com/dalsori/fuckmemory

## Awesome lists

Submit a PR adding a link. Suggested targets (most are MCP/Claude ecosystems):

- **awesome-mcp-servers** — https://github.com/punkpeye/awesome-mcp-servers
  Add to the memory section: `fuckmemory — Local-first temporal knowledge graph
  shared by every AI coding agent, with autosave hooks and zero LLM writes.`
- **awesome-claude-code** — https://github.com/davila7/claude-code-templates or
  the community equivalent
- **awesome-codex** — search GitHub for the current Codex list

For the PR body: "Adds fuckmemory — a local-first MCP memory server with
autosave hooks for 7 agents, bi-temporal facts, and a ~172 µs write path.
Repo: https://github.com/dalsori/fuckmemory"

## Suggested posting order

1. Show HN (biggest multiplier; reply to comments for ~2 h)
2. r/rust + r/LocalLLaMA the same day
3. X thread
4. Awesome-list PRs (low effort, permanent backlinks)

**Wait until after crates.io + Homebrew are live** so the install line in every
post is `cargo install fuckmemory` / `brew install fuckmemory`, not just curl.
