#!/usr/bin/env bash
# Build fuckmemory and wire it into every agent on this machine.
#
# Usage:
#   ./install.sh                 build, install to ~/.local/bin, register agents
#                                (and delete any ~/.cargo/bin copy that shadows it)
#   ./install.sh --dry-run       build, then show what would change
#   ./install.sh --no-model      skip the 125 MB embedding model
#   PREFIX=/usr/local ./install.sh
set -euo pipefail

PREFIX="${PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
PASSTHROUGH=("$@")

say() { printf '\033[1m%s\033[0m\n' "$*"; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs"

cd "$(dirname "$0")"

say "Building (release, this takes a couple of minutes with LTO)…"
cargo build --release

say "Installing to $BINDIR"
mkdir -p "$BINDIR"
# Copy via a temp name and rename, so an upgrade cannot break a running server.
install -m 755 target/release/fuckmemory "$BINDIR/.fuckmemory.new"
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
"$BINDIR/fuckmemory" install --command "$BINDIR/fuckmemory" "${PASSTHROUGH[@]}"

echo
say "Done. Check it with:  fuckmemory doctor"
