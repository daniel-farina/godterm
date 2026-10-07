# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub's private
vulnerability reporting on `daniel-farina/godterm` (Security > Report a
vulnerability), not in a public issue. You can expect an answer within 3
working days, and a fix or a plan within 14 days for anything rated high.

## Supported versions

Only the latest release gets security fixes before 1.0.

## What GodTerm handles

- **Claude account logins.** GodTerm never stores OAuth tokens itself. Each
  account is a Claude Code config folder; claude keeps its login in the
  macOS login keychain (service `Claude Code-credentials-<hash>`) or, on
  Linux, in `<folder>/.credentials.json`. GodTerm reads them only to show
  plan usage, and writes keychain items only in `godterm migrate-accounts`
  (through `security -i` on stdin, never on a command line). It never
  touches the default `~/.claude` login.
- **The control socket.** `~/.godterm/` holds a Unix socket (mode 0600) and
  a per-launch random token file (mode 0600). Every request must carry the
  token. `user_says` (a stand-in for the user's own confirmation) is refused
  unless GodTerm runs in test control mode. Destructive plans need an
  explicit confirmation token from a later user turn.
- **Logs.** `~/.godterm/godterm.log` (rotated at 2 MB) records actions, not
  secrets: tokens and OAuth credentials must never be logged. Privacy mode
  hides account emails on screen.
- **Voice.** Local by default: audio is processed on the machine
  (whisper.cpp, Apple on device recognition, Kokoro). Grok voice is opt in:
  Grok recognition sends microphone audio to xAI and Grok talk back sends
  the reply text to xAI. Debug utterances, when enabled, stay in
  `~/.godterm/voice/debug` (newest 20).
- **Release integrity.** macOS builds are signed with a Developer ID and
  notarized by Apple. Every release ships SHA256SUMS.
