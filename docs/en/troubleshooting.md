# Troubleshooting

Common problems and what to do about them.

## The hook says "unknown hook event"

Your binary is newer than your agent's config, or older. Re-run
`fuckmemory install --autosave` to rewrite the hooks, and `fuckmemory update`
to make sure you are current.

## `doctor` reports a model mismatch

Vectors were stored with one embedding model and you switched. Run
`fuckmemory reindex` to re-embed everything (or `doctor --fix`, which does it
automatically).

## Agents don't pick up memories

Restart them after `install` — MCP servers and hooks are read at startup. Then
check `fuckmemory agents` shows them wired.

## Autosave slows a prompt down

It should not: the whole round trip is a few milliseconds thanks to the mmap'd
model cache. Run `fuckmemory bench` to see your own numbers, and make sure
`fast = true` in the config.

## A recall returns something odd

```bash
fuckmemory explain "your query"   # see each retriever's raw view
```

This shows the cosine of every fact and whether it cleared the relevance floor,
so a surprising ranking can be reasoned about instead of guessed at.

## I pasted a secret and want it gone

```bash
fuckmemory forget <id> --hard     # delete permanently
```

Then run `fuckmemory consolidate && fuckmemory prune --days 0` to clean up the
raw episode.

## Building on Windows fails in `ar.exe` with error `0xc000012f`

Some Scoop/winlibs `gcc` builds ship a broken `ar.exe` that crashes with
`STATUS_BAD_IMAGE (0xc000012f)` the moment it loads the LTO plugin from
`lib\bfd-plugins\libdep.a` (a known upstream binutils packaging bug). Any
`cargo build --release` on the `x86_64-pc-windows-gnu` toolchain can hit it,
because the release profile uses `lto = "fat"`.

Fix the environment, not the project:

- Run the **one-line installer** instead of building — `irm
  https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.ps1 | iex`
  downloads the prebuilt binary and never touches cargo/gcc.
- Or install the stable toolchain: `rustup toolchain install stable-x86_64-pc-windows-msvc`
  and set it as default — the MSVC build does not use `ar.exe`.
- Or remove the broken plugin: rename `lib\bfd-plugins\libdep.a` in the Scoop
  gcc directory so `ar.exe` stops trying to load it.

## `fuckmemory` on the PATH is stale

If a flag that exists in the source rejects as "unexpected argument", the binary
on your PATH is a stale build — rebuild and reinstall rather than adding the
flag. `fuckmemory update` fixes the common case.

Back to [docs index](../README.md).
