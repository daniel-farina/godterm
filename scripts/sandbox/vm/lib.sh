
set -u

VM_USER="${VM_USER:-admin}"
VM_PASS="${VM_PASS:-admin}"
SSHPASS_BIN="${SSHPASS_BIN:-$(command -v sshpass || echo /opt/homebrew/bin/sshpass)}"
SSH_BASE=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
  -o ConnectTimeout=8 -o ServerAliveInterval=10 -o ServerAliveCountMax=6
  -o LogLevel=ERROR)
SSH_PW=("${SSH_BASE[@]}" -o PubkeyAuthentication=no -o PreferredAuthentications=password)
SSH_OPTS=("${SSH_PW[@]}")

VM_NAME=""
VM_IP=""
VM_RUN_PID=""
RESULTS=""
KEEP=0
STEP_PASS=0
STEP_FAIL=0
STEP_SKIP=0

ts() { date '+%H:%M:%S'; }
log() { printf '[%s] %s\n' "$(ts)" "$*" | tee -a "$RESULTS/run.log" >&2; }
die() { log "FATAL: $*"; exit 1; }

assert_own_vm() {
  case "$1" in
    godterm-e2e-*) ;;
    *) die "refusing to act on VM '$1' (only godterm-e2e-* VMs are ours)" ;;
  esac
}

free_ram_mb() {
  local line n unit
  line=$(top -l 1 -n 0 | grep PhysMem)
  n=$(echo "$line" | sed -E 's/.* ([0-9]+)([KMG]) unused.*/\1/')
  unit=$(echo "$line" | sed -E 's/.* ([0-9]+)([KMG]) unused.*/\2/')
  case "$unit" in
    G) echo $((n * 1024)) ;;
    M) echo "$n" ;;
    K) echo $((n / 1024)) ;;
    *) echo 0 ;;
  esac
}

avail_ram_mb() {
  local pct total
  pct=$(memory_pressure -Q 2>/dev/null | sed -nE 's/.*free percentage: ([0-9]+)%.*/\1/p')
  total=$(($(sysctl -n hw.memsize) / 1048576))
  if [ -n "$pct" ]; then echo $((total * pct / 100)); else free_ram_mb; fi
}

check_resources() {
  local need_mb="$1" free avail margin="${MIN_AVAIL_MARGIN_MB:-8192}"
  free=$(free_ram_mb)
  avail=$(avail_ram_mb)
  log "host: $(sysctl -n hw.ncpu) CPUs, $(top -l 1 -n 0 | grep 'CPU usage' | sed 's/^ *//')"
  log "host: $(top -l 1 -n 0 | grep PhysMem); available ${avail} MB; $(sysctl -n vm.swapusage)"
  log "host: VM needs ${need_mb} MB (+${margin} MB margin)"
  if [ "$avail" -lt $((need_mb + margin)) ]; then
    die "not enough available RAM (${avail} MB, unused ${free} MB) to boot a ${need_mb} MB VM safely"
  fi
  if [ "$(sysctl -n kern.memorystatus_vm_pressure_level 2>/dev/null || echo 1)" -gt 1 ]; then
    die "host is under memory pressure; not booting a VM"
  fi
  local running
  running=$(tart list 2>/dev/null | awk '$1=="local" && $NF=="running" {print $2}' | grep '^godterm-e2e-' || true)
  if [ -n "$running" ]; then
    die "another godterm-e2e VM is running ($running); one VM at a time"
  fi
}

vm_create() { # base name cpu mem_mb
  local base="$1" name="$2" cpu="$3" mem="$4"
  assert_own_vm "$name"
  log "clone $base -> $name"
  tart clone "$base" "$name" || die "tart clone failed"
  VM_NAME="$name"
  tart set "$name" --cpu "$cpu" --memory "$mem" || die "tart set failed"
}

vm_boot() { # extra tart run args...
  assert_own_vm "$VM_NAME"
  log "boot $VM_NAME (headless)"
  tart run --no-graphics --no-audio --no-clipboard "$@" "$VM_NAME" \
    >"$RESULTS/tart-run.log" 2>&1 &
  VM_RUN_PID=$!
  local i
  for i in $(seq 1 60); do
    VM_IP=$(tart ip "$VM_NAME" 2>/dev/null) && [ -n "$VM_IP" ] && break
    kill -0 "$VM_RUN_PID" 2>/dev/null || die "tart run exited early (see tart-run.log)"
    sleep 3
  done
  [ -n "$VM_IP" ] || die "no IP for $VM_NAME after 180 s"
  log "ip $VM_IP; waiting for ssh"
  local t0=$SECONDS up=0
  for i in $(seq 1 60); do
    if vssh -n -o ConnectTimeout=5 true 2>/dev/null; then up=1; break; fi
    sleep 5
  done
  [ "$up" = 1 ] || die "ssh to $VM_IP did not come up in 300 s"
  log "ssh up after $((SECONDS - t0)) s"
  install_run_key
}

install_run_key() {
  local k="$RESULTS/.ssh/id_ed25519" i
  mkdir -p "$RESULTS/.ssh" && chmod 700 "$RESULTS/.ssh"
  [ -f "$k" ] || ssh-keygen -q -t ed25519 -N '' -C godterm-e2e-run -f "$k"
  for i in 1 2 3 4 5; do
    if "$SSHPASS_BIN" -p "$VM_PASS" ssh "${SSH_PW[@]}" "$VM_USER@$VM_IP" \
      'mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >>~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys' \
      <"$k.pub" 2>/dev/null; then
      SSH_OPTS=("${SSH_BASE[@]}" -o IdentitiesOnly=yes -o PasswordAuthentication=no
        -o KbdInteractiveAuthentication=no -i "$k")
      if vssh -n true 2>/dev/null; then log "run key installed"; return 0; fi
      SSH_OPTS=("${SSH_PW[@]}")
    fi
    sleep 3
  done
  log "WARN: could not install run key; staying on password auth"
}

vm_teardown() {
  [ -n "$VM_NAME" ] || return 0
  assert_own_vm "$VM_NAME"
  if [ "$KEEP" = 1 ]; then
    log "--keep: leaving $VM_NAME running at $VM_IP (ssh $VM_USER@$VM_IP, pw $VM_PASS)"
    log "delete later with: tart stop $VM_NAME; tart delete $VM_NAME"
    return 0
  fi
  log "stop and delete $VM_NAME"
  tart stop "$VM_NAME" --timeout 30 >/dev/null 2>&1 || true
  if [ -n "$VM_RUN_PID" ]; then
    local i
    for i in $(seq 1 20); do kill -0 "$VM_RUN_PID" 2>/dev/null || break; sleep 1; done
    kill "$VM_RUN_PID" 2>/dev/null || true
  fi
  tart delete "$VM_NAME" >/dev/null 2>&1 || log "WARN: tart delete $VM_NAME failed"
  rm -rf "$RESULTS/.ssh"   # the run key is useless once the VM is gone
  VM_NAME=""
}

vssh() { # [ssh opts] cmd...
  "$SSHPASS_BIN" -p "$VM_PASS" ssh "${SSH_OPTS[@]}" "$VM_USER@$VM_IP" "$@"
}

vrun() { # timeout_s  (script on stdin)
  local t="$1"
  perl -e 'alarm shift; exec @ARGV' "$t" \
    "$SSHPASS_BIN" -p "$VM_PASS" ssh "${SSH_OPTS[@]}" "$VM_USER@$VM_IP" \
    'f=$(mktemp /tmp/e2e-step.XXXXXX); cat >"$f"; bash "$f" </dev/null; rc=$?; rm -f "$f"; exit $rc'
}

vcopy_to() { # local_file... remote_dir
  local dest="${!#}"
  local n=$(($# - 1))
  "$SSHPASS_BIN" -p "$VM_PASS" scp -q "${SSH_OPTS[@]}" "${@:1:$n}" "$VM_USER@$VM_IP:$dest"
}

vcopy_from() { # remote_path local_path
  "$SSHPASS_BIN" -p "$VM_PASS" scp -q -r "${SSH_OPTS[@]}" "$VM_USER@$VM_IP:$1" "$2"
}

TMUX_SOCK="godterm-e2e"

VM_PATH='$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'

tmux_start() { # session cols rows env_and_command
  local s="$1" c="$2" r="$3" cmd="$4" b64
  b64=$(printf '%s\n' "$cmd" | base64 | tr -d '\n')
  vrun 30 <<EOF
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK kill-session -t $s 2>/dev/null
rm -f /tmp/e2e-$s.rc
{
  echo 'export PATH="$VM_PATH" TERM=xterm-256color COLORTERM=truecolor LANG=en_US.UTF-8'
  echo '$b64' | base64 -d
  echo 'rc=\$?; echo \$rc >/tmp/e2e-$s.rc; echo; echo "[[e2e: command exited rc=\$rc]]"; sleep 3600'
} >/tmp/e2e-$s.sh
tmux -L $TMUX_SOCK new-session -d -s $s -x $c -y $r "bash /tmp/e2e-$s.sh" \; \
  set-option -t $s remain-on-exit on >/dev/null
EOF
}

tmux_rc() {
  vrun 15 <<EOF 2>/dev/null
export PATH="$VM_PATH"
if [ -s /tmp/e2e-$1.rc ]; then cat /tmp/e2e-$1.rc
else
  d=\$(tmux -L $TMUX_SOCK display -p -t $1 '#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}' 2>/dev/null)
  IFS='|' read -r dead st sig <<<"\$d"
  if [ "\$dead" = 1 ]; then
    if [ -n "\$sig" ]; then echo "signal-\$sig"; else echo "\${st:-1}"; fi
  fi
fi
EOF
}

wait_exit() {
  local s="$1" t="$2" end rc
  end=$(($(date +%s) + t))
  while [ "$(date +%s)" -lt "$end" ]; do
    rc=$(tmux_rc "$s")
    if [ -n "$rc" ]; then echo "$rc"; return 0; fi
    sleep 2
  done
  return 1
}

tmux_keys() { # session keys... (tmux send-keys syntax)
  local s="$1"; shift
  local q="" k
  for k in "$@"; do q="$q '$k'"; done
  vrun 15 <<EOF
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK send-keys -t $s $q
EOF
}

tmux_literal() { # session text
  local s="$1" txt="$2"
  vrun 15 <<EOF
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK send-keys -t $s -l '$txt'
EOF
}

CAP_N=0
LAST_CAP=""
capture() {
  local s="$1" name
  CAP_N=$((CAP_N + 1))
  name=$(printf '%02d-%s' "$CAP_N" "$2")
  LAST_CAP="screens/$name.txt"
  mkdir -p "$RESULTS/screens"
  vrun 15 >"$RESULTS/screens/$name.txt" <<EOF
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK capture-pane -p -t $s
EOF
  vrun 15 >"$RESULTS/screens/$name.ansi.txt" <<EOF
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK capture-pane -p -e -t $s
EOF
  log "  capture screens/$name.txt"
}

screen_now() { # session -> stdout
  vrun 15 <<EOF
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK capture-pane -p -t $1
EOF
}

wait_screen() {
  local s="$1" t="$2" re="$3" end
  end=$(($(date +%s) + t))
  while [ "$(date +%s)" -lt "$end" ]; do
    if screen_now "$s" 2>/dev/null | grep -Eq -- "$re"; then return 0; fi
    sleep 2
  done
  return 1
}

tmux_kill() {
  vrun 15 <<EOF >/dev/null 2>&1
export PATH="$VM_PATH"
tmux -L $TMUX_SOCK kill-session -t $1
EOF
}

expect_screen() {
  local s="$1" t="$2" re="$3" name="$4" rc=0
  wait_screen "$s" "$t" "$re" || rc=1
  capture "$s" "$name"
  [ "$rc" = 0 ] || log "  timed out after ${t}s waiting for /$re/ (see $LAST_CAP)"
  return "$rc"
}

results_init() { # dir
  RESULTS="$1"
  mkdir -p "$RESULTS/screens" "$RESULTS/logs"
  : >"$RESULTS/summary.txt"
  : >>"$RESULTS/run.log"
}

STEP_WARN=0
step() {
  local st="$1" id="$2" desc="$3" note="${4:-}"
  case "$st" in
    PASS) STEP_PASS=$((STEP_PASS + 1)) ;;
    FAIL) STEP_FAIL=$((STEP_FAIL + 1)) ;;
    WARN) STEP_WARN=$((STEP_WARN + 1)) ;;
    *) STEP_SKIP=$((STEP_SKIP + 1)) ;;
  esac
  printf '%-4s  %-20s %s%s\n' "$st" "$id" "$desc" "${note:+  [$note]}" >>"$RESULTS/summary.txt"
  log "$st $id $desc${note:+ [$note]}"
}

vstep() {
  local id="$1" t="$2" desc="$3" rc note
  vrun "$t" >"$RESULTS/logs/$id.log" 2>&1
  rc=$?
  note=$(grep '^NOTE: ' "$RESULTS/logs/$id.log" | sed 's/^NOTE: //' | paste -sd ';' - | cut -c1-200)
  case "$rc" in
    0) step PASS "$id" "$desc" "$note" ;;
    3) step WARN "$id" "$desc" "$note" ;;
    *) step FAIL "$id" "$desc" "rc=$rc, logs/$id.log${note:+; $note}" ;;
  esac
  return "$rc"
}

write_report() { # title started_epoch
  local title="$1" t0="$2" dur
  dur=$(($(date +%s) - t0))
  {
    echo "# $title"
    echo
    echo "- date: $(date '+%Y-%m-%d %H:%M:%S')"
    echo "- VM: ${VM_LABEL:-?}"
    echo "- artifacts: ${ARTIFACTS:-?}"
    echo "- duration: $((dur / 60)) min $((dur % 60)) s"
    echo "- result: $STEP_PASS pass, $STEP_FAIL fail, $STEP_WARN warn, $STEP_SKIP skip"
    echo
    echo '```'
    cat "$RESULTS/summary.txt"
    echo '```'
    echo
    echo "Screens: screens/*.txt (plain text, tmux capture-pane -p) and"
    echo "screens/*.ansi.txt (with colors, capture-pane -e; view with less -R)."
    echo "Logs: logs/ (one per step, plus godterm logs copied from the VM), run.log."
  } >"$RESULTS/report.md"
  log "report: $RESULTS/report.md ($STEP_PASS pass, $STEP_FAIL fail, $STEP_WARN warn, $STEP_SKIP skip, ${dur}s)"
}
