#!/bin/zsh -l
# The command the GodTerm window runs: a clean environment (no Claude Code
# session markers, which would turn every child claude into a subagent),
# a title, and a pause on exit so errors stay readable.
for v in $(env | sed -nE 's/^(CLAUDECODE|CLAUDE_CODE_[A-Za-z0-9_]*|CLAUDE_PID|CLAUDE_EFFORT)=.*/\1/p'); do
  unset "$v"
done
here="${0:A:h}"
printf '\033]0;godterm\007'
"$here/../MacOS/godterm" "$@"
code=$?
echo
echo "godterm exited with code $code. Press Enter to close this window."
read -r _
