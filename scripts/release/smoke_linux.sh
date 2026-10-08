#!/usr/bin/env bash
# Smoke test a GodTerm Linux package ON a Linux host (a VM, a droplet, a
# container). Installs it, runs the TUI under tmux against a fake claude in
# a throwaway home, opens a tab, quits, and checks the exit code.
#
#   smoke_linux.sh deb      ./godterm_0.2.0_amd64.deb
#   smoke_linux.sh rpm      ./godterm-0.2.0-1.x86_64.rpm
#   smoke_linux.sh appimage ./GodTerm-0.2.0-x86_64.AppImage
#   smoke_linux.sh tar      ./godterm-0.2.0-x86_64-unknown-linux-gnu.tar.gz
#
# Needs root for deb/rpm (apt/dnf), and tmux (installed if missing).
set -euo pipefail
kind="$1"; pkg="$(readlink -f "$2")"
pass=0; fail=0
ok() { echo "  ok   $*"; pass=$((pass + 1)); }
bad() { echo "  FAIL $*"; fail=$((fail + 1)); }

have() { command -v "$1" >/dev/null; }
if ! have tmux; then
  if have apt-get; then apt-get update -qq >/dev/null && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq tmux >/dev/null
  elif have dnf; then dnf install -y -q tmux >/dev/null; fi
fi

case "$kind" in
  deb) DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$pkg" >/dev/null; BIN=godterm ;;
  rpm) dnf install -y -q "$pkg" >/dev/null; BIN=godterm ;;
  appimage) chmod +x "$pkg"; BIN="$pkg --appimage-extract-and-run" ;;
  tar) d="$(mktemp -d)"; tar -xzf "$pkg" -C "$d"; BIN="$(ls "$d"/*/godterm)" ;;
  *) echo "unknown kind $kind" >&2; exit 2 ;;
esac
echo "==> $kind: $($BIN --version)"
$BIN --version | grep -q "^godterm " && ok "--version" || bad "--version"

H="$(mktemp -d)/home"
mkdir -p "$H/work" "$H/tabs" "$H/accounts/one" "$H/accounts/two"
printf '#!/bin/sh\necho "fake claude in $(basename "$PWD") args:$*"\necho "? for shortcuts"\nexec cat\n' > "$H/fake-claude"
chmod 755 "$H/fake-claude"
creds='{"claudeAiOauth":{"accessToken":"fake","refreshToken":"fake","expiresAt":4102444800000,"subscriptionType":"max"}}'
echo "$creds" > "$H/accounts/one/.credentials.json"; echo "$creds" > "$H/accounts/two/.credentials.json"
cat > "$H/config.toml" <<EOF
claude_bin = "$H/fake-claude"
notifications = false
setup_dont_show = true
new_tab_base = "$H/tabs"
[[account]]
name = "one"
label = "Alpha"
cwd = "$H/work"
[[account]]
name = "two"
label = "Bravo"
cwd = "$H/work"
[voice]
tts = false
chime = false
wake_model = "off"
EOF
echo 1 > "$H/tour_done"

S=godterm-smoke
tmux kill-session -t $S 2>/dev/null || true
tmux new-session -d -s $S -x 140 -y 40 \
  "GODTERM_HOME='$H' GODTERM_NO_AUDIO=1 GODTERM_NO_MIC=1 GODTERM_NO_OPEN=1 TERM=xterm-256color $BIN; echo GODTERM_EXIT=\$?; sleep 30"
screen() { tmux capture-pane -p -t $S; }
wait_for() { local t=0; while (( t < $2 * 4 )); do screen | grep -qF -- "$1" && return 0; sleep 0.25; t=$((t + 1)); done; return 1; }

wait_for "Alpha" 20 && ok "TUI renders account Alpha" || bad "TUI renders account Alpha"
wait_for "Bravo" 5 && ok "TUI renders account Bravo" || bad "TUI renders account Bravo"
wait_for "fake claude in work" 15 && ok "fake claude runs in a pane" || bad "fake claude runs in a pane"
tmux send-keys -t $S C-a t
wait_for "New tab for Alpha" 10 && ok "New Tab dialog opens" || bad "New Tab dialog opens"
tmux send-keys -t $S Enter
wait_for "[2/2]" 15 && ok "second tab opens" || bad "second tab opens"
screen | head -12
tmux send-keys -t $S C-a Q
wait_for "GODTERM_EXIT=0" 20 && ok "quits cleanly (exit 0)" || { bad "quits cleanly"; screen | tail -5; }
tmux kill-session -t $S 2>/dev/null || true
rm -rf "$(dirname "$H")"
echo "==> $kind: $pass/$((pass + fail)) checks passed"
(( fail == 0 ))
