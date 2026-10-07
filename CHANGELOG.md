# Changelog

All notable changes to GodTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/). The release workflow
copies the section for a tag into the GitHub release notes.

## [Unreleased]

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

[Unreleased]: https://github.com/daniel-farina/godterm/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/daniel-farina/godterm/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/daniel-farina/godterm/releases/tag/v0.2.0
[0.1.0]: https://github.com/daniel-farina/godterm/commits/v0.2.0
