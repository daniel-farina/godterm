#!/usr/bin/env bash
# GodTerm clean-machine test on a fresh Ubuntu 24.04 arm64 VM (tart).
#
# Clones ghcr.io/cirruslabs/ubuntu:24.04 into godterm-e2e-linux-<timestamp>,
# installs the .deb with apt, drives godterm in tmux (first launch, demo),
# then tries the AppImage and the tarball, and deletes the VM (unless --keep).
#
# Usage: scripts/sandbox/vm/linux-e2e.sh [options]
#   --artifacts DIR   release artifacts (default dist/release of the main
#                     checkout, or $GODTERM_ARTIFACTS): the arm64 .deb,
#                     optionally the aarch64 AppImage, the aarch64 tarball
#                     and SHA256SUMS
#   --version V       expected version (default: from the .deb name)
#   --only LIST       scenarios, comma separated (default all):
#                     1 deb install  2 first launch  3 demo  4 onboarding
#                     5 AppImage  6 tarball  7 apt remove
#   --results DIR     where to write (default e2e-results/vm-<ts>/linux)
#   --base IMAGE      tart image (default ghcr.io/cirruslabs/ubuntu:24.04)
#   --cpu N --memory MB   VM size (default 2 / 4096, the maximum allowed)
#   --keep            leave the VM running at the end
#   --no-recommends   apt install without Recommends (skips ffmpeg, faster)
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
ONLY="1,2,3,4,5,6,7"
RESULTS_DIR=""
BASE="ghcr.io/cirruslabs/ubuntu:24.04"
CPU=2
MEM=4096
NO_RECOMMENDS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --artifacts) ARTIFACTS="$2"; shift 2 ;;
    --version) EXPECT_VERSION="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --results) RESULTS_DIR="$2"; shift 2 ;;
    --base) BASE="$2"; shift 2 ;;
    --cpu) CPU="$2"; shift 2 ;;
    --memory) MEM="$2"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --no-recommends) NO_RECOMMENDS=1; shift ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ "$CPU" -le 2 ] || { echo "at most 2 CPUs for the Linux VM" >&2; exit 2; }
[ "$MEM" -le 4096 ] || { echo "at most 4096 MB for the Linux VM" >&2; exit 2; }

RESULTS_DIR="${RESULTS_DIR:-$REPO/e2e-results/vm-$TS/linux}"
results_init "$RESULTS_DIR"
VM_LABEL="Ubuntu ($BASE), $CPU CPU, $MEM MB"
T0=$(date +%s)
want() { case ",$ONLY," in *",$1,"*) return 0 ;; esac; return 1; }

pick() {
  local p f
  for p in "$@"; do
    for f in "$ARTIFACTS"/$p; do [ -f "$f" ] && { echo "$f"; return 0; }; done
  done
  return 1
}
DEB=$(pick 'godterm_[0-9]*_arm64.deb' 'godterm_arm64.deb') || die "no arm64 .deb in $ARTIFACTS"
APPIMAGE=$(pick 'GodTerm-[0-9]*-aarch64.AppImage' 'GodTerm-aarch64.AppImage' || true)
TARBALL=$(pick 'godterm-[0-9]*-aarch64-unknown-linux-gnu.tar.gz' 'godterm-aarch64-unknown-linux-gnu.tar.gz' || true)
SUMS=$(pick SHA256SUMS || true)
if [ -z "$EXPECT_VERSION" ]; then
  EXPECT_VERSION=$(basename "$DEB" | sed -nE 's/^godterm_([0-9][^_]*)_.*/\1/p')
  [ -n "$EXPECT_VERSION" ] || EXPECT_VERSION=$(sed -nE 's/^version = "(.*)"/\1/p' "$REPO/Cargo.toml" | head -1)
fi
log "results: $RESULTS"
log "artifacts: $ARTIFACTS (deb $(basename "$DEB"), expect $EXPECT_VERSION)"

cleanup() {
  local rc=$?
  vm_teardown
  write_report "GodTerm VM e2e: Linux" "$T0"
  log "remaining godterm-e2e VMs: $(tart list | awk '{print $2}' | grep -c '^godterm-e2e-' || true)"
  exit "$rc"
}
trap cleanup EXIT
trap 'log "interrupted"; exit 130' INT TERM

check_resources "$MEM"
vm_create "$BASE" "godterm-e2e-linux-$TS" "$CPU" "$MEM"
vm_boot

RA=/home/$VM_USER/e2e-artifacts
vrun 20 <<<"mkdir -p $RA" || die "mkdir in VM failed"
FILES=("$DEB")
for f in "$APPIMAGE" "$TARBALL" "$SUMS"; do [ -n "$f" ] && FILES+=("$f"); done
log "copy ${#FILES[@]} files into the VM"
vcopy_to "${FILES[@]}" "$RA/" || die "scp of artifacts failed"
DN=$(basename "$DEB"); AN=${APPIMAGE:+$(basename "$APPIMAGE")}; TN=${TARBALL:+$(basename "$TARBALL")}

vstep vm-info 30 "VM facts (distro, glibc, audio, FUSE)" <<'EOF'
. /etc/os-release; echo "$PRETTY_NAME"; uname -m; ldd --version | head -1
ls /dev/snd 2>&1; ls /dev/fuse 2>&1; dpkg -l | grep -E 'libfuse2|libasound2' || true
for b in claude grok codex godterm tmux; do printf '%s: ' $b; command -v $b || echo none; done
echo "NOTE: $PRETTY_NAME, $(ldd --version | head -1 | awk '{print $NF}')"
EOF
[ -z "$SUMS" ] || vstep checksums 60 "artifacts match SHA256SUMS" <<EOF
cd $RA && sha256sum -c --ignore-missing SHA256SUMS
EOF
vstep tmux 600 "apt install tmux (harness only)" <<'EOF' || die "no tmux in the VM"
export DEBIAN_FRONTEND=noninteractive
command -v tmux >/dev/null || { sudo -n apt-get update -qq && sudo -n apt-get install -y -qq tmux; }
tmux -V
EOF

# The tarball on the untouched machine, before apt brings libasound2.
if want 6 && [ -n "$TN" ]; then
  vstep 6.tarball-clean 60 "tarball on a clean machine (before any apt install)" <<EOF
rm -rf /tmp/tgz && mkdir /tmp/tgz && cd /tmp/tgz && tar xzf $RA/$TN || exit 1
b=\$(find . -name godterm -type f | head -1)
if out=\$(\$b --version 2>&1); then echo "\$out"; echo "\$out" | grep -q "godterm $EXPECT_VERSION "; exit; fi
echo "\$out"
if echo "\$out" | grep -q libasound; then echo "NOTE: needs libasound2 first (README says so): \$out"; exit 3; fi
exit 1
EOF
fi

if want 1; then
  log "== 1. .deb install with apt"
  vstep 1.apt-update 300 "apt-get update (the image's package lists are stale)" <<'EOF'
sudo -n apt-get update -qq
EOF
  vstep 1.deb-info 30 ".deb metadata (depends)" <<EOF
dpkg-deb -I $RA/$DN | sed -n '1,30p'
dpkg-deb -c $RA/$DN
EOF
  vstep 1.deb-install 900 "sudo apt-get install ./$DN${NO_RECOMMENDS:+ (--no-install-recommends)}" <<EOF
export DEBIAN_FRONTEND=noninteractive
t0=\$(date +%s)
sudo -n apt-get install -y ${NO_RECOMMENDS:+--no-install-recommends} $RA/$DN || exit 1
echo "NOTE: apt took \$((\$(date +%s) - t0)) s; ffmpeg (Recommends) \$(command -v ffmpeg >/dev/null && echo installed || echo not installed)"
dpkg -L godterm
EOF
  vstep 1.version 20 "godterm --version" <<EOF
command -v godterm
v=\$(godterm --version); echo "\$v"; echo "NOTE: \$v"
echo "\$v" | grep -q "^godterm $EXPECT_VERSION "
EOF
  vstep 1.help 20 "godterm --help prints usage" <<'EOF'
godterm --help | tee /tmp/help.txt | head -5; grep -q 'USAGE:' /tmp/help.txt
EOF
  vstep 1.doctor 60 "godterm doctor on a clean machine" <<'EOF'
GODTERM_HOME=/tmp/h-doctor godterm doctor; rc=$?
echo "NOTE: doctor exit $rc"; [ "$rc" -lt 100 ]
EOF
fi

if want 2; then
  log "== 2. first launch"
  vrun 20 <<<'rm -rf ~/.godterm' >/dev/null
  S=first
  tmux_start $S "$COLS" "$ROWS" "$GT_ENV godterm"
  ui 2.setup-prompt "first run asks how many agent slots" $S 30 'How many agent slots' first-run-setup-prompt
  if answer_setup $S 2; then
    ui 2.welcome "voice welcome before the UI" $S 30 'Turn voice on' first-run-voice-welcome
    tmux_keys $S Escape
    if ui 2.setup-page "Settings > Setup: Claude Code missing" $S 20 'claude not found' first-run-setup-page; then
      if grep -q 'claude.ai/install.sh' "$RESULTS/$LAST_CAP"; then
        step PASS 2.missing-cli-guidance "install command for Claude Code shown" "$LAST_CAP"
      else
        step FAIL 2.missing-cli-guidance "install command for Claude Code shown" "$LAST_CAP"
      fi
    fi
    tmux_keys $S Escape
    ui 2.grid "grid with both accounts and login hints" $S 20 "Claude Code isn't installed" first-run-grid
    tmux_keys $S C-a '?'
    ui 2.help "Ctrl-a ? help" $S 10 'godterm help' first-run-help
    tmux_keys $S Escape
    quit_tui 2.quit $S
  else
    capture $S first-run-setup-stuck
    step FAIL 2.setup-answers "answer the first run prompts" "$LAST_CAP"
    tmux_kill $S
  fi
fi

if want 3; then
  log "== 3. demo"
  demo_smoke 3 demo godterm
fi

if want 4; then
  log "== 4. voice onboarding (no sound card in the VM)"
  for envs in "$GT_ENV" ""; do
    if [ -n "$envs" ]; then S=onb; H=/tmp/h-onb; tag="no audio env"; else S=onbhw; H=/tmp/h-onb2; tag="no sound card, no env"; fi
    tmux_start $S "$COLS" "$ROWS" "GODTERM_HOME=$H $envs godterm --onboarding"
    if answer_setup $S 1; then
      ui "4.$S-welcome" "onboarding welcome ($tag)" $S 30 'Turn voice on' "$S-welcome"
      tmux_keys $S Enter
      ui "4.$S-pieces" "Turn voice on: the pieces step ($tag)" $S 20 'voice setup 1 of 3' "$S-pieces"
      tmux_keys $S c
      ui "4.$S-tour" "continue without voice: voice tour step 1 ($tag)" $S 20 'step 1 of 5' "$S-step1"
      quit_tui "4.$S-quit" $S
    else
      capture $S "$S-setup-stuck"
      step FAIL "4.$S-setup" "setup prompts before onboarding ($tag)" "$LAST_CAP"
      tmux_kill $S
    fi
  done
fi

if want 5; then
  log "== 5. AppImage"
  if [ -n "$AN" ]; then
    vstep 5.appimage-version 120 "AppImage --version (direct, else --appimage-extract-and-run)" <<EOF
cd $RA && chmod +x $AN
if out=\$(./$AN --version 2>&1); then echo "\$out"; echo "NOTE: runs directly: \$out"
else
  echo "direct run failed: \$out"
  out=\$(./$AN --appimage-extract-and-run --version 2>&1) || { echo "\$out"; exit 1; }
  echo "\$out"; echo "NOTE: needs --appimage-extract-and-run (no FUSE): \$out"
fi
echo "\$out" | grep -q "godterm $EXPECT_VERSION "
EOF
    # The demo with the AppImage, extracted (works with or without FUSE).
    vrun 120 <<EOF >"$RESULTS/logs/5.appimage-extract.log" 2>&1
rm -rf /tmp/appimg && mkdir -p /tmp/appimg && cd /tmp/appimg && $RA/$AN --appimage-extract >/dev/null && ls squashfs-root
EOF
    demo_smoke 5.appimage demoai "env APPIMAGE_EXTRACT_AND_RUN=1 $RA/$AN"
  else
    step SKIP 5.appimage "AppImage" "none in $ARTIFACTS"
  fi
fi

if want 6; then
  log "== 6. tarball"
  if [ -n "$TN" ]; then
    vstep 6.tarball 60 "tarball: extract, godterm --version" <<EOF
rm -rf /tmp/tgz && mkdir /tmp/tgz && cd /tmp/tgz && tar xzf $RA/$TN && find . -maxdepth 2 | sort
b=\$(find . -name godterm -type f | head -1)
v=\$(\$b --version); echo "\$v"; echo "\$v" | grep -q "godterm $EXPECT_VERSION "
EOF
  else
    step SKIP 6.tarball "tarball" "none in $ARTIFACTS"
  fi
fi

collect_logs

if want 7 && want 1; then
  vstep 7.apt-remove 300 "sudo apt-get remove godterm" <<'EOF'
export DEBIAN_FRONTEND=noninteractive
sudo -n apt-get remove -y godterm && ! command -v godterm
EOF
fi

[ "$STEP_FAIL" = 0 ]
