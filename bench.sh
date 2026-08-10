#!/usr/bin/env bash
# Reproducible benchmark for the README's numbers.
#
# Runs `fuckmemory bench` twice — with and without the embedding model — and
# prints a markdown table plus an ASCII chart of the medians, annotated with the
# machine it was measured on. Your real store is never touched: bench writes to
# a throwaway database in the system temp dir.
#
# Usage:
#   ./bench.sh                     # use the installed binary, 10k facts
#   BIN=/path/to/fuckmemory ./bench.sh
#   ./bench.sh 10000 30 10         # facts rounds queries-per-round
set -euo pipefail

BIN="${BIN:-$(command -v fuckmemory || echo "$(dirname "$0")/target/release/fuckmemory")}"
FACTS="${1:-10000}"
ROUNDS="${2:-30}"
QUERIES="${3:-10}"

say() { printf '\033[1m%s\033[0m\n' "$*"; }

command -v "$BIN" >/dev/null 2>&1 || { echo "binary not found: $BIN" >&2; exit 1; }

say "benchmarking $BIN — $FACTS facts, $ROUNDS rounds, $QUERIES queries/round"

# Always measure both paths so the README table has semantic on/off side by side.
ON=$(FUCKMEMORY_SEMANTIC=1 "$BIN" bench --facts "$FACTS" --rounds "$ROUNDS" --queries "$QUERIES" 2>&1)
OFF=$(FUCKMEMORY_SEMANTIC=0 "$BIN" bench --facts "$FACTS" --rounds "$ROUNDS" --queries "$QUERIES" 2>&1)

# Pull the medians (µs) out of the output. `recall ` (trailing space) would also
# match `recall hot`, so match the exact label followed by digits.
pick() { echo "$1" | grep -E "^$2[[:space:]]+[0-9]+" | grep -oE '[0-9]+ µs' | grep -oE '[0-9]+'; }
WRITE_ON=$(pick "$ON" write)
WRITE_OFF=$(pick "$OFF" write)
RECALL_ON=$(pick "$ON" 'recall ')
RECALL_OFF=$(pick "$OFF" 'recall ')
DISK_ON=$(pick "$ON" 'recall disk')
HOT_ON=$(pick "$ON" 'recall hot')

echo
say "results (µs, median)"
echo
printf '%-26s %12s %12s\n' "" "semantic on" "semantic off"
printf '%-26s %12s %12s\n' "--------------------------" "------------" "------------"
printf '%-26s %12s %12s\n' "write (per remember)" "$WRITE_ON" "$WRITE_OFF"
printf '%-26s %12s %12s\n' "recall (per query)" "$RECALL_ON" "$RECALL_OFF"
[ -n "$DISK_ON" ] && printf '%-26s %12s\n' "recall, persisted index" "$DISK_ON"
[ -n "$HOT_ON" ] && printf '%-26s %12s\n' "recall, hot cache" "$HOT_ON"

echo
say "markdown table"
echo
cat <<EOF
| metric | semantic on | semantic off |
|---|---|---|
| write (per \`remember\`) | ${WRITE_ON:-—} µs | ${WRITE_OFF:-—} µs |
| recall (per query) | ${RECALL_ON:-—} µs | ${RECALL_OFF:-—} µs |
| recall, persisted index | ${DISK_ON:-—} µs | — |
| recall, hot cache | ${HOT_ON:-—} µs | — |
EOF

echo
say "ascii chart (µs, log-ish bar, semantic on)"
echo
bar() {
  local val=$1 label=$2
  [ -z "$val" ] && return
  # 1 bar per 500 µs, floored at 1, capped at 60.
  local n=$(( val / 500 ))
  [ "$n" -lt 1 ] && n=1
  [ "$n" -gt 60 ] && n=60
  printf '%-26s %8s µs ▕%s\n' "$label" "$val" "$(printf '█%.0s' $(seq 1 "$n"))"
}
bar "$WRITE_ON" "write"
bar "$RECALL_ON" "recall"
bar "$DISK_ON" "recall (disk)"
bar "$HOT_ON" "recall (hot)"

echo
printf 'machine    %s / %s\n' "$(uname -s)" "$(uname -m)"
if command -v nproc >/dev/null 2>&1; then
  printf 'cores      %s\n' "$(nproc)"
fi
if command -v lscpu >/dev/null 2>&1; then
  lscpu 2>/dev/null | grep -E '^Model name' | sed 's/^Model name:\s*/cpu        /'
fi
printf 'date       %s\n' "$(date -u +%Y-%m-%d)"
printf 'binary     %s\n' "$BIN"
