# Shared settings for the release scripts. Sourced, not run.
# shellcheck shell=bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

VERSION="$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -1)"
# Inside the Linux build container there is no usable .git; the caller
# passes these in.
GIT_SHA="${GODTERM_GIT_SHA:-$(git rev-parse --short=9 HEAD 2>/dev/null || echo unknown)}"
BUILD_NUMBER="${BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"
export GODTERM_GIT_SHA="$GIT_SHA"
# Reproducible build date: the commit time, not the wall clock.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
export SOURCE_DATE_EPOCH

# A target dir of our own, so release builds never replace target/release/godterm
# (which `godterm install` points the user's app at).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-release-eng}"
DIST="${DIST:-$ROOT/dist}"
STAGE="$DIST/stage"
PROFILE="${PROFILE:-dist}"
mkdir -p "$DIST" "$STAGE"

# Machine specific settings (signing identity, the Windows build host) live
# outside the repo, in ~/.config/godterm/release.env (KEY=value lines):
#   SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
#   WIN_HOST=<windows build machine>  WIN_USER=<user>  WIN_SSH_KEY=<key path>
RELEASE_ENV="${RELEASE_ENV:-$HOME/.config/godterm/release.env}"
if [[ -f "$RELEASE_ENV" ]]; then
  # shellcheck disable=SC1090
  set -a; . "$RELEASE_ENV"; set +a
fi

log() { printf '\033[1m==> %s\033[0m\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# Parallel jobs: free cores minus headroom, and fewer when RAM is short
# (another build may be running on this machine).
njobs() {
  if [[ -n "${JOBS:-}" ]]; then echo "$JOBS"; return; fi
  local ncpu idle free_gb j
  if [[ "$(uname)" == Darwin ]]; then
    ncpu="$(sysctl -n hw.ncpu)"
    idle="$(top -l 1 -n 0 | sed -nE 's/.* ([0-9.]+)% idle.*/\1/p' | cut -d. -f1)"
    free_gb="$(top -l 1 -n 0 | sed -nE 's/.*, ([0-9]+)([GM]) unused.*/\1 \2/p' | awk '{print ($2=="G")?$1:0}')"
  else
    ncpu="$(nproc)"; idle=80
    free_gb="$(awk '/MemAvailable/ {print int($2/1048576)}' /proc/meminfo)"
  fi
  j=$(( ncpu * ${idle:-50} / 100 - 2 ))
  (( free_gb < 8 )) && j=$(( j < 4 ? j : 4 ))
  (( j < 2 )) && j=2
  echo "$j"
}

# notarytool credentials as an argument list, never printed. In order:
#   NOTARY_PROFILE      a `xcrun notarytool store-credentials` keychain profile
#   APPLE_API_KEY (path to AuthKey_XXXX.p8), APPLE_API_KEY_ID, APPLE_API_ISSUER
#   the same three in ~/.config/godterm/notary.env (KEY=value lines)
notary_args() {
  if [[ -n "${NOTARY_PROFILE:-}" ]]; then
    NOTARY_AUTH=(--keychain-profile "$NOTARY_PROFILE"); return 0
  fi
  local f="${NOTARY_ENV:-$HOME/.config/godterm/notary.env}"
  if [[ -z "${APPLE_API_KEY:-}" && -f "$f" ]]; then
    # shellcheck disable=SC1090
    set -a; . "$f"; set +a
  fi
  if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_KEY_ID:-}" && -n "${APPLE_API_ISSUER:-}" ]]; then
    [[ -f "$APPLE_API_KEY" ]] || die "APPLE_API_KEY does not point at a .p8 file"
    NOTARY_AUTH=(--key "$APPLE_API_KEY" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER"); return 0
  fi
  return 1
}

sha256() { if command -v shasum >/dev/null; then shasum -a 256 "$@"; else sha256sum "$@"; fi; }
