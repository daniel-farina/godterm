#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
REF="${REF:-HEAD}"
export DIST="${DIST:-$ROOT/dist/release}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-release-eng}"

sha="$(git -C "$ROOT" rev-parse --verify "$REF^{commit}")"
WT="$CARGO_TARGET_DIR/worktree"
git -C "$ROOT" worktree remove --force "$WT" 2>/dev/null || rm -rf "$WT"
git -C "$ROOT" worktree prune
git -C "$ROOT" worktree add --detach "$WT" "$sha" >/dev/null
trap 'git -C "$ROOT" worktree remove --force "$WT" 2>/dev/null || true' EXIT
echo "==> building $REF ($sha) in $WT"
if [[ "$REF" == v* && ( -n "${DRAFT:-}" || -n "${LINUX_FROM_CI:-}" || -n "${WINDOWS_FROM_CI:-}" ) ]]; then
  git -C "$ROOT" push -q origin "refs/tags/$REF"
fi

if [[ -z "${KEEP_DIST:-}" && -d "$DIST" ]]; then
  rm -rf "$DIST/stage" "$DIST/ci" "$DIST/ci-linux" "$DIST/ci-windows"
  find "$DIST" -maxdepth 1 -type f \( -name '*.dmg' -o -name '*.zip' -o -name '*.tar.gz' \
    -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' -o -name '*.exe' \
    -o -name 'SHA256SUMS*' -o -name '.linux.log' -o -name '.windows.log' \) -delete
fi
mkdir -p "$DIST"
from_ci() {
  local wf="$1" tag="$2" before id
  shift 2
  before="$(gh run list --repo daniel-farina/godterm --workflow "$wf" --limit 1 --json databaseId --jq '.[0].databaseId // 0')"
  gh workflow run "$wf" --repo daniel-farina/godterm -f ref="$sha"
  for _ in $(seq 60); do
    id="$(gh run list --repo daniel-farina/godterm --workflow "$wf" --limit 1 --json databaseId --jq '.[0].databaseId // 0')"
    [[ "$id" != "$before" ]] && break
    sleep 5
  done
  [[ "$id" != "$before" ]] || { echo "$wf did not start" >&2; exit 1; }
  echo "==> waiting for $wf run $id"
  gh run watch "$id" --repo daniel-farina/godterm --exit-status >/dev/null
  gh run download "$id" --repo daniel-farina/godterm --dir "$DIST/ci-$tag"
  find "$DIST/ci-$tag" -type f \( "$@" \) -exec mv {} "$DIST/" \;
  rm -rf "$DIST/ci-$tag"
}
linux_from_ci() {
  from_ci build-linux.yml linux -name '*.tar.gz' -o -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage'
}
windows_from_ci() {
  from_ci build-windows.yml windows -name '*-pc-windows-msvc.zip' -o -name 'GodTerm-*-windows-x64-setup.exe'
}
linux() {
  if [[ -n "${LINUX_FROM_CI:-}" ]]; then linux_from_ci; else "$WT/scripts/release/linux.sh"; fi
}
windows() {
  if [[ -n "${WINDOWS_FROM_CI:-}" ]]; then windows_from_ci; else "$WT/scripts/release/windows-remote.sh"; fi
}
case "${ONLY:-all}" in
  macos) "$WT/scripts/release/macos.sh" ;;
  linux) linux ;;
  windows) windows ;;
  all)
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
"$WT/scripts/release/aliases.sh"
"$WT/scripts/release/checksums.sh"

UPDATE_KEY="${UPDATE_KEY:-$HOME/.config/godterm-release/minisign.key}"
if [[ -f "$UPDATE_KEY" ]]; then
  SIGN_PY=python3
  if ! "$SIGN_PY" -c 'import cryptography' 2>/dev/null; then
    SIGN_VENV="${SIGN_VENV:-$HOME/.cache/godterm-release/venv}"
    if [[ ! -x "$SIGN_VENV/bin/python" ]] || ! "$SIGN_VENV/bin/python" -c 'import cryptography' 2>/dev/null; then
      echo "==> installing the python 'cryptography' package into $SIGN_VENV for signing"
      python3 -m venv "$SIGN_VENV" && "$SIGN_VENV/bin/pip" install --quiet cryptography \
        || { echo "cannot set up python cryptography for minisign.py (pip install cryptography)" >&2; exit 1; }
    fi
    SIGN_PY="$SIGN_VENV/bin/python"
  fi
  REL_VERSION="$(sed -nE 's/^version = "([^"]+)"/\1/p' "$WT/Cargo.toml" | head -1)"
  [[ -n "$REL_VERSION" ]] || { echo "no version in $WT/Cargo.toml" >&2; exit 1; }
  "$SIGN_PY" "$WT/scripts/update-signing/minisign.py" sign "$UPDATE_KEY" "$DIST/SHA256SUMS" "$REL_VERSION" >/dev/null
  grep -q "version:$REL_VERSION" "$DIST/SHA256SUMS.minisig" || { echo "SHA256SUMS.minisig lacks the signed version" >&2; exit 1; }
  pub="$(sed -nE 's/^pub const UPDATE_PUBKEY: &str = "([^"]+)";/\1/p' "$WT/src/update.rs" 2>/dev/null || true)"
  if [[ -n "$pub" ]] && command -v minisign >/dev/null; then
    minisign -Vm "$DIST/SHA256SUMS" -P "$pub" -q || { echo "SHA256SUMS.minisig does not verify" >&2; exit 1; }
    echo "==> SHA256SUMS signed and verified with the update key"
  fi
elif [[ -n "${GPG_KEY:-}" ]]; then
  gpg --batch --yes --local-user "$GPG_KEY" --armor --detach-sign -o "$DIST/SHA256SUMS.asc" "$DIST/SHA256SUMS"
else
  echo "warning: no update signing key ($UPDATE_KEY): releases without SHA256SUMS.minisig are refused by the self updater" >&2
fi

if [[ -n "${DRAFT:-}" ]]; then
  tag="$REF"
  [[ "$tag" == v* ]] || { echo "DRAFT needs REF to be a v* tag" >&2; exit 1; }
  notes="$(mktemp)"
  "$WT/scripts/release/notes.sh" "${tag#v}" > "$notes"
  pre=(); [[ "$tag" == *-* ]] && pre=(--prerelease)
  gh release create "$tag" --repo daniel-farina/godterm --draft ${pre[@]+"${pre[@]}"} --verify-tag \
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
