#!/bin/sh
set -eu
repo="$(cd "$(dirname "$0")/.." && pwd)"
bin="$repo/target/release/godterm"
case "${1:-}" in
  ""|--*) ;;
  *) bin="$1"; shift ;;
esac
if [ ! -x "$bin" ]; then
  echo "building release binary..."
  (cd "$repo" && cargo build --release)
fi
exec "$bin" install "$@"
