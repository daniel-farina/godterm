#!/usr/bin/env bash
# Print the CHANGELOG.md section for a version (0.2.0, or 0.2.0-rc1 which
# falls back to 0.2.0), plus install and verification notes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
v="${1:?usage: notes.sh <version>}"
section() {
  awk -v v="$1" '
    $0 ~ "^## \\[" v "\\]" { on = 1; next }
    on && /^## \[/ { exit }
    on && /^\[[^]]+\]: / { exit }
    on { print }
  ' "$ROOT/CHANGELOG.md"
}
body="$(section "$v")"
[[ -z "$body" ]] && body="$(section "${v%%-*}")"
[[ -n "$body" ]] || { echo "no CHANGELOG.md section for $v" >&2; exit 1; }
printf '%s\n' "$body"
cat <<'EOT'

## Install

- macOS: open the DMG and drag GodTerm to Applications (signed with Developer ID and notarized), or `brew install --cask daniel-farina/godterm/godterm`. CLI only: `brew install daniel-farina/godterm/godterm`.
- Debian and Ubuntu: `sudo apt install ./godterm_<version>_<arch>.deb`
- Fedora, RHEL, openSUSE: `sudo rpm -i godterm-<version>-1.<arch>.rpm`
- Any Linux: extract the tar.gz and put `godterm` on your PATH.

Verify downloads with `shasum -a 256 -c SHA256SUMS --ignore-missing`.
EOT
