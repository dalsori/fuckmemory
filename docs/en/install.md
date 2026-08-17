# Install

`fuckmemory` is a single static binary. Nothing runs in the background and
nothing is sent anywhere: your memories live in one local directory.

## Linux / macOS

**One-liner** (no Rust toolchain, no clone) — installs the newest prebuilt
binary and wires every detected agent, autosave included:

```bash
curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | bash
```

Pass `--no-autosave` to skip the per-prompt hooks. On macOS and Linux you can
also use Homebrew:

```bash
brew tap dalsori/fuckmemory
brew install fuckmemory
```

Or grab a prebuilt binary from the
[releases page](https://github.com/dalsori/fuckmemory/releases). Assets are
named `fuckmemory-<target>.tar.gz`:

| Target | When |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux x86_64 |
| `aarch64-unknown-linux-gnu` | Linux ARM64 (e.g. Raspberry Pi) |
| `x86_64-apple-darwin` | Intel Macs |
| `aarch64-apple-darwin` | Apple Silicon Macs |

Then run `fuckmemory install` to wire your agents — see [agents](agents.md).

## Windows

**One-liner** (no Rust toolchain, no clone) — installs the newest prebuilt
binary to `%LOCALAPPDATA%\fuckmemory`, adds it to your PATH, and wires every
detected agent, autosave included:

```powershell
irm https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.ps1 | iex
```

Run from a repo checkout to build from source instead:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1        # build + install + register
powershell -ExecutionPolicy Bypass -File install.ps1 -NoAutosave -NoModel -DryRun
```

Or do it by hand: grab the `fuckmemory-x86_64-pc-windows-msvc.zip` asset from
the [releases page](https://github.com/dalsori/fuckmemory/releases), extract it
somewhere on your PATH (or under `%LOCALAPPDATA%\fuckmemory`), then run:

```powershell
fuckmemory install --command "$env:LOCALAPPDATA\fuckmemory\fuckmemory.exe" --autosave
```

The installer passes the absolute binary path, so agents launched from a GUI
(which may have a different PATH than your shell) still find it.

## Any OS, from crates.io or source

With Rust 1.85+:

```bash
cargo install fuckmemory
fuckmemory install
```

Or from a clone:

```bash
git clone https://github.com/dalsori/fuckmemory && cd fuckmemory
./install.sh
```

`install` is idempotent, backs up every file it edits, and `fuckmemory uninstall`
reverses it. Use `--dry-run` to see the plan first.

## Upgrade

Your memories live in `$FUCKMEMORY_HOME`, which the binary never touches.
Upgrading only replaces the executable:

```bash
fuckmemory update                # download the newest release and swap itself
fuckmemory update --check        # report what's available, change nothing
fuckmemory doctor                # confirm the store and wiring are intact
```

## Uninstall

```bash
fuckmemory uninstall
```

This reverses every byte `install` wrote. Your data under `$FUCKMEMORY_HOME` is
left alone; delete that directory too if you want it gone entirely.

Next: [wire your agents](agents.md).
