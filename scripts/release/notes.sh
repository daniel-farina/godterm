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
# A release whose notes say it has no Windows build must not read like it has one.
# Only when no Windows asset sits in DIST (all.sh exports it): a later
# WINDOWS_FROM_CI build of the same version must not be called missing.
have_windows=
for f in "${DIST:-$ROOT/dist/release}"/*windows*; do [[ -f "$f" ]] && have_windows=1; done
if [[ -z "$have_windows" ]] && grep -q 'no Windows build' <<< "$body"; then
  printf '\nWindows: this release has no Windows installer or zip. The last Windows build is v0.2.9.\n'
fi
