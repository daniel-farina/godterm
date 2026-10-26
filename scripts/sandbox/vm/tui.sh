# shellcheck shell=bash
# GodTerm TUI steps shared by mac-e2e.sh and linux-e2e.sh (needs lib.sh).

GT_ENV="${GT_ENV:-GODTERM_NO_AUDIO=1 GODTERM_NO_MIC=1}"
COLS="${COLS:-160}"
ROWS="${ROWS:-48}"

# ui <id> <desc> <session> <timeout> <regex> <capture>: one TUI expectation.
ui() {
  local id="$1" desc="$2" s="$3" t="$4" re="$5" cap="$6"
  if expect_screen "$s" "$t" "$re" "$cap"; then step PASS "$id" "$desc" "$LAST_CAP"; return 0; fi
  step FAIL "$id" "$desc" "no /$re/ in ${t}s, $LAST_CAP"
  return 1
}

# quit_tui <id> <session>: Ctrl-a q, confirm with y, expect exit code 0.
quit_tui() {
  local id="$1" s="$2" rc
  tmux_keys "$s" Escape; sleep 1
  tmux_keys "$s" C-a q
  if expect_screen "$s" 10 'Quit godterm\?' "$s-quit-confirm"; then
    tmux_keys "$s" y
  else
    tmux_keys "$s" C-a Q
  fi
  rc=$(wait_exit "$s" 30) || rc="timeout"
  capture "$s" "$s-after-quit"
  if [ "$rc" = 0 ]; then step PASS "$id" "quit cleanly (Ctrl-a q, y)" "exit 0"
  else step FAIL "$id" "quit cleanly (Ctrl-a q, y)" "exit $rc, $LAST_CAP"; fi
  tmux_kill "$s"
}

# answer_setup <session> <slots>: the text prompts of the first run setup
# (slot count, then label and directory per account, all defaults).
answer_setup() {
  local s="$1" n="$2" i
  wait_screen "$s" 30 'How many agent slots' || return 1
  tmux_literal "$s" "$n"; tmux_keys "$s" Enter
  for i in $(seq 1 "$n"); do
    wait_screen "$s" 10 "Account $i of $n" || return 1
    sleep 1; tmux_keys "$s" Enter   # label: default
    sleep 1; tmux_keys "$s" Enter   # directory: default
  done
  return 0
}

# demo_smoke <prefix> <session> <godterm command>: grid, help, quit, no panic.
demo_smoke() {
  local p="$1" s="$2" cmd="$3"
  vrun 20 <<<'rm -rf ~/.godterm-demo' >/dev/null
  tmux_start "$s" "$COLS" "$ROWS" "$GT_ENV $cmd --demo --demo-no-external"
  ui "$p.grid" "demo grid with the made up accounts" "$s" 60 'Work.*Personal' "$s-grid"
  sleep 5; capture "$s" "$s-grid-later"
  tmux_keys "$s" C-a '?'
  ui "$p.help" "Ctrl-a ? help in the demo" "$s" 10 'godterm help' "$s-help"
  tmux_keys "$s" Escape; sleep 1
  quit_tui "$p.quit" "$s"
  vstep "$p.no-panic" 20 "demo log has no panic" <<'EOF'
find ~/.godterm-demo -name '*.log' | while read -r f; do echo "== $f"; tail -5 "$f"; done
n=$(find ~/.godterm-demo -name '*.log' -exec grep -il panic {} + | wc -l | tr -d ' ')
echo "NOTE: $(find ~/.godterm-demo -name '*.log' | wc -l | tr -d ' ') logs, $n with panic"
[ "$n" = 0 ]
EOF
}

# collect_logs: every godterm log in the VM into logs/godterm-logs.txt, and
# one step that fails on any panic.
collect_logs() {
  vrun 30 >"$RESULTS/logs/godterm-logs.txt" 2>&1 <<'EOF'
for f in $(find ~/.godterm ~/.godterm-demo /tmp/h-* -name '*.log' 2>/dev/null); do
  echo "===== $f"; cat "$f"
done
EOF
  if grep -qi panic "$RESULTS/logs/godterm-logs.txt"; then
    step FAIL logs.no-panic "no panic in any godterm log in the VM" "logs/godterm-logs.txt"
  else
    step PASS logs.no-panic "no panic in any godterm log in the VM"
  fi
}
