#!/usr/bin/env bash
# Full local release: macOS (universal, arm64, x86_64; signed, notarized),
# Linux (x86_64, aarch64) and Windows (x86_64, built on the Windows machine
# over ssh) artifacts, then SHA256SUMS, all into dist/. Builds from a clean git worktree of REF, so
# uncommitted work in this checkout never ends up in a release.
#
#   scripts/release/all.sh                    REF=HEAD
#   REF=v0.2.0-rc1 scripts/release/all.sh
#   DRAFT=1 REF=v0.2.0-rc1 scripts/release/all.sh   also a draft GitHub release
#   ONLY=macos | ONLY=linux | ONLY=windows    one platform
#   NO_WINDOWS=1                              skip Windows (machine offline)
#
# LINUX_FROM_CI=1: build Linux on GitHub's native runners (build-linux.yml)
# instead of local Docker, and download the artifacts into dist/. Needs REF
# pushed. Use it on Macs whose Docker cannot emulate x86_64.
#
# Signing a Linux checksums file: set MINISIGN_KEY (path to a minisign
# secret key) or GPG_KEY (a key id); without either only SHA256SUMS ships.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
REF="${REF:-HEAD}"
export DIST="${DIST:-$ROOT/dist}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-release-eng}"

sha="$(git -C "$ROOT" rev-parse --verify "$REF^{commit}")"
WT="$CARGO_TARGET_DIR/worktree"
git -C "$ROOT" worktree remove --force "$WT" 2>/dev/null || rm -rf "$WT"
git -C "$ROOT" worktree prune
git -C "$ROOT" worktree add --detach "$WT" "$sha" >/dev/null
trap 'git -C "$ROOT" worktree remove --force "$WT" 2>/dev/null || true' EXIT
echo "==> building $REF ($sha) in $WT"
if [[ "$REF" == v* && ( -n "${DRAFT:-}" || -n "${LINUX_FROM_CI:-}" ) ]]; then
  git -C "$ROOT" push -q origin "refs/tags/$REF"
fi

# KEEP_DIST=1 adds to dist/ (for example ONLY=linux after a macOS run).
[[ -n "${KEEP_DIST:-}" ]] || rm -rf "$DIST"
mkdir -p "$DIST"
linux_from_ci() {
  local before id
  before="$(gh run list --repo daniel-farina/godterm --workflow build-linux.yml --limit 1 --json databaseId --jq '.[0].databaseId // 0')"
  gh workflow run build-linux.yml --repo daniel-farina/godterm -f ref="$sha"
  for _ in $(seq 60); do
    id="$(gh run list --repo daniel-farina/godterm --workflow build-linux.yml --limit 1 --json databaseId --jq '.[0].databaseId // 0')"
    [[ "$id" != "$before" ]] && break
    sleep 5
  done
  [[ "$id" != "$before" ]] || { echo "build-linux.yml did not start" >&2; exit 1; }
  echo "==> waiting for build-linux run $id"
  gh run watch "$id" --repo daniel-farina/godterm --exit-status >/dev/null
  gh run download "$id" --repo daniel-farina/godterm --dir "$DIST/ci"
  find "$DIST/ci" -type f \( -name '*.tar.gz' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' \) -exec mv {} "$DIST/" \;
  rm -rf "$DIST/ci"
}
linux() {
  if [[ -n "${LINUX_FROM_CI:-}" ]]; then linux_from_ci; else "$WT/scripts/release/linux.sh"; fi
}
windows() { "$WT/scripts/release/windows-remote.sh"; }
case "${ONLY:-all}" in
  macos) "$WT/scripts/release/macos.sh" ;;
  linux) linux ;;
  windows) windows ;;
  all)
    # Linux (CI) and Windows (remote) build while macOS builds and notarizes.
    pids=()
    if [[ -n "${LINUX_FROM_CI:-}" ]]; then linux_from_ci > "$DIST/.linux.log" 2>&1 & pids+=($!); fi
    if [[ -z "${NO_WINDOWS:-}" ]]; then windows > "$DIST/.windows.log" 2>&1 & pids+=($!); fi
    "$WT/scripts/release/macos.sh"
    [[ -n "${LINUX_FROM_CI:-}" ]] || linux
    failed=0
    for p in "${pids[@]}"; do wait "$p" || failed=1; done
    tail -n 20 "$DIST"/.linux.log "$DIST"/.windows.log 2>/dev/null || true
    rm -f "$DIST"/.linux.log "$DIST"/.windows.log
    [[ $failed == 0 ]] || { echo "a Linux or Windows build failed (logs above)" >&2; exit 1; }
    ;;
esac
"$WT/scripts/release/checksums.sh"

if [[ -n "${MINISIGN_KEY:-}" ]]; then
  minisign -S -s "$MINISIGN_KEY" -m "$DIST/SHA256SUMS"
elif [[ -n "${GPG_KEY:-}" ]]; then
  gpg --batch --yes --local-user "$GPG_KEY" --armor --detach-sign -o "$DIST/SHA256SUMS.asc" "$DIST/SHA256SUMS"
fi

if [[ -n "${DRAFT:-}" ]]; then
  tag="$REF"
  [[ "$tag" == v* ]] || { echo "DRAFT needs REF to be a v* tag" >&2; exit 1; }
  notes="$(mktemp)"
  "$WT/scripts/release/notes.sh" "${tag#v}" > "$notes"
  pre=(); [[ "$tag" == *-* ]] && pre=(--prerelease)
  # Create the draft with notes only, then upload each asset on its own
  # with retries: large multi file uploads tend to stall.
  gh release create "$tag" --repo daniel-farina/godterm --draft "${pre[@]}" --verify-tag \
    --title "GodTerm ${tag#v}" --notes-file "$notes"
  for f in "$DIST"/*.dmg "$DIST"/*.zip "$DIST"/*.tar.gz "$DIST"/*.deb "$DIST"/*.rpm \
           "$DIST"/*.AppImage "$DIST"/*.exe "$DIST"/SHA256SUMS*; do
    [[ -f "$f" ]] || continue
    for try in 1 2 3; do
      gh release upload "$tag" --repo daniel-farina/godterm --clobber "$f" && break
      echo "upload of $(basename "$f") failed (try $try)" >&2; sleep 10
    done
  done
  gh release view "$tag" --repo daniel-farina/godterm --json url,assets \
    --jq '.url, (.assets[] | "  \(.name)  \(.size)")'
  rm -f "$notes"
fi
ls -lh "$DIST"
