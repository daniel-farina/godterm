#!/bin/sh
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
PORT=${GATE_PORT:-7993}
while [ "$PORT" = 7878 ] || [ "$PORT" = 7880 ] || lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; do PORT=$((PORT + 1)); done
LOG=$(mktemp -t gate-mock)
python3 "$REPO/web/dev/mock.py" --port "$PORT" >"$LOG" 2>&1 &
MOCK=$!
trap 'kill $MOCK 2>/dev/null || true; rm -f "$LOG"' EXIT INT TERM
i=0
until curl -fs "http://127.0.0.1:$PORT/" >/dev/null 2>&1; do
  i=$((i + 1))
  [ $i -gt 50 ] && { echo "gate: the mock did not start:"; cat "$LOG"; exit 2; }
  sleep 0.2
done
node "$HERE/web_gate.mjs" --url "http://127.0.0.1:$PORT/" "$@"
