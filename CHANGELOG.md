# Changelog

All notable changes to GodTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/). The release workflow
copies the section for a tag into the GitHub release notes.

## [Unreleased]

## [0.2.7] - 2026-10-08

### Usage
- Failover offers: when a tab's account is about to run out (under
  `usage.failover_pct`, 5% by default) or the tab hits a usage limit, the
  status bar offers to move that session to the account of the same
  provider with the most left. `Ctrl-a F`, a click, or "move it" to the
  assistant moves it; "not now" stops offering. `usage.auto_failover`
  can move idle tabs by itself (ask, auto or off). Grok accounts now show
  their usage buckets too.
- One usage request per account, shared by every GodTerm you have open,
  with a real backoff when the usage service asks to slow down; the
  rate limit note only appears once the numbers shown are getting old.

### Assistant
- Pasting into the assistant's input works (it used to go to the pane
  behind it).
- A typed Enter always sends, even on a busy machine (before, a fast
  Enter could become a new line and the request never went).
- Speaker mute separate from the mic (`Ctrl-a O`, "be quiet"), volume
  with `Ctrl-a +` and `-`, and replies to typed messages in text only
  unless `assistant.speak_typed` is on.
- The panel can float over the panes or dock beside them
  (`assistant.panel`: docked, overlay, auto), shows clearly when it has
  the keys, and shows usage per provider in its title and account menu.
- Asking for a session in a folder that does not exist yet creates it
  (inside your home folder) and opens the session there.

### Fixed
- One terminal cursor: it no longer flashes across the screen while
  GodTerm draws, and the grey band left by select mode is cleared.


## [0.2.6] - 2026-10-08

### Assistant
- Remote Control: say "turn on remote control" and you can drive the
  assistant from claude.ai or the Claude app on your phone (it asks once
  first, since anyone on that claude.ai login can then use the assistant
  and its admin tools). The panel shows the session link; turn it off by
  voice or from the ⋯ menu. It works when the assistant runs on Claude,
  not on Grok, and it ends whenever the assistant restarts (switching
  account, provider or model, a reset); GodTerm says so when that
  happens.

### Look
- A start up splash, the glass crystal and the GodTerm wordmark, shown
  once on a new install and once after an update ("updated to vX").
  Settings > General: "Show splash at start" and "Splash animation".


## [0.2.5] - 2026-10-07

### Setup
- Setup that works out of the box: Settings > Setup shows what GodTerm
  needs (Claude Code required, Grok Build optional, the voice pack:
  ffmpeg, whisper.cpp with a model, espeak-ng and the Kokoro files, the
  speaker model) and installs it for you with your package manager
  (Homebrew, apt, dnf, pacman, winget), the official Claude Code and Grok
  installers, or checksum verified downloads. The first click shows the
  exact commands and sizes; the second runs them. Anything that needs
  sudo runs in a visible tab for your password. It opens at start when
  something required is missing; `godterm doctor` prints the same plan.
- A tab whose program is not installed says so and offers to install it.

### Assistant
- The assistant can run on Claude or on Grok, switchable any time from
  its title menu or by voice ("switch to Grok", "use Opus"); the recent
  conversation carries over.
- It can manage your accounts by voice: add MCP servers and plugins to
  one or several accounts (with the sign in), change settings, and add,
  log in or log out accounts. Every change is planned, shown with the
  exact commands, needs your yes, and is logged.
- A redesigned panel: a toolbar that never clips, the account and model in
  the title, tool calls shown as readable chips, and a waiting question
  that turns into "answered" in place.

### Grid
- Close an account to take it out of the grid without logging it out
  (its tabs keep running) and open it again later, by voice ("close the
  grok accounts", "only account 2 and 3") or from View > Closed accounts.
- Any split layout by voice ("two columns", "big left with the rest
  stacked"), saved across restarts.

### Fixed
- The test run could crash after all tests passed (a speaker started
  during a test).


## [0.2.4] - 2026-10-07

### Self update
- Update signatures are now required by default
  (`update.require_signature = true`): an update is installed only if the
  release's SHA256SUMS carries a valid signature from GodTerm's release
  key. A release without one is still reported by the check, but never
  downloaded or installed ("release vX is not signed; refusing to update.
  Set update.require_signature = false to override"). `--force` lifts only
  the downgrade guard, never the signature check. Every release from 0.2.2
  is signed, so updates keep working.
- Settings > General: "Require signed updates" (turning it off is not
  recommended).

### Voice
- Fixed: saying the wake word alone ("hey god") did nothing after a pause
  or in open mic; it now always answers, in every mode.


## [0.2.3] - 2026-10-07

### Fixed
- The assistant failed on tool heavy replies with "API Error: Claude's
  response exceeded the 300 output token maximum": its reply limit
  (`assistant.max_output_tokens`) now defaults to 8192, and a reply that
  still hits the limit is retried once with twice the limit (then a plain
  message instead of an API error).


## [0.2.2] - 2026-10-07

### Self update
- GodTerm checks GitHub Releases at start and every 6 hours (`[update]`
  enabled, channel, auto_download, require_signature, check_hours) and
  downloads the update for your platform in the background.
- Every update is verified before it is used: its sha256 against the
  release's SHA256SUMS, the minisign signature of SHA256SUMS made with the
  release key, and on macOS the Developer ID code signature.
- Installed side by side in `~/.godterm/versions/<version>` and switched
  with one atomic rename; the previous version is kept. "Restart to
  update" saves your tabs and restarts into the new version with every tab
  restored. The app bundle, AppImage and the Windows installer are updated
  the same way; Homebrew, deb and rpm installs are told the command to
  run instead.
- `godterm update` (`--check`, `--rollback`, `--force`), a status bar chip,
  Settings > About (Check now, Restart to update, Skip this version),
  `Ctrl-a N`, and the assistant's restart_to_update tool.

### Release
- Every release's SHA256SUMS is signed (`SHA256SUMS.minisig`).


## [0.2.1] - 2026-10-07

### Assistant
- "Ask the agent" delegates: one prompt to the tab, nothing read first,
  and the tab's answer is told back to you in a sentence or two when its
  turn ends (it waits while you talk or listening is paused, batches
  answers, gives up after `assistant.answer_wait_min`, and "never mind"
  cancels).
- Status questions start from the tab's own conversation (a new
  `recent_turns` tool); files are read only when you ask about the code.
- No spoken narration: "Let me...", "I'll check..." are dropped from
  speech and shown dim, and nothing before the last tool call is spoken.
- Spoken answers stop after two sentences (`assistant.spoken_sentences`)
  with "Want the rest?"; saying "more" speaks the rest without a new turn.

### Look
- The app icon is the blue crystal, the same as the site and the README
  (macOS app and DMG, the Windows exe and installer, the Linux desktop
  icon).

### Docs and release
- The README says what is local and what optional Grok voice sends to
  xAI.
- Release downloads also have version-less names (for example
  `GodTerm-macos-universal.dmg`), so `releases/latest/download/<name>`
  links keep working across versions.


### Voice
- Background voices: a speaker lock that only accepts your enrolled voice
  (WeSpeaker ResNet34, `godterm voice install-speaker`, Settings > Voice
  and Audio > Train my voice), a near field gate for far away speech,
  Apple voice processing capture on macOS (echo cancellation, noise
  suppression, Voice Isolation via Open macOS mic modes) and RNNoise
  noise suppression. Rejections show in the voice strip and log.

## [0.2.0] - 2026-10-07

The first public release, under the MIT License. claudego is now GodTerm.

### Distribution
- Signed and notarized macOS builds for universal, Apple silicon (arm64)
  and Intel (x86_64): `GodTerm.app` in a DMG and the CLI in a zip each.
- Linux builds for x86_64 and aarch64 as tar.gz, `.deb`, `.rpm` and
  AppImage.
- Windows x86_64: a portable zip and a per user installer (Start menu,
  PATH). The control socket uses AF_UNIX on Windows 10 1803 and newer.
- SHA256SUMS for every artifact.
- `godterm --version` prints the git commit and build date.
- A private Homebrew tap with a formula (CLI) and a cask (app).

### Sessions and accounts
- Any number of Claude accounts side by side, each with its own config
  folder and keychain login, with layouts, paging, split and hidden panes.
- Tabs per account with a New Tab dialog built around a dated folder and
  recent paths, an overview, activity detection and restore on restart
  (eager, staggered).
- Move a live tab to another account and continue the conversation there.
- Bring a session running elsewhere into GodTerm (take over or copy),
  checking scheduled loops before recreating them.
- Sessions from the main `~/.claude`, with copy and move between sources,
  honest token totals and a persisted index.
- `godterm migrate-accounts` moves account folders into
  `~/.godterm/accounts`, carrying each keychain login.
- The claudego to GodTerm rename migrates every login.
- Grok Build accounts and tabs, with usage read from the real billing reply.

### Usage and approvals
- 5 hour and weekly plan usage under each pane and on the dashboard, with
  gradient, banded or mono colors, and back off from 429s.
- An approvals queue across all accounts, and answers to prompts by
  reading the options claude shows.
- macOS notifications for background tabs that need approval or finish.
- A loops view for scheduled prompts in running tabs.

### Interface
- A menu bar (Tabs, View, Voice, Settings) that behaves like macOS, every
  action clickable through a hit region registry, a command palette, a
  tour, clickable help and a labeled screen map.
- A Settings screen that edits `config.toml` live, and live reload of
  `config.toml`.
- Pane headers with label, folder and email; privacy mode hides emails.
- A sidebar listing each pane's tabs on the left, right or top.
- Memory saver, buffer settings and pausing idle background tabs.
- Remembered window position that copes with missing monitors.
- An app icon (crystal core) and `godterm install` / `uninstall` for
  `GodTerm.app` and the `godterm` command.

### Voice
- Local hands free voice control across all sessions: push to talk, wake
  word, open mic, with a live level meter, partial transcripts and a log.
- Whisper (whisper.cpp) or Apple on-device recognition, with `stt-eval`.
- Talk back with Kokoro v1.0 in Rust, sentence by sentence, with barge in.
- A natural language assistant that operates GodTerm on a chosen account:
  opens tabs, prompts them, follows moves and reports delivery.

### Reliability
- Never blocks the UI on child IO, survives output floods, cleans up every
  child on exit, crash recovery and logging, hardened parsers, PTY
  integration tests, and a guided first run setup with `godterm doctor`.
- Tabs start in bypass permissions mode by default (changeable), and tab
  folders are trusted automatically.
- Claude Code session markers never leak into child claudes, and Grok tabs
  keep the user's Claude hooks out.
- The control socket accepts `user_says` only in test control mode.

## [0.1.0] - 2026-10-05

- claudego: config, credentials, usage, sessions, PTY panes and the TUI.

[Unreleased]: https://github.com/daniel-farina/godterm/compare/v0.2.7...HEAD
[0.2.7]: https://github.com/daniel-farina/godterm/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/daniel-farina/godterm/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/daniel-farina/godterm/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/daniel-farina/godterm/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/daniel-farina/godterm/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/daniel-farina/godterm/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/daniel-farina/godterm/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/daniel-farina/godterm/releases/tag/v0.2.0
[0.1.0]: https://github.com/daniel-farina/godterm/commits/v0.2.0
