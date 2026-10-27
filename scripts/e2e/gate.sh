#!/bin/sh
# The web app's merge gate: run it before merging UI work (and on every merge
# to master). One headless browser against the dev mock (web/dev/mock.py, a
# private port, never the live GodTerm), about two minutes. Exit 1 and a list
# of what is broken when a control is invisible, clipped, covered or off
# screen in any dialog at any of five sizes, or a flow does not work.
#
#   scripts/e2e/gate.sh                      # chromium, the web/ of this checkout
#   scripts/e2e/gate.sh --engine both        # + WebKit
#   scripts/e2e/gate.sh --sizes 390,1440     # some sizes
#   scripts/e2e/gate.sh --only move          # screens/flows with that text in the name
#   scripts/e2e/gate.sh --out DIR            # where the report and screenshots go
#
# Playwright for Node: web/dev/node_modules (a link) or PW_DIR=<node_modules>.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
PORT=${GATE_PORT:-7993}
# Never the live GodTerm's ports; take the next free one.
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
