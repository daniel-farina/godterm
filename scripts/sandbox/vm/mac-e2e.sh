#!/usr/bin/env bash
# GodTerm clean-machine test on a fresh macOS VM (tart).
#
# Clones a cirruslabs macOS base image into godterm-e2e-mac-<timestamp>,
# boots it headless, copies the release artifacts in, installs them the way
# a new user would, drives godterm in tmux inside the VM, saves the screens
# to the host and deletes the VM (unless --keep).
#
# Usage: scripts/sandbox/vm/mac-e2e.sh [options]
#   --artifacts DIR   release artifacts (default dist/release of the main
#                     checkout, or $GODTERM_ARTIFACTS); needs the macOS arm64
#                     zip, optionally the arm64 DMG, the universal zip and
#                     SHA256SUMS. A fresh local build works too: a dir with a
#                     godterm-*-macos-arm64.zip from scripts/release.
#   --version V       expected version (default: from the zip name)
#   --only LIST       scenarios to run, comma separated: 1..6 (default all)
#                     1 install (zip, DMG, Gatekeeper)  2 first launch
#                     3 setup wizard  4 voice onboarding  5 demo  6 Homebrew
#   --brew MODE       tap | local | both | none (default both; tap is
#                     skipped when the tap is not reachable from the VM)
#   --results DIR     where to write (default e2e-results/vm-<ts>/mac)
#   --base IMAGE      tart image (default ghcr.io/cirruslabs/macos-sonoma-base:latest)
#   --cpu N --memory MB   VM size (default 4 / 8192, the maximum allowed)
#   --keep            leave the VM running at the end (prints how to delete)
#
# Exit code: 0 when no step failed, 1 otherwise.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
# shellcheck source=lib.sh
source "$HERE/lib.sh"
# shellcheck source=tui.sh
source "$HERE/tui.sh"

TS=$(date +%Y%m%d-%H%M%S)
ARTIFACTS="${GODTERM_ARTIFACTS:-$REPO/dist/release}"
EXPECT_VERSION=""
ONLY="1,2,3,4,5,6"
BREW_MODE="both"
RESULTS_DIR=""
BASE="ghcr.io/cirruslabs/macos-sonoma-base:latest"
CPU=4
MEM=8192

while [ $# -gt 0 ]; do
  case "$1" in
    --artifacts) ARTIFACTS="$2"; shift 2 ;;
    --version) EXPECT_VERSION="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --brew) BREW_MODE="$2"; shift 2 ;;
    --results) RESULTS_DIR="$2"; shift 2 ;;
    --base) BASE="$2"; shift 2 ;;
    --cpu) CPU="$2"; shift 2 ;;
    --memory) MEM="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ "$CPU" -le 4 ] || { echo "at most 4 CPUs for a macOS VM" >&2; exit 2; }
[ "$MEM" -le 8192 ] || { echo "at most 8192 MB for a macOS VM" >&2; exit 2; }

RESULTS_DIR="${RESULTS_DIR:-$REPO/e2e-results/vm-$TS/mac}"
results_init "$RESULTS_DIR"
VM_LABEL="macOS ($BASE), $CPU CPU, $MEM MB"
T0=$(date +%s)

want() { case ",$ONLY," in *",$1,"*) return 0 ;; esac; return 1; }

# --- artifacts on the host ---------------------------------------------------
pick() { # pattern...: first match in $ARTIFACTS
  local p f
  for p in "$@"; do
    for f in "$ARTIFACTS"/$p; do [ -f "$f" ] && { echo "$f"; return 0; }; done
  done
  return 1
}
ZIP=$(pick 'godterm-[0-9]*-macos-arm64.zip' 'godterm-macos-arm64.zip') || die "no macOS arm64 zip in $ARTIFACTS"
DMG=$(pick 'GodTerm-[0-9]*-macos-arm64.dmg' 'GodTerm-macos-arm64.dmg' || true)
UZIP=$(pick 'godterm-[0-9]*-macos-universal.zip' 'godterm-macos-universal.zip' || true)
SUMS=$(pick SHA256SUMS || true)
if [ -z "$EXPECT_VERSION" ]; then
  EXPECT_VERSION=$(basename "$ZIP" | sed -nE 's/^godterm-([0-9][^-]*)-macos.*/\1/p')
  [ -n "$EXPECT_VERSION" ] || EXPECT_VERSION=$(sed -nE 's/^version = "(.*)"/\1/p' "$REPO/Cargo.toml" | head -1)
fi
log "results: $RESULTS"
log "artifacts: $ARTIFACTS (zip $(basename "$ZIP"), dmg ${DMG:+$(basename "$DMG")}, expect $EXPECT_VERSION)"

# --- VM ------------------------------------------------------------------------
cleanup() {
  local rc=$?
  vm_teardown
  write_report "GodTerm VM e2e: macOS" "$T0"
  log "remaining godterm-e2e VMs: $(tart list | awk '{print $2}' | grep -c '^godterm-e2e-' || true)"
  exit "$rc"
}
trap cleanup EXIT
trap 'log "interrupted"; exit 130' INT TERM

check_resources "$MEM"
vm_create "$BASE" "godterm-e2e-mac-$TS" "$CPU" "$MEM"
vm_boot

RA=/Users/admin/e2e-artifacts
vrun 20 <<<"mkdir -p $RA" || die "mkdir in VM failed"
FILES=("$ZIP")
for f in "$DMG" "$UZIP" "$SUMS"; do [ -n "$f" ] && FILES+=("$f"); done
# The repo's Homebrew formula and cask (for the local formula test).
FILES+=("$REPO/packaging/homebrew/Formula/godterm.rb")
log "copy ${#FILES[@]} files into the VM"
vcopy_to "${FILES[@]}" "$RA/" || die "scp of artifacts failed"
vcopy_to "$REPO/packaging/homebrew/Casks/godterm.rb" "$RA/godterm-cask.rb" || die "scp of cask failed"
ZN=$(basename "$ZIP"); DN=${DMG:+$(basename "$DMG")}; UN=${UZIP:+$(basename "$UZIP")}

vstep vm-info 30 "VM facts (macOS version, Gatekeeper, network)" <<'EOF'
sw_vers; uname -m; spctl --status; csrutil status
ls ~/.godterm ~/.claude 2>&1
for b in claude grok codex godterm tmux; do printf '%s: ' $b; command -v $b || echo none; done
echo "NOTE: $(sw_vers -productVersion), $(spctl --status)"
EOF

[ -z "$SUMS" ] || vstep checksums 60 "artifacts match SHA256SUMS" <<EOF
cd $RA && shasum -a 256 -c SHA256SUMS --ignore-missing
EOF

# tmux drives the TUI. The base image has Homebrew; tmux is not part of
# GodTerm's install, only of this harness.
vstep tmux 300 "install tmux in the VM (harness only)" <<'EOF' || die "no tmux in the VM, cannot drive the TUI"
export PATH=/opt/homebrew/bin:$PATH HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1
command -v tmux >/dev/null || brew install tmux
tmux -V
EOF


# ============================================================================
# 1. Install: zip on PATH, DMG into /Applications, Gatekeeper
# ============================================================================
if want 1; then
  log "== 1. install from the zip and the DMG"
  vstep 1.zip-contents 30 "zip unpacks: godterm, godterm-speech, README, LICENSE" <<EOF
cd /tmp && rm -rf gz && mkdir gz && cd gz && unzip -q $RA/$ZN
find . -type f | sort
d=\$(ls -d */ | head -1)
test -x "\$d/godterm" && test -x "\$d/godterm-speech"
EOF
  vstep 1.zip-no-appledouble 30 "zip has no AppleDouble (._*) or __MACOSX entries" <<EOF
n=\$(unzip -Z1 $RA/$ZN | grep -cE '(^|/)\._|__MACOSX' || true)
unzip -Z1 $RA/$ZN | grep -E '(^|/)\._|__MACOSX' || true
if [ "\$n" -gt 0 ]; then echo "NOTE: \$n AppleDouble ._ files in the zip (unzip leaves ._godterm etc. next to the binaries)"; exit 3; fi
EOF
  vstep 1.zip-install 30 "install godterm to ~/.local/bin and run godterm --version" <<EOF
mkdir -p ~/.local/bin
d=\$(ls -d /tmp/gz/*/ | head -1)
cp "\$d/godterm" "\$d/godterm-speech" ~/.local/bin/
export PATH="\$HOME/.local/bin:\$PATH"
command -v godterm
v=\$(godterm --version); echo "\$v"
echo "NOTE: \$v"
echo "\$v" | grep -q "^godterm $EXPECT_VERSION "
EOF
  vstep 1.zip-codesign 30 "zip binary: Developer ID signature, hardened runtime" <<'EOF'
codesign --verify --strict --verbose=2 ~/.local/bin/godterm ~/.local/bin/godterm-speech || exit 1
codesign -dv --verbose=2 ~/.local/bin/godterm 2>&1 | tee /tmp/cs.txt
grep -q 'Authority=Developer ID Application' /tmp/cs.txt && grep -q 'flags=.*runtime' /tmp/cs.txt
EOF
  vstep 1.zip-quarantine 30 "zip binary runs with a quarantine attribute (downloaded file)" <<'EOF'
cp ~/.local/bin/godterm /tmp/godterm-q
xattr -w com.apple.quarantine "0083;$(printf %x $(date +%s));Safari;$(uuidgen)" /tmp/godterm-q
xattr -l /tmp/godterm-q
/tmp/godterm-q --version
EOF
  vstep 1.help 30 "godterm --help prints usage" <<'EOF'
export PATH="$HOME/.local/bin:$PATH"
godterm --help | tee /tmp/help.txt | head -5
grep -q 'USAGE:' /tmp/help.txt
EOF
  vstep 1.doctor 60 "godterm doctor runs on a clean machine (no claude, no audio)" <<'EOF'
export PATH="$HOME/.local/bin:$PATH"
GODTERM_HOME=/tmp/h-doctor godterm doctor; rc=$?
echo "doctor rc=$rc"
echo "NOTE: doctor exit $rc"
[ "$rc" -lt 100 ]
EOF
  if [ -n "$DN" ]; then
    vstep 1.dmg-install 60 "DMG: attach, copy GodTerm.app to /Applications, detach" <<EOF
out=\$(hdiutil attach -nobrowse -readonly -noverify $RA/$DN) || exit 1
echo "\$out" | tail -2
vol=\$(echo "\$out" | awk -F'\t' '/\/Volumes\//{print \$NF}' | tail -1)
ls -la "\$vol"
test -L "\$vol/Applications" || echo "NOTE: no Applications link in the DMG"
rm -rf /Applications/GodTerm.app && ditto "\$vol/GodTerm.app" /Applications/GodTerm.app || exit 1
hdiutil detach "\$vol" -quiet || hdiutil detach "\$vol" -force
ls /Applications/GodTerm.app/Contents/MacOS
EOF
    vstep 1.app-codesign 60 "GodTerm.app: codesign --deep --strict, notarized, stapled" <<'EOF'
A=/Applications/GodTerm.app
codesign --verify --deep --strict --verbose=2 $A || exit 1
spctl -a -vv -t exec $A 2>&1 | tee /tmp/sp.txt
grep -q 'source=Notarized Developer ID' /tmp/sp.txt || exit 1
xcrun stapler validate $A || exit 1
EOF
    vstep 1.app-gatekeeper-quarantine 60 "GodTerm.app with quarantine: Gatekeeper accepts, syspolicy_check passes" <<'EOF'
A=/Applications/GodTerm.app
xattr -w com.apple.quarantine "0083;$(printf %x $(date +%s));Safari;$(uuidgen)" $A
xattr -p com.apple.quarantine $A
spctl -a -vv -t exec $A 2>&1 | tee /tmp/sp.txt
grep -q accepted /tmp/sp.txt || exit 1
syspolicy_check distribution $A 2>&1 | tail -3
EOF
    vstep 1.app-version 30 "GodTerm.app binary --version" <<EOF
B=/Applications/GodTerm.app/Contents/MacOS
ls -la \$B
# The cask links Contents/MacOS/godterm; the bundle's executable is GodTerm.
# On a case sensitive volume that path would not exist.
if [ ! -e "\$B/godterm" ] || ! ls \$B | grep -qx godterm; then
  echo "NOTE: no Contents/MacOS/godterm by exact name (only via case insensitive APFS): \$(ls \$B | tr '\n' ' ')"
fi
v=\$(\$B/GodTerm --version); echo "\$v"
echo "\$v" | grep -q "^godterm $EXPECT_VERSION "
EOF
  else
    step SKIP 1.dmg "DMG checks" "no DMG in $ARTIFACTS"
  fi
fi

# ============================================================================
# 2. First launch of godterm on a fresh home
# ============================================================================
if want 2; then
  log "== 2. first launch (fresh ~/.godterm)"
  vrun 20 <<<'rm -rf ~/.godterm ~/.godterm-demo' >/dev/null
  S=first
  tmux_start $S $COLS $ROWS "$GT_ENV godterm"
  ui 2.setup-prompt "first run asks how many agent slots" $S 30 'How many agent slots' first-run-setup-prompt
  if answer_setup $S 2; then
    capture $S first-run-setup-answered
    ui 2.welcome "voice welcome before the UI (Turn voice on / Skip)" $S 30 'Turn voice on' first-run-voice-welcome
    tmux_keys $S Escape
    if ui 2.setup-page "Settings > Setup opens: Claude Code missing" $S 20 'claude not found' first-run-setup-page; then
      if grep -q 'claude.ai/install.sh' "$RESULTS/$LAST_CAP" && grep -q 'SETUP: CLAUDE CODE MISSING' "$RESULTS/$LAST_CAP"; then
        step PASS 2.missing-cli-guidance "install command for Claude Code and the other CLIs shown" "$LAST_CAP"
      else
        step FAIL 2.missing-cli-guidance "install command for Claude Code shown" "$LAST_CAP"
      fi
    fi
    tmux_keys $S Escape
    ui 2.grid "grid with both accounts, 'not installed' and login hints" $S 20 "Claude Code isn't installed" first-run-grid
    sleep 4
    capture $S first-run-grid-later
    if grep -q 'tour 1/' "$RESULTS/$LAST_CAP"; then
      step PASS 2.tour-first-launch "tour shows on the first launch" "$LAST_CAP"
    else
      step WARN 2.tour-first-launch "tour shows on the first launch" "no tour: with Claude Code missing the skip path opens Setup and never starts the tour (README says the first launch shows it), $LAST_CAP"
    fi
    tmux_keys $S C-a '?'
    ui 2.help "Ctrl-a ? opens the key help" $S 10 'godterm help' first-run-help
    tmux_keys $S Escape
    quit_tui 2.quit $S
    vstep 2.files 20 "first run wrote config.toml, voice_onboarding=skipped" <<'EOF'
ls -la ~/.godterm
cat ~/.godterm/voice_onboarding; echo
test -f ~/.godterm/config.toml && grep -q skip ~/.godterm/voice_onboarding
EOF
    # Second launch: the tour should come now.
    S=second
    tmux_start $S $COLS $ROWS "$GT_ENV godterm"
    if ui 2.tour-second-launch "tour shows on the second launch" $S 30 'tour 1/' second-launch-tour; then
      for i in 2 3 4 5 6 7; do
        tmux_keys $S Enter
        expect_screen $S 8 "tour $i/" "second-launch-tour-$i" >/dev/null || break
      done
      tmux_keys $S Enter
      sleep 2
      capture $S second-launch-after-tour
      if grep -q 'tour [0-9]/' "$RESULTS/$LAST_CAP"; then
        step FAIL 2.tour-walk "walk the 7 tour steps with Enter" "$LAST_CAP"
      else
        step PASS 2.tour-walk "walk the 7 tour steps with Enter" "$LAST_CAP"
      fi
    fi
    quit_tui 2.quit-second $S
  else
    capture $S first-run-setup-stuck
    step FAIL 2.setup-answers "answer the first run prompts" "$LAST_CAP"
    tmux_kill $S
  fi
fi

# ============================================================================
# 3. godterm setup (the wizard), quit without finishing
# ============================================================================
if want 3; then
  log "== 3. setup wizard"
  S=setup
  tmux_start $S $COLS $ROWS "GODTERM_HOME=/tmp/h-setup $GT_ENV godterm setup"
  ui 3.first-screen "godterm setup: first prompt" $S 30 'How many agent slots' setup-first
  tmux_literal $S 2; tmux_keys $S Enter
  ui 3.account-1 "asks label for account 1" $S 10 'Account 1 of 2' setup-account1
  tmux_literal $S Work; tmux_keys $S Enter
  ui 3.directory "asks default directory" $S 10 'Default directory' setup-dir
  tmux_keys $S C-c
  rc=$(wait_exit $S 15) || rc=timeout
  capture $S setup-after-ctrl-c
  if [ "$rc" != timeout ]; then step PASS 3.ctrl-c "Ctrl-C leaves the wizard" "exit $rc"
  else step FAIL 3.ctrl-c "Ctrl-C leaves the wizard" "$LAST_CAP"; fi
  tmux_kill $S
  vstep 3.no-config 20 "nothing written but lock and exit files (no config.toml)" <<'EOF'
ls -la /tmp/h-setup
find /tmp/h-setup -type f | sort
test ! -e /tmp/h-setup/config.toml || exit 1
extra=$(find /tmp/h-setup -type f ! -name 'godterm.lock*' ! -name last_exit.json | wc -l | tr -d ' ')
echo "NOTE: files left: $(cd /tmp/h-setup && ls | tr '\n' ' ')"
[ "$extra" = 0 ]
EOF
  # With an existing config (from scenario 2): setup shows the accounts,
  # Ctrl-C must leave config.toml as it was.
  if want 2; then
    vrun 10 <<<'shasum ~/.godterm/config.toml > /tmp/cfg.sha' >/dev/null
    S=setup2
    tmux_start $S $COLS $ROWS "$GT_ENV godterm setup"
    ui 3.existing-first "godterm setup on an existing config: first prompt" $S 30 'slots|Account|\[' setup-existing-first
    tmux_keys $S C-c
    wait_exit $S 15 >/dev/null
    capture $S setup-existing-after-ctrl-c
    tmux_kill $S
    vstep 3.existing-unchanged 10 "existing config.toml unchanged after Ctrl-C" <<'EOF'
shasum -c /tmp/cfg.sha
EOF
  fi
fi

# ============================================================================
# 4. Voice onboarding (no mic, no speakers)
# ============================================================================
if want 4; then
  log "== 4. voice onboarding"
  S=onb
  tmux_start $S $COLS $ROWS "GODTERM_HOME=/tmp/h-onb $GT_ENV godterm --onboarding"
  if answer_setup $S 1; then
    ui 4.welcome "onboarding welcome" $S 30 'Turn voice on' onboarding-welcome
    tmux_keys $S Enter
    ui 4.pieces "voice setup 1 of 3: the pieces (install offer)" $S 20 'voice setup 1 of 3' onboarding-pieces
    tmux_keys $S c
    ui 4.tour-1 "continue without voice: voice tour step 1 of 5" $S 20 'voice tour · step 1 of 5' onboarding-step1
    for i in 2 3 4 5; do
      tmux_keys $S C-a J
      ui "4.tour-$i" "Ctrl-a J: step $i of 5" $S 10 "step $i of 5" "onboarding-step$i" || break
    done
    tmux_keys $S C-a J
    ui 4.recap "recap card with the phrases" $S 10 'voice tour: done' onboarding-recap
    tmux_keys $S Enter
    sleep 2
    capture $S onboarding-closed
    quit_tui 4.quit $S
    vstep 4.recorded 10 "voice_onboarding file records the outcome" <<'EOF'
cat /tmp/h-onb/voice_onboarding; echo
grep -q done /tmp/h-onb/voice_onboarding
EOF
  else
    capture $S onboarding-setup-stuck
    step FAIL 4.setup "answer the setup prompts before onboarding" "$LAST_CAP"
    tmux_kill $S
  fi
  # The same without GODTERM_NO_AUDIO / NO_MIC: the VM has no audio device
  # at all (tart --no-audio), the real "no hardware" case.
  S=onbhw
  tmux_start $S $COLS $ROWS "GODTERM_HOME=/tmp/h-onb2 godterm --onboarding"
  if answer_setup $S 1; then
    ui 4.noaudio-welcome "no audio device: welcome shows" $S 30 'Turn voice on' onboarding-noaudio-welcome
    tmux_keys $S Enter
    ui 4.noaudio-pieces "no audio device: Turn voice on reaches the pieces step" $S 20 'voice setup|pieces|microphone|mic' onboarding-noaudio-pieces
    tmux_keys $S Escape
    sleep 3
    capture $S onboarding-noaudio-skipped
    quit_tui 4.noaudio-quit $S
  else
    capture $S onboarding-noaudio-stuck
    step FAIL 4.noaudio-setup "setup prompts (no audio env)" "$LAST_CAP"
    tmux_kill $S
  fi
fi

# ============================================================================
# 5. Demo mode
# ============================================================================
if want 5; then
  log "== 5. demo"
  S=demo
  tmux_start $S $COLS $ROWS "$GT_ENV godterm --demo --demo-no-external"
  ui 5.grid "demo grid with the made up accounts" $S 40 'Work.*Personal' demo-grid
  sleep 5; capture $S demo-grid-later
  tmux_keys $S C-a '?'
  ui 5.help "Ctrl-a ? help in the demo" $S 10 'godterm help' demo-help
  tmux_keys $S Escape; sleep 1
  tmux_keys $S C-a o
  ui 5.overview "Ctrl-a o overview" $S 10 '[Oo]verview|Work' demo-overview
  tmux_keys $S Escape; sleep 1
  quit_tui 5.quit $S
  vstep 5.no-panic 20 "demo log has no panic" <<'EOF'
find ~/.godterm-demo -name '*.log' | while read -r f; do echo "== $f"; tail -5 "$f"; done
n=$(find ~/.godterm-demo -name '*.log' -exec grep -il panic {} + | wc -l | tr -d ' ')
echo "NOTE: $(find ~/.godterm-demo -name '*.log' | wc -l | tr -d ' ') logs, $n with panic"
[ "$n" = 0 ]
EOF
fi

# ============================================================================
# 6. Homebrew: the public tap, and the repo's formula on the local zip
# ============================================================================
BREW_ENV='export PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ENV_HINTS=1'
brew_check_installed() { # id desc
  vstep "$1" 60 "$2" <<EOF
$BREW_ENV
ls -la /opt/homebrew/bin/godterm
v=\$(/opt/homebrew/bin/godterm --version); echo "\$v"; echo "NOTE: \$v"
echo "\$v" | grep -q "^godterm $EXPECT_VERSION "
EOF
}
# cask_test <id prefix> <cask ref> <label>: install, version, uninstall.
# Returns 1 when the install fails (the later steps are then skipped).
cask_test() {
  local p="$1" ref="$2" label="$3"
  vrun 30 <<<'rm -rf /Applications/GodTerm.app' >/dev/null
  if ! vstep "$p-install" 900 "brew install --cask $ref ($label)" <<EOF
$BREW_ENV
out=\$(brew install --cask $ref 2>&1); rc=\$?; echo "\$out"
[ \$rc = 0 ] || { echo "NOTE: \$(echo "\$out" | grep -m1 '^Error')"; exit 1; }
ls -la /Applications/GodTerm.app/Contents/MacOS /opt/homebrew/bin/godterm
EOF
  then
    step SKIP "$p-version" "cask: godterm --version" "install failed"
    return 1
  fi
  brew_check_installed "$p-version" "cask ($label): godterm --version via the linked binary"
  vstep "$p-uninstall" 120 "brew uninstall --cask ($label)" <<EOF
$BREW_ENV
brew uninstall --cask godterm || exit 1
test ! -e /Applications/GodTerm.app && test ! -e /opt/homebrew/bin/godterm
EOF
  return 0
}

if want 6 && [ "$BREW_MODE" != none ]; then
  log "== 6. Homebrew"
  vrun 30 <<<'rm -rf /Applications/GodTerm.app' >/dev/null
  TAP_OK=0
  if [ "$BREW_MODE" = tap ] || [ "$BREW_MODE" = both ]; then
    if vrun 30 <<<'curl -sf --max-time 15 -o /dev/null https://api.github.com/repos/daniel-farina/homebrew-godterm' >/dev/null 2>&1; then
      TAP_OK=1
    else
      step SKIP 6.tap "public tap daniel-farina/homebrew-godterm" "not reachable from the VM"
    fi
  fi
  if [ "$TAP_OK" = 1 ]; then
    vstep 6.tap-install 600 "brew install daniel-farina/godterm/godterm (formula, public tap)" <<EOF
$BREW_ENV
brew tap daniel-farina/godterm || exit 1
grep -E 'version|url' "\$(brew --repository daniel-farina/godterm)/Formula/godterm.rb"
brew install daniel-farina/godterm/godterm
EOF
    brew_check_installed 6.tap-version "tap formula: godterm --version"
    vstep 6.tap-test 120 "brew test (the formula's test block)" <<EOF
$BREW_ENV
brew test daniel-farina/godterm/godterm
EOF
    vstep 6.tap-uninstall 120 "brew uninstall the formula" <<EOF
$BREW_ENV
brew uninstall godterm && test ! -e /opt/homebrew/bin/godterm
EOF
    cask_test 6.tap-cask daniel-farina/godterm/godterm "public tap"
    vrun 60 <<<"$BREW_ENV; brew untap daniel-farina/godterm" >/dev/null 2>&1
  fi
  if [ "$BREW_MODE" = local ] || [ "$BREW_MODE" = both ]; then
    # The repo's formula with its macOS url pointed at the copied zip, in a
    # throwaway local tap, so the install and test blocks run unchanged.
    FZ="${UN:-$ZN}"
    vstep 6.local-install 600 "repo formula on the local zip (file://$RA/$FZ)" <<EOF
$BREW_ENV
sha=\$(shasum -a 256 $RA/$FZ | cut -d' ' -f1)
brew tap-new --no-git e2e/local >/dev/null || exit 1
T=\$(brew --repository e2e/local)
# The first url/sha256 pair in the formula is the on_macos one.
awk -v u="file://$RA/$FZ" -v s="\$sha" '
  !du && /^[[:space:]]*url "/ { sub(/url "[^"]*"/, "url \"" u "\""); du = 1 }
  !ds && /^[[:space:]]*sha256 "/ { sub(/sha256 "[^"]*"/, "sha256 \"" s "\""); ds = 1 }
  { print }' $RA/godterm.rb >\$T/Formula/godterm.rb
grep -A3 on_macos \$T/Formula/godterm.rb
grep -q "file://$RA/$FZ" \$T/Formula/godterm.rb || exit 1
echo "NOTE: formula url -> $FZ"
brew install e2e/local/godterm
EOF
    brew_check_installed 6.local-version "local formula: godterm --version"
    vstep 6.local-test 120 "brew test e2e/local/godterm (formula test block)" <<EOF
$BREW_ENV
brew test e2e/local/godterm
EOF
    vstep 6.local-uninstall 120 "brew uninstall the local formula" <<EOF
$BREW_ENV
brew uninstall godterm && test ! -e /opt/homebrew/bin/godterm
EOF
    # The repo's cask on the copied DMG (any DMG: the cask only needs
    # GodTerm.app inside).
    FD="$DN"
    if [ -n "$FD" ]; then
      vstep 6.local-cask-write 60 "repo cask with url file://$RA/$FD in the local tap" <<EOF
$BREW_ENV
sha=\$(shasum -a 256 $RA/$FD | cut -d' ' -f1)
T=\$(brew --repository e2e/local); mkdir -p \$T/Casks
sed -e "s|^  url \".*\"|  url \"file://$RA/$FD\"|" -e "s|^  sha256 \".*\"|  sha256 \"\$sha\"|" $RA/godterm-cask.rb >\$T/Casks/godterm.rb
grep -E '^  (url|sha256|depends_on)' \$T/Casks/godterm.rb
EOF
      if ! cask_test 6.local-cask e2e/local/godterm "repo cask"; then
        # Diagnostic: the same cask with depends_on macos: ">= :monterey".
        vstep 6.local-cask-fixed-deps 60 "diagnostic: repo cask with depends_on macos: \">= :monterey\"" <<EOF
$BREW_ENV
T=\$(brew --repository e2e/local)
grep -n 'depends_on macos' \$T/Casks/godterm.rb
sed -i '' 's|depends_on macos: :monterey|depends_on macos: ">= :monterey"|' \$T/Casks/godterm.rb
grep -n 'depends_on macos' \$T/Casks/godterm.rb
EOF
        cask_test 6.local-cask-fixed e2e/local/godterm "repo cask, depends_on fixed (diagnostic)"
      fi
    fi
    vrun 60 <<<"$BREW_ENV; brew untap e2e/local" >/dev/null 2>&1
  fi
fi

collect_logs

[ "$STEP_FAIL" = 0 ]
