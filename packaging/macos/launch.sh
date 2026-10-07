#!/bin/sh
# Opens iTerm (or Terminal) running the godterm inside this bundle, at the
# remembered window position. Mirrors what `godterm install` writes.
res="$(cd "$(dirname "$0")" && pwd)"
bin="$(cd "$res/../MacOS" && pwd)/godterm"
run="$res/run.sh"
bounds="$(env -u CLAUDECODE "$bin" window place 2>/dev/null | head -1)"
set -- $bounds
if [ -d "/Applications/iTerm.app" ] || [ -d "$HOME/Applications/iTerm.app" ]; then
  osascript - "$run" "${1:-}" "${2:-}" "${3:-}" "${4:-}" <<'OSA'
on run argv
  set cmd to quoted form of (item 1 of argv)
  tell application "iTerm"
    activate
    set w to (create window with default profile command cmd)
    if (item 2 of argv) is not "" then
      set bounds of w to {(item 2 of argv) as integer, (item 3 of argv) as integer, (item 4 of argv) as integer, (item 5 of argv) as integer}
    end if
  end tell
end run
OSA
else
  osascript - "$run" "${1:-}" "${2:-}" "${3:-}" "${4:-}" <<'OSA'
on run argv
  tell application "Terminal"
    activate
    do script quoted form of (item 1 of argv)
    if (item 2 of argv) is not "" then
      set bounds of front window to {(item 2 of argv) as integer, (item 3 of argv) as integer, (item 4 of argv) as integer, (item 5 of argv) as integer}
    end if
  end tell
end run
OSA
fi
