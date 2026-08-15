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

```console
$ fuckmemory install
detected  Claude Code, OpenCode, Qwen Code, Codex, GitHub Copilot CLI
command   /home/you/.local/bin/fuckmemory

  ✓ claude-code    register      /home/you/.claude.json
  ✓ codex          register      /home/you/.codex/config.toml
  ✓ opencode       register      /home/you/.config/opencode/opencode.jsonc
  ✓ qwen           register      /home/you/.qwen/settings.json
  ✓ copilot-cli    register      /home/you/.copilot/mcp-config.json
  ✓ instructions   instructions  /home/you/.claude/CLAUDE.md

Restart your agents so they pick up the new MCP server.
```

Turn on autosave and it stops depending on the agent's discipline entirely:
every prompt you send is stored, and relevant memories are handed back on every
prompt, whether or not the model thought to ask.

```console
$ fuckmemory install --autosave
...
  ✓ claude-code    autosave      /home/you/.claude/settings.json
autosave  on — every prompt is stored, memories are injected back
```

## Highlights

- **Shared across agents, automatically.** One `install` wires every agent on
  the machine to the *same* store. Claude Code remembers something on Monday;
  OpenCode and Codex recall it on Tuesday. No per-agent setup, no manual MCP
  configuration — one temporal graph on your disk.
- **Involuntary memory.** With `--autosave`, every prompt is kept and relevant
  memories are fed back into every prompt, whether or not the agent thought to
  call a tool. Hooks reach Claude Code, Codex, Gemini CLI, Antigravity, Qwen,
  Cursor and Copilot CLI.
- **Instant writes.** No LLM runs in the write path — a `remember` is an INSERT
  plus a static embedding, averaging **~5 ms** per prompt even under autosave.
- **Fused retrieval that stays in budget.** BM25 + embeddings + graph votes are
  ranked by Reciprocal Rank Fusion, deduped by MMR, and hard-capped at a token
  budget, so recall never floods a context window.
- **Memories point at files.** `remember` attaches the file a memory was learned
  against (path + line range + bounded snippet), so recall shows you *where* a
  convention lives, not just that it exists.
- **Time travel.** Every fact has two time axes; `--as-of` answers what you
  believed on any past date even after the fact was overwritten.
- **Self-managing.** `fuckmemory update` upgrades itself, `doctor --fix` repairs
  a broken install, and one `curl | bash` line installs everything with autosave
  on.
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

The two ideas that matter:

**No LLM in the write path.** Mem0 and Zep spend hundreds of milliseconds to
seconds per write asking a model to extract entities from your text. But the agent
calling the tool *is already a model*. So the `remember` schema invites it to hand
over structured facts directly, and falls back to cheap lexical extraction when it
doesn't. A write is an INSERT plus a static embedding.

**Memory shouldn't be voluntary.** MCP tools only fire when the model decides to
call them, and agents forget to call `remember` exactly when they are deep in
something worth remembering. So the same store has a second, involuntary path
through the agent's hooks: every prompt is kept, and every prompt gets memories
injected back. See [Autosave](#autosave).

**Facts have two time axes.** `valid_from`/`valid_to` say when something was true
in the world; `recorded_at`/`invalidated_at` say when you learned it. So "the
project uses npm" is not deleted when you switch to pnpm — it is closed out, and
`--as-of 2026-03-01` can still answer what you believed back then. This is the
Graphiti model, without Neo4j.

## Install

> Full per-OS walkthroughs, everyday usage, config and troubleshooting live in
> the [usage guide](docs/en/usage.md) ([español](docs/es/usage.md)).

**The one-liner** (no Rust toolchain, no clone) — installs the newest prebuilt
binary and wires every detected agent, autosave included:

```bash
curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | bash
```

Add `--no-autosave` to skip the per-prompt hooks. On macOS and Linux you can
also install with Homebrew:

```bash
brew tap dalsori/fuckmemory
brew install fuckmemory
```

Or grab a prebuilt binary for
your platform from the [releases](https://github.com/dalsori/fuckmemory/releases)
directly. Assets are named `fuckmemory-<target>.tar.gz` (Unix) or
`fuckmemory-<target>.zip` (Windows):

| Target | When |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux x86_64 |
| `aarch64-unknown-linux-gnu` | Linux ARM64, e.g. Raspberry Pi |
| `x86_64-apple-darwin` | Intel Macs |
| `aarch64-apple-darwin` | Apple Silicon Macs |
| `x86_64-pc-windows-msvc` | Windows x86_64 |

Then run `fuckmemory install` to wire your agents.

Or install from crates.io (needs Rust 1.85+ — `rustup` from https://rustup.rs):

```bash
cargo install fuckmemory
fuckmemory install          # detect agents, register, download the model
```

Or build from source:

```bash
git clone https://github.com/dalsori/fuckmemory && cd fuckmemory
./install.sh
```

Or manually:

```bash
cargo install --path .
fuckmemory install          # detect agents, register, download the model
```

`cargo install` drops the binary in `~/.cargo/bin`; `install.sh` uses
`~/.local/bin` and removes the `~/.cargo/bin` copy so the two never drift apart.
`cargo install fuckmemory` is the same thing but pulls the published crate.

`install` is idempotent, backs up every file it edits, and `fuckmemory uninstall`
reverses it. Add `--dry-run` to see the plan first.

## Upgrading

Your memories live somewhere the binary never touches — everything you stored is
under `~/.local/share/fuckmemory` (or `$FUCKMEMORY_HOME`): database, model,
config. Upgrading only replaces the executable, so **nothing is lost**. Run
`fuckmemory doctor` afterwards to confirm the store is intact and the agent
configs are still wired.

If you keep a clone of the repo:

```bash
cd fuckmemory
git pull
./install.sh                     # rebuild + replace the binary in place
```

Once installed, the simplest upgrade is the binary updating itself:

```bash
fuckmemory update              # check, download the newest release, replace this binary
fuckmemory update --check      # report what's available without touching anything
```

`update` downloads the same asset CI publishes for your platform, verifies the
remote tag is newer than the running version, and swaps the binary in place —
no manual `curl`, no knowing the version number.

Or pull the newest binary from a release by hand (replace `1.1.2` with the
latest version number):

**Linux / macOS:**

```bash
ver=1.1.2                                        # or whatever is latest
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)              target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-x86_64)             target=x86_64-apple-darwin ;;
  Darwin-arm64)              target=aarch64-apple-darwin ;;
esac
curl -L -o /tmp/fuckmemory.tar.gz \
  https://github.com/dalsori/fuckmemory/releases/download/v$ver/fuckmemory-$target.tar.gz
tar -xzf /tmp/fuckmemory.tar.gz -C /tmp
install -m 755 /tmp/fuckmemory ~/.local/bin/fuckmemory
```

**Windows (PowerShell):**

```powershell
$ver = "1.1.2"   # or whatever is latest
$target = "x86_64-pc-windows-msvc"
Invoke-WebRequest `
  "https://github.com/dalsori/fuckmemory/releases/download/v$ver/fuckmemory-$target.zip" `
  -OutFile "$env:TEMP\fuckmemory.zip"
Expand-Archive "$env:TEMP\fuckmemory.zip" "$env:TEMP\fuckmemory" -Force
# put fuckmemory.exe on your PATH, or reference it directly below
```

Then point your agents at the fresh binary — this rewrites the configs, is
idempotent, and edits nothing of yours:

```bash
fuckmemory install --command "$(which fuckmemory)" --autosave
# Windows: fuckmemory install --command "$env:LOCALAPPDATA\fuckmemory\fuckmemory.exe" --autosave
```

For `cargo install` users: `cargo install --path . --force` in the repo.

Releases are built automatically in CI for every target in the table above, so
`curl`/`Invoke-WebRequest` never gets a 404. Check what moved between versions
in the [CHANGELOG](CHANGELOG.md).

### What gets written where

`install` touches two kinds of file per agent: the MCP server registration, and
the instruction file that tells the agent *when* to call the tools. Without the
second one the tools exist and nothing ever calls them.

| Agent | MCP config | Instructions | Autosave hooks |
|---|---|---|---|
| Claude Code | `~/.claude.json`, `./.mcp.json` | `CLAUDE.md` | `./.claude/settings.json` |
| OpenAI Codex CLI | `~/.codex/config.toml` | `AGENTS.md` | `./.codex/hooks.json` |
| Gemini CLI | `~/.gemini/settings.json` | `AGENTS.md` | `./.gemini/settings.json` |
| Antigravity CLI | `~/.gemini/config/mcp_config.json`, `./.agents/mcp_config.json` | `AGENTS.md` | `~/.gemini/config/hooks.json`, `./.agents/hooks.json` |
| OpenCode | `~/.config/opencode/opencode.json[c]` | `AGENTS.md` | `~/.config/opencode/plugins/fuckmemory.js`, `./.opencode/plugins/fuckmemory.js` |
| Qwen Code | `~/.qwen/settings.json` | `AGENTS.md` | `./.qwen/settings.json` |
| Cursor | `~/.cursor/mcp.json` | `AGENTS.md` | `./.cursor/hooks.json` |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` | `AGENTS.md` | `~/.copilot/hooks/fuckmemory.json`, `./.github/hooks/fuckmemory.json` |
| VS Code | `./.vscode/mcp.json` | `AGENTS.md` | — |
| Kimi Code | prints a snippet — format unverified | `AGENTS.md` | — |

The instruction block lives between `<!-- fuckmemory:begin -->` and
`<!-- fuckmemory:end -->` markers, so re-running `install` replaces it in place
and never duplicates or clobbers what you wrote around it.

With `--autosave`, it also writes hooks into the agents that support them
(Claude Code, OpenAI Codex CLI, Gemini CLI, Antigravity CLI, Qwen Code, Cursor,
Copilot CLI, OpenCode) — see below.

Everything the tool writes lives in one directory you can delete:
`~/.local/share/fuckmemory` (override with `FUCKMEMORY_HOME`).

## Autosave

```bash
fuckmemory install --autosave     # or toggle it in `fuckmemory tui`
```

This wires `fuckmemory hook prompt` into each agent's per-prompt hook, so every
prompt goes through the store on its way to the model. The hook format is the
Anthropic JSON shape used by Claude Code (`settings.json`), Codex
(`hooks.json`) and Gemini CLI (`settings.json`); only the `timeout` unit differs
(seconds for Claude Code and Codex, milliseconds for Qwen and Gemini). Cursor
and Copilot CLI use their own flat `hooks.json` shapes (`bash` keys, camelCase
events). Antigravity uses a named-map `hooks.json` with `PreInvocation`/`Stop`
events, and since its prompt event only points at the conversation transcript,
the prompt is read back from there. OpenCode has no settings-file hook channel,
so install writes a plugin into `~/.config/opencode/plugins/` (or
`.opencode/plugins/` per project) that runs the same hook on every user message
and injects the recalled context back. Agents without a verified hook format
are left alone rather than guessed at.

Recall-injection only happens where the hook channel can carry context back
(Claude Code, Codex, Qwen, Gemini, Antigravity, OpenCode); Cursor and Copilot
CLI autosave every prompt but their hooks do not accept injected context, so
autorecall for them relies on the model calling the MCP tools.

- **Every prompt is kept**, verbatim, as a searchable episode. Nothing is lost.
- **Only prompts that read like durable knowledge become facts.** "never force
  push to main" and "prefiero pnpm en este repo" enter the graph as a constraint
  and a preference; "fix the flaky test in `src/db.rs`" stays an episode.
  Otherwise a week of chores would outrank every real decision you ever stored.
- **Acknowledgements are dropped.** "ok", "sí", "continue", `/compact`.
- **Credentials never reach the disk.** Tokens with known prefixes, assignments
  to secret-ish keys and opaque blobs are replaced with `[redacted]` first.
- **Sensitive files are ignored by name.** Add globs to `[ignore] paths`
  (`.env`, `*.pem`, `~/.aws/*`) and any prompt naming a matching file is
  redacted too — even when the path itself carries no recognizable token.
- **Nothing is paraphrased.** No model runs in this path either, so the stored
  text is exactly what you typed, minus redactions.

The classification is marker-based and bilingual (English/Spanish), which means
it is honest about what it can tell: "looks like a rule" or "don't know". It has
no opinion about anything subtler.

Auto-recall is the other half: before the prompt reaches the model, memory is
searched and the results are handed back as context, capped at
`autorecall.budget_tokens`. Prompts with no query signal ("ok", `/clear`) skip
this, because injecting memories there just spends context on a coin flip.

At session end, the hook consolidates instead of storing: duplicates merge and
the indexes compact, at the one moment nobody is waiting on it.

The whole round trip costs **~5 ms per prompt**, which is only possible because
of the embedding cache below — on the old path it would have added a quarter of
a second to every message you send.

## The settings screen

```bash
fuckmemory tui
```

```
┌ fuckmemory — 11 facts · 3 scopes · 128 KiB ────────────────────────┐
│  Settings │ Memories                                              │
└────────────────────────────────────────────────────────────────────┘
┌ settings ─────────────────────────┐┌ autosave ──────────────────────┐
│ ▸ autosave         on             ││Store every prompt you send,    │
│     scope          project        ││without the agent having to call│
│     min length     12 chars       ││`remember`. Turning this on     │
│     derive facts   on             ││wires a hook into each agent    │
│     redact secrets on             ││that supports one.              │
│   auto-recall      on             │└────────────────────────────────┘
│     memories       6              │┌ agents ────────────────────────┐
│     token budget   600 tokens     ││claude-code  tools ✓ autosave ✓ │
│   semantic search  on             ││codex        tools ✓ autosave ✓ │
│   fast embed cache on             ││qwen         tools ✓ autosave ✓ │
│   recall budget    1200 tokens    ││opencode     tools ✓ autosave — │
└───────────────────────────────────┘└────────────────────────────────┘
 ↑↓ move · space toggle · ←→ adjust · s save · r rebuild cache · q quit
```

Saving does not just write `config.toml` — flipping autosave also writes or
removes the hooks in each agent, and the Agents pane reads those files back, so
a toggle that says "on" while nothing is wired is visible rather than silent.

The Memories tab lists what is actually stored, newest first, with `/` to search
and `x` to retract. It is the fastest way to find out whether autosave is
keeping the right things.

Settings pinned by a `FUCKMEMORY_*` variable are marked `env` and cannot be
changed from the screen, because the variable would keep winning.

## What the agent sees

Four MCP tools. Deliberately four — every tool description is spent from the
agent's context window on every single turn.

- **`recall`** — search memory before assuming anything. Returns a
  token-budgeted list of standalone facts.
- **`remember`** — store something that will matter next session. Optional
  structured `facts` (`src`/`rel`/`dst`) make retrieval much better. Pass
  `files` (path, plus optional `line_from`/`line_to`, or a ready-made `snippet`)
  to attach the file a memory was learned against — recall then points straight
  at the source, and shows the excerpt in `--debug`.
- **`forget`** — retract something wrong or obsolete. Soft by default.
- **`timeline`** — how knowledge about one entity changed over time.

A recall renders like this, hard-capped at a token budget. Memories that name a
file show it backticked with the line range. The bracketed note is provenance —
**who** recorded the memory (`by claude-code`), **from what repository state**
(`@ <commit>`, the short git HEAD at write time), **when** it was true or was
retracted, and confidence when it isn't full:

```markdown
## Memory — myproject
- deploys go out through fly.io, never vercel [by codex @ ab12cd0 since 2026-07-20]
    `Makefile`:1–6
- the project uses pnpm now, npm left duplicate lockfiles [since 2026-03-14]
- never commit with --no-verify, the pre-commit hook is the only formatter
```

## How retrieval works

Three independent retrievers vote, and their **ranks** are fused with Reciprocal
Rank Fusion — ranks, not scores, because a BM25 score and a cosine are not on a
comparable scale.

1. **BM25** (SQLite FTS5) — exact tokens: `--no-verify`, `src/db.rs`,
   `potion-retrieval-32M`. Embeddings are bad at these.
2. **Vector** — static embeddings ([Model2Vec](https://github.com/MinishLab/model2vec)
   `potion-retrieval-32M`), int8-quantized, brute-force SIMD scan. Catches
   paraphrase: "how do I ship this" → "deploys go out through fly.io".
3. **Graph** — facts that neither matched, but which hang off an entity that did.
   Ask about "github actions" and you also get "CI runs on Node 22". This is the
   part a flat vector store structurally cannot do.

Then MMR drops near-duplicates, because ten phrasings of "use pnpm" is the
failure mode that quietly eats a context window.

Static embeddings are the reason this is fast: encoding is a tokenize plus an
embedding-table lookup and a mean pool — no transformer forward pass, no ONNX, no
GPU. And int8 vectors mean a linear scan beats an ANN index at this scale while
staying exact, with no index to rebuild on every write.

`fuckmemory explain <query>` shows each retriever's raw view, including the cosine
of every fact and whether it cleared the relevance floor. Use it if a recall
surprises you, or after changing models.

## Cold start

Loading the model through `model2vec-rs` costs **206 ms**: it reads a 129 MB f32
safetensors file, converts every value into a `Vec<f32>`, and builds a tokenizer
out of 1.5 MB of JSON. Encoding a query then takes 16 µs and a whole recall
0.3 ms. So 99% of a short-lived invocation was setup — survivable for the
long-lived MCP server, fatal for autosave, which spawns a process per message.

So the model is precompiled into a cache that needs no parsing at all:

- the embedding table as f16, one row per **token id**, with `mapping`/`weights`
  already folded in, so pooling is a plain row average;
- the vocabulary as a byte-sorted index into a string blob — a token lookup is a
  binary search straight against the mapped file, no HashMap to build;
- the file is `mmap`ed, so opening it costs a few page faults and a query only
  touches the ~20 rows it pools.

The catch is that the tokenizer then has to be reimplemented — BertNormalizer,
BertPreTokenizer, WordPiece, added tokens — and any drift from HuggingFace would
make stored and query vectors quietly incomparable. So the cache is verified
against the real model on a probe corpus (accents, CJK, identifiers, punctuation
runs, added tokens, uncoverable words) before it is installed, and a cache that
disagrees is deleted rather than used. Measured worst-case agreement: **cosine
0.999999**.

| | before | after |
|---|---|---|
| Model load | 206 ms | **0.09 ms** |
| `fuckmemory recall`, whole process | 231 ms | **4 ms** |
| Autosave hook, per prompt | (255 ms) | **5 ms** |
| Encode one query | 16 µs | 11 µs |

Building the cache costs 0.5 s once per model and 62 MB on disk; `install` and
`model pull` do it for you. `fuckmemory model verify` re-checks it against the
model, and `FUCKMEMORY_FAST=0` turns the whole thing off.

## Benchmarks

Reproducible with `fuckmemory bench` (or `./bench.sh`, which prints this table
and an ASCII chart plus the machine it measured). All timings are medians over
30 rounds on a throwaway store in the temp dir — your real database is never
touched. 10,000 facts, release build:

| metric | semantic on | semantic off |
|---|---|---|
| write (per `remember`) | **172 µs** | 141 µs |
| recall (per query) | 14.5 ms | 7.7 ms |
| recall, persisted index | 10.7 ms | — |
| recall, hot cache | 9.8 ms | — |

```
write                        172 µs ▕█
recall                     14506 µs ▕█████████████████████████████
recall (disk)              10673 µs ▕██████████████████████
recall (hot)                9833 µs ▕███████████████████
```

The `recall` line is the cold path — it re-reads every vector out of SQLite,
which is why it alone stays behind the others. The persisted line is what an
autosave hook or CLI recall actually pays now: the index lives in an mmap'd file
keyed by the store version, so a one-shot process opens it in milliseconds.
The `hot cache` line is a long-lived MCP server, where the index never leaves
the process at all. Seeding those 10,000 facts costs ~0.3 ms each, and a full
`remember` (INSERT + static embedding) is under a quarter of a millisecond.

Measured on an AMD Ryzen 7 7445HS (12 cores), Linux x86_64, 2026-08-10. To get
your own numbers: `fuckmemory bench` or `./bench.sh`.

## CLI

```
fuckmemory install [--dry-run] [--only ids] [--project] [--no-model]
                   [--autosave] [--no-hooks]
fuckmemory uninstall
fuckmemory tui                     # interactive settings, agents, memories
fuckmemory agents                  # what's installed, and where it's configured
fuckmemory doctor                  # paths, schema, model, cache, registrations
fuckmemory doctor --fix            # repair what the check finds, automatically
fuckmemory update [--check]        # upgrade this binary from the latest release

fuckmemory hook prompt|session-end [--agent id] [--text ...]
                                   # agents call this; reads the payload on stdin

fuckmemory remember <text> [--kind decision|preference|constraint|error]
                           [--src X --rel uses --dst Y] [--since YYYY-MM-DD]
                           [--file PATH[:FROM-TO]] ...   # attach a file/snippet
fuckmemory recall <query> [--limit N] [--as-of DATE] [--hops 0-2]
                          [--budget N] [--raw] [--debug] [--json]
fuckmemory forget <id> | --query <text> [--hard]
fuckmemory timeline <entity>
fuckmemory explain <query>         # per-retriever breakdown

fuckmemory task save <state> [--file F,...] [--goal "what we're doing"]
fuckmemory task status            # resume: what the previous session left
fuckmemory task done [--note ...] # close the task; last save stays searchable
fuckmemory task remember <text>   # set the checkpoint AND store a durable memory

fuckmemory stats | scopes
fuckmemory bench [--facts N] [--rounds N] [--queries N]   # write/recall latency
fuckmemory consolidate             # merge duplicates, compact indexes
fuckmemory prune --days 90         # drop retracted, never-read facts
fuckmemory reindex                 # re-embed after changing models
fuckmemory model pull | which | cache [--force] | verify
fuckmemory export | import <file>
fuckmemory serve                   # MCP stdio; agents call this
```

### Resuming interrupted work

An agent loses its context when a session ends — a token budget hits, the
machine reboots, or a different agent takes over. `task save` records what the
work is doing *right now* so the next session — same agent or a different one —
can pick up exactly where the last one stopped instead of re-deriving the plan
and re-touching files:

```bash
# The agent that is about to be interrupted (or is mid-work and wants a checkpoint):
fuckmemory task save "wired the opencode plugin, verifying install/uninstall" \
  --file src/install.rs --goal "add opencode autosave hooks"

# Whoever resumes (maybe a different agent, maybe tomorrow):
fuckmemory task status
```

Every `task save` is also stored as a searchable episode, so a resuming agent
finds it through `recall` even before it thinks to ask for `task status`.
Updating the checkpoint keeps the original goal and open date; `task done`
closes the task while leaving its last state readable, and a final note records
what shipped or what blocked it.

### Scopes

Memories are namespaced. A project scope is keyed to the repository root, so
subdirectories share it. The `global` scope holds facts about *you* that travel
between projects — a recall reads the current project **and** `global` together.

```bash
fuckmemory remember --scope global "no emoji in commit messages, ever"
```

### Configuration

Settings live in `$FUCKMEMORY_HOME/config.toml`, written by `fuckmemory tui` and
safe to edit by hand. Environment variables override it, so a variable set for
one shell always wins:

```toml
model = "minishlab/potion-retrieval-32M"
budget_tokens = 1200
semantic = true
fast = true                  # the mmap'd embedding cache

[autosave]
enabled = true
min_chars = 12               # below this a prompt is not a memory
facts = true                 # promote salient prompts into the graph
scope = "project"            # or "global"
redact = true

[autorecall]
enabled = true
limit = 6
budget_tokens = 600

[ignore]
paths = [".env", "*.pem", "~/.aws/*"]   # redact prompts naming these files
```

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

The default model is **125 MB** on disk (32M parameters in f32).
`FUCKMEMORY_MODEL=minishlab/potion-base-8M` is ~4x smaller and loads ~4x faster,
at some retrieval quality. Run `fuckmemory reindex` after switching — vectors from
two different models are not comparable.

### Keeping it tidy

A memory store that is never consolidated decays into an append-only log that
poisons retrieval. With autosave on, the session-end hook consolidates for you.
Otherwise run it from a cron:

```bash
fuckmemory consolidate && fuckmemory prune --days 90
```

## Honest limitations

- **Static embeddings are the weak leg.** They have no contextual understanding —
  a query like "how do I ship this to production" ranks the right memory second,
  not first. This is why the vector leg votes at 0.7 weight against BM25's 1.0,
  and why BM25 and the graph carry most of the load.
- **A truly irrelevant query still returns one hit.** The vector relevance floor
  is relative to the best match, because measured cosines make an absolute
  threshold impossible: good matches land at 0.09–0.21 while an unrelated query
  still peaks at 0.12. Ranges overlap, so no fixed cutoff separates them.
- **The vector index is persisted, but rebuilt when the store changes.** The
  index is cached to an mmap'd file keyed by the store version, so one-shot
  commands (`recall`, the autosave hook) open it in milliseconds instead of
  re-reading every vector. A write anywhere invalidates it, so a busy store
  rebuilds — fine to ~100k facts (~90 ms), the point where the rebuild itself
  starts to matter. A long-lived MCP server keeps one index in process and only
  reloads on `data_version` changes.
- **JSONC comments are lost.** Configs with comments (OpenCode, VS Code) are
  rewritten as plain JSON. A backup is kept next to the original and a warning is
  printed. TOML keeps its comments — `toml_edit` preserves them.
- **Consolidation does not use an LLM**, so it merges near-identical facts but
  will not notice that two differently-worded facts contradict each other unless
  they share a `src` + single-valued `rel`.
- **Kimi Code** is detected but its MCP config format is unverified, so a snippet
  is printed rather than a guess written into a real config file.
- **Not every agent's hook carries context back.** Autosave reaches Claude Code,
  Codex, Gemini CLI, Antigravity, Qwen, Cursor, Copilot CLI and OpenCode; recall
  injection works through the ones whose hook channel accepts context (Claude
  Code, Codex, Qwen, Gemini, Antigravity, OpenCode). Cursor and Copilot CLI
  autosave every prompt but ignore injected context, so autorecall there relies
  on the model calling the MCP tools.
- **Salience is marker-based.** No model runs in the write path, so a rule phrased
  without any of the markers ("the replica lags by an hour") is kept as an
  episode, not promoted to a fact. Recall reads facts; `recall --raw` reads both.
- **Redaction is heuristic.** It catches known token formats, assignments to
  secret-ish keys and opaque blobs, plus any path matching an `[ignore]` glob.
  A password that looks like an English word in a file you did not list will
  survive it, so it is a safety net, not a guarantee.

## Development

```bash
cargo test                  # 189 tests: 165 unit, 24 integration
cargo build --release
```

The integration tests drive the real binary as a subprocess — including a live
MCP handshake over stdio, eight concurrent writers, and eight processes racing
to migrate a fresh database. Those caught the bugs that mattered, none of which
a unit test would have found: deferred transactions returning `SQLITE_BUSY`
under concurrent writes, a non-transactional migration, and `busy_timeout` being
set after the one pragma that needed it. End-to-end runs against a real store
caught two more: the vector
leg ignoring the temporal filter (so `--as-of` silently returned today's beliefs),
and `AtomicI64::fetch_update` returning the *previous* value, which made the first
`now()` of every process return 0. Adding autosave surfaced one more: `PRAGMA
journal_mode = WAL` returns `SQLITE_BUSY` *without* consulting the busy handler,
so `busy_timeout` never protected it and a fresh store opened by several
processes at once could fail outright — rare with one long-lived server, routine
once a hook runs on every prompt.

Layout:

| File | |
|---|---|
| `db.rs` | schema, migrations, pragmas |
| `scope.rs` | project/global namespacing |
| `embed.rs` | static embeddings, int8 quantization, vector scan |
| `fast.rs` | the mmap'd model cache and its verified tokenizer |
| `hook.rs` | autosave, auto-recall, salience, redaction |
| `tui.rs` | the settings screen |
| `store.rs` | the LLM-free write path, temporal invalidation |
| `graph.rs` | fact reads, neighbour expansion, time travel |
| `retrieve.rs` | BM25 + vector + graph, RRF, MMR |
| `pack.rs` | token-budgeted rendering |
| `consolidate.rs` | dedup, pruning, reindex |
| `mcp.rs` | stdio JSON-RPC server |
| `install.rs` | agent detection and config patching |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and the [changelog](CHANGELOG.md). Report
vulnerabilities via [SECURITY.md](SECURITY.md) — not in public issues.

## License

MIT
