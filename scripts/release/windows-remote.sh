#!/usr/bin/env bash
# Build the Windows artifacts on a Windows build machine over ssh, then copy
# them into dist/.
# Copies this tree (no .git; the commit is passed in), runs
# scripts/release/windows.ps1 there, and pulls the zip and installer back.
#
#   scripts/release/windows-remote.sh            x64 zip + installer
#   WIN_ARCHES="x64 arm64" scripts/release/windows-remote.sh
#
# Env (or ~/.config/godterm/release.env): WIN_HOST, WIN_USER, WIN_SSH_KEY.
# The machine needs OpenSSH server, Rust (MSVC) and Inno Setup 6. ssh runs
# with -F /dev/null so a local ~/.ssh/config cannot redirect the port.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

[[ -n "${WIN_HOST:-}" && -n "${WIN_USER:-}" ]] || die "set WIN_HOST and WIN_USER (the Windows build machine) in ~/.config/godterm/release.env"
WIN_SSH_KEY="${WIN_SSH_KEY:-$HOME/.ssh/id_ed25519}"
read -r -a ARCHES <<< "${WIN_ARCHES:-x64}"
SSH=(ssh -p 22 -F /dev/null -i "$WIN_SSH_KEY" -o BatchMode=yes -o ConnectTimeout=20 -o ServerAliveInterval=30 -o StrictHostKeyChecking=accept-new "$WIN_USER@$WIN_HOST")
SCP=(scp -P 22 -F /dev/null -i "$WIN_SSH_KEY" -o BatchMode=yes -o ConnectTimeout=20)
REMOTE='C:\Users\'"$WIN_USER"'\code\godterm'
REMOTE_TARGET='C:\Users\'"$WIN_USER"'\code\godterm-target'

# PowerShell from stdin, as -EncodedCommand (quoting through ssh mangles).
winps() {
  local b64
  b64="$(iconv -t UTF-16LE | base64)"
  "${SSH[@]}" "powershell -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand $b64" \
    2> >(grep -v -iE 'post-quantum|store now|openssh.com/pq|CLIXML|<Objs' >&2)
}

log "probe $WIN_USER@$WIN_HOST"
echo 'hostname' | winps >/dev/null || die "cannot reach the Windows host $WIN_HOST over ssh"

log "copy the source tree"
tar -C "$ROOT" --exclude ./target --exclude ./dist --exclude './target-*' --exclude ./.git \
  --exclude ./assets/godterm-icon -czf - . \
  | "${SSH[@]}" "powershell -NoProfile -Command \"Remove-Item -Recurse -Force '$REMOTE' -ErrorAction SilentlyContinue; New-Item -ItemType Directory -Force -Path '$REMOTE' | Out-Null; tar -xzf - -C '$REMOTE'\"" \
    2> >(grep -v -iE 'post-quantum|store now|openssh.com/pq' >&2)

for arch in "${ARCHES[@]}"; do
  log "build on Windows ($arch)"
  out="$(winps <<EOF
\$ErrorActionPreference = 'Continue'
\$env:CARGO_TARGET_DIR = '$REMOTE_TARGET'
\$env:GODTERM_GIT_SHA = '$GODTERM_GIT_SHA'
\$env:SOURCE_DATE_EPOCH = '$SOURCE_DATE_EPOCH'
\$env:DIST = '$REMOTE\dist'
Set-Location '$REMOTE'
& .\scripts\release\windows.ps1 -Arch $arch -Profile $PROFILE 2>&1 | ForEach-Object { "\$_" }
"free {0:N1} GB" -f ((Get-PSDrive C).Free/1GB)
EOF
)" || true
  printf '%s\n' "$out" | grep -vE '^\s+(Compiling|Downloaded|Downloading)' | tail -25
  grep -q 'BUILD_RESULT:0' <<< "$out" || die "Windows build ($arch) failed"
done

log "pull artifacts"
"${SCP[@]}" "$WIN_USER@$WIN_HOST:C:/Users/$WIN_USER/code/godterm/dist/godterm-$VERSION-*-pc-windows-msvc.zip" "$DIST/"
"${SCP[@]}" "$WIN_USER@$WIN_HOST:C:/Users/$WIN_USER/code/godterm/dist/GodTerm-$VERSION-windows-x64-setup.exe" "$DIST/" 2>/dev/null || true
ls -lh "$DIST"/*windows*
log "Windows done"
