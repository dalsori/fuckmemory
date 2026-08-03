# Contributing

Thanks for wanting to help. This file is short on purpose — the README's
[Development](README.md#development) section has the build and test commands.

## Setup

```bash
cargo build --release
cargo test                  # 136 tests: 117 unit, 19 integration
cargo fmt --check           # must be clean
cargo clippy --all-targets -- -D warnings
```

## Ground rules

- **`cargo fmt` before you commit.** CI enforces it.
- **Clippy must be clean with `-D warnings`.** CI enforces it.
- **Write a regression test** for anything that broke. The integration tests in
  `tests/integration.rs` drive the real binary as a subprocess — concurrency,
  migrations, and MCP handshake bugs only ever surfaced there.
- **Cold start is a correctness constraint.** Hooks spawn a process per message;
  nothing may regress the ~1 ms fast path (`src/fast.rs`) without a strong reason.
- **The write path stays LLM-free.** `remember` is an INSERT plus a static
  embedding. Model calls belong in offline utilities, never in the hot path.

## Where things live

See the module table in `src/lib.rs` for the layer layout.

## Opening a PR

- One logical change per PR. Rebase onto `master` rather than merging it in.
- Describe *why*, not just what. If it changes behaviour, say how you verified it.
- This repo does not use `--no-verify`; the CI gate is the point.

## Reporting bugs

Open an issue. For anything security-related, use
[SECURITY.md](SECURITY.md) instead.
