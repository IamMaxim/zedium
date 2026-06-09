#!/usr/bin/env bash
# GAP-8 runtime air-gap smoke.
#
# Launches the built Zedium binary under strace, lets it boot and open a file,
# then quits and reports every connect() to a non-local destination. The static
# verify.sh gate cannot prove zero boot-time egress; this can. Run it after every
# upstream bump (see docs/MAINTAINING.md GAP-8).
#
# Usage: tools/airgap-smoke.sh [BINARY] [DWELL_SECONDS]
#   BINARY        defaults to the debug zedium, then the release zedium.
#   DWELL_SECONDS how long to let the app run before quitting (default 30).
#
# PASS = no non-local connect(). Localhost (127.0.0.1/::1), AF_UNIX, and the
# user-opt-in local LLM ports (ollama 11434, LM Studio 1234) are allowed.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${1:-}"
if [[ -z "$BIN" ]]; then
  for cand in zed/target/debug/zedium zed/target/release/zedium \
              zed/target/debug/zed zed/target/release/zed; do
    [[ -x "$cand" ]] && BIN="$cand" && break
  done
fi
[[ -x "$BIN" ]] || { echo "no zedium binary found (build first: just build)"; exit 2; }
DWELL="${2:-30}"

TRACE="$(mktemp /tmp/zedium-airgap.XXXXXX.strace)"
WORKFILE="$(mktemp /tmp/zedium-smoke.XXXXXX.txt)"
echo "air-gap smoke marker" >"$WORKFILE"

echo ">> binary : $BIN"
echo ">> trace  : $TRACE"
echo ">> dwell  : ${DWELL}s"

# strace the whole process tree; -f follows forks/threads (the network workers
# live on spawned threads). `timeout` time-boxes the whole launch and sends TERM
# after the dwell, then KILL if it lingers — no separate foreground sleep (which
# some sandboxes kill, aborting the script before analysis).
timeout -s TERM -k 5 "${DWELL}s" \
  strace -f -e trace=connect,network -o "$TRACE" "$BIN" "$WORKFILE" >/dev/null 2>&1 || true
pkill -KILL -f "$(basename "$BIN")" 2>/dev/null || true

echo
echo "=== connect() calls to NON-LOCAL destinations (should be empty) ==="
# AF_INET/AF_INET6 connects only; drop loopback, AF_UNIX, and local LLM ports.
LEAKS="$(grep -E 'connect\(' "$TRACE" 2>/dev/null \
  | grep -E 'AF_INET' \
  | grep -vE '127\.0\.0\.1|::1|inet6 ::1|sin_port=htons\((11434|1234)\)' \
  || true)"
if [[ -n "$LEAKS" ]]; then
  echo "$LEAKS"
  echo
  echo "RESULT: FAIL — non-local egress observed at boot. Trace: $TRACE"
  exit 1
fi
echo "(none)"
echo
echo "=== summary of ALL connect targets seen (for the record) ==="
grep -E 'connect\(' "$TRACE" 2>/dev/null | grep -oE 'AF_[A-Z]+|sa_family=AF_[A-Z]+' | sort | uniq -c || true
echo
echo "RESULT: PASS — no non-local boot-time egress. Trace: $TRACE"
