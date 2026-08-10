#!/usr/bin/env bash
# Install fuckmemory and wire it into every agent on this machine.
#
# Works two ways:
#
#   1. One-liner (no Rust toolchain, no clone):
#        curl -fsSL https://raw.githubusercontent.com/dalsori/fuckmemory/master/install.sh | sh
#      Downloads the newest prebuilt binary for your platform and installs it to
#      ~/.local/bin. Add --no-autosave to skip the autosave hooks.
#
#   2. From a repo checkout:
#        ./install.sh                 # build + install to ~/.local/bin + register agents
#        ./install.sh --dry-run       # build, then show what would change
#        ./install.sh --no-model      # skip the 125 MB embedding model
#      PREFIX=/usr/local ./install.sh
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --autosave is the default so the one-liner gives you shared memory out of the
# box; --no-autosave opts out. Everything else is passed through to `install`.
AUTOSAVE="--autosave"
PASSTHROUGH=()
for arg in "$@"; do
  if [ "$arg" = "--no-autosave" ]; then
    AUTOSAVE=""
  else
    PASSTHROUGH+=("$arg")
  fi
done

# Map the platform to the release asset name, in lockstep with
# .github/workflows/release.yml.
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)              TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  Darwin-x86_64)             TARGET=x86_64-apple-darwin ;;
  Darwin-arm64)              TARGET=aarch64-apple-darwin ;;
  *) die "unsupported platform: $(uname -s)-$(uname -m)" ;;
esac

# A repo checkout has Cargo.toml in the current directory; `curl | sh` does not.
# Build from source when we can, otherwise fetch the prebuilt binary.
if [ -f Cargo.toml ] && command -v cargo >/dev/null 2>&1; then
  say "Building (release, this takes a couple of minutes with LTO)…"
  cargo build --release
  BIN="target/release/fuckmemory"
else
  say "Downloading fuckmemory ($TARGET)…"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  # Try the modern per-target asset first; older releases used a flat
  # `fuckmemory-<os>-<arch>.gz` name, so fall back to it.
  if ! curl -fL -o "$tmp/fuckmemory.tar.gz" \
    "https://github.com/dalsori/fuckmemory/releases/latest/download/fuckmemory-$TARGET.tar.gz"; then
    case "$(uname -s)-$(uname -m)" in
      Linux-x86_64)              LEGACY="fuckmemory-linux-x86_64.gz" ;;
      Linux-aarch64|Linux-arm64) LEGACY="fuckmemory-linux-aarch64.gz" ;;
      Darwin-x86_64)             LEGACY="fuckmemory-darwin-x86_64.gz" ;;
      Darwin-arm64)              LEGACY="fuckmemory-darwin-arm64.gz" ;;
    esac
    curl -fL -o "$tmp/fuckmemory.gz" \
      "https://github.com/dalsori/fuckmemory/releases/latest/download/$LEGACY"
    gunzip -f "$tmp/fuckmemory.gz"
    BIN="$tmp/fuckmemory"
  else
    tar -xzf "$tmp/fuckmemory.tar.gz" -C "$tmp"
    BIN="$tmp/fuckmemory"
  fi
fi

say "Installing to $BINDIR"
mkdir -p "$BINDIR"
# Copy via a temp name and rename, so an upgrade cannot break a running server.
install -m 755 "$BIN" "$BINDIR/.fuckmemory.new"
mv -f "$BINDIR/.fuckmemory.new" "$BINDIR/fuckmemory"

# `cargo install --path .` leaves a second copy in ~/.cargo/bin. Whichever one
# comes first on PATH wins, so the two drift apart silently and you end up
# debugging a binary you did not just build. This script owns the install.
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin/fuckmemory"
if { [ -e "$CARGO_BIN" ] || [ -L "$CARGO_BIN" ]; } && [ "$CARGO_BIN" != "$BINDIR/fuckmemory" ]; then
  case " ${PASSTHROUGH[*]:-} " in
    *" --dry-run "*)
      printf '\033[33mnote:\033[0m would remove the shadowing copy at %s\n' "$CARGO_BIN" ;;
    *)
      rm -f "$CARGO_BIN"
      printf '\033[33mnote:\033[0m removed the shadowing copy at %s\n' "$CARGO_BIN" ;;
  esac
fi

case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) printf '\033[33mnote:\033[0m %s is not on your PATH. Add it:\n  export PATH="%s:$PATH"\n' "$BINDIR" "$BINDIR" ;;
esac

say "Registering with your agents"
# Absolute path, not the bare name: agents launched from a GUI often have a
# different PATH than your shell.
"$BINDIR/fuckmemory" install --command "$BINDIR/fuckmemory" $AUTOSAVE "${PASSTHROUGH[@]}"

echo
say "Done. Check it with:  fuckmemory doctor"
