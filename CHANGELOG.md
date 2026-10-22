# Changelog

All notable changes to GodTerm are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [Semantic Versioning](https://semver.org/). The release workflow
copies the section for a tag into the GitHub release notes.

## [0.2.13] - 2026-10-22

Platforms: macOS (Apple silicon and Intel, universal), Linux (x86_64 and aarch64) and Windows (x64; built on GitHub Actions). The Windows installer and zip are not code signed, so SmartScreen will warn: click More info, then Run anyway.

This release adds a spoken first-run onboarding, update mode, a Kokoro voice picker and optional local voices. The onboarding and the local voices are new, so feedback is welcome.

### Added
- Update mode: "update mode", "keep me posted" or "give me updates"
  (also Voice ▾, the command palette and Settings > Voice) and GodTerm
  tells you, a sentence or two per project, what changed in every active
  tab as things move: finished work, approvals, errors, to do steps done,
  subagents and background commands, jobs that ran. Claude tabs are read
  from the new part of their transcript, the other CLIs from their screen
  state. Big news comes at once (at most every `[voice] updates_gap_s`,
  60 s), progress in a roundup every `updates_every_min` (4) minutes and
  only when something changed; nothing already announced is repeated.
  It never talks over you and waits while paused, brainstorming or in
  direct mode; with voice off or muted the same words show in the
  assistant panel. "What's the latest?" gives a roundup any time. "Stop
  updates" or "quiet" ends it. No model is called: the wording is built
  locally, so it costs nothing.
- Voice onboarding: on a brand new install the first window, before any
  UI, speaks a short hook and offers "Turn voice on" (Enter, or say
  "yes") or "Skip" (Esc), with a gentle reminder after 20 s. With a yes it
  installs what voice still needs (the Settings > Setup installer, after a
  second yes), checks the mic with a live meter and the speaker with the
  local Kokoro voice, then walks through five short steps done by voice
  through the real assistant: ask how something works, log in a Claude
  session, create a project in account 1, log in a Grok session, change
  the layout or open loops. Each step is skippable ("skip", "next",
  Ctrl-a J), "stop onboarding" ends it, and a recap card keeps the most
  useful phrases (Ctrl-a J, "what can I say", the palette). Logins and
  new accounts still need their own yes. It runs once; existing users
  never see it unless they ask: `godterm --onboarding`, the palette,
  Settings > Voice > Replay voice onboarding, or "voice onboarding".
- Optional local voices as alternatives to Kokoro, which stays the default
  (Kokoro af_heart). Apple Silicon Macs only, and nothing is downloaded
  unless you ask for it:
  Chatterbox Turbo (more natural, streams its audio, can clone a voice
  from a short clip, about 5 GB RAM, MIT) and Ming-3.0 (5 English voices
  or a voice designed from a description, about 4 GB RAM, MIT). Each is
  installed only after a yes in
  Settings > Voice and Audio, which shows the download (about 2.9 GB),
  memory, speed and license first, and can be removed again. They run in
  a helper process GodTerm manages (kept warm, unloaded when idle,
  restarted after a crash) over a private pipe; when one fails Kokoro
  speaks and a note says why once.
- Kokoro voice choice: Settings lists the 28 English voices, American
  then British, female then male, with their official grades and a
  Preview; the engine row shows where each engine runs, its memory,
  speed and license. Voice commands: "use the Bella voice", "use the
  local voice", "use chatterbox", "use Kokoro", "use the Grok voice".
- Demo mode with all six harnesses: `godterm --demo --demo-harnesses`
  opens a made up world with one account per agent CLI (Claude Code,
  Grok Build, Codex, Cursor, Antigravity, OpenCode), each tab running its
  own scenario with that CLI's screens and approval prompts. Plain
  `godterm --demo` is unchanged.

### Changed
- Demo mode no longer reads your real Chrome profiles: the Browser button
  and dialog show made up profiles from the demo home.

### Release
- The release script signs SHA256SUMS from a private Python venv when the
  system Python lacks the `cryptography` package.

## [0.2.12] - 2026-10-21

Platforms: macOS (Apple silicon and Intel, universal), Linux (x86_64 and aarch64) and Windows (x64; built on GitHub Actions). The Windows installer and zip are not code signed, so SmartScreen will warn: click More info, then Run anyway.

This release adds Codex, Cursor, Antigravity and OpenCode as tab harnesses (thanks to Waylon Brown, #1) and a per tab Browser button.

### Coding agents: Codex, Cursor, Antigravity and OpenCode
- What is supported: launching and resuming them in a tab, typing into them,
  permission modes, approvals answered with each CLI's own keys, and
  voice-driven tab creation. What is not: usage meters, a model picker,
  transcript browsing and transcript based message receipts (a send is
  reported only as delivered or not), moving, copying and failover to
  another slot (refused), and archiving until a session id is recorded.
  Antigravity has no tab job or browser tools.
- Interactive Codex CLI, Cursor CLI, Antigravity CLI (`agy`) and OpenCode
  slots, with agent selection in Add account and setup, binary overrides,
  native launch/resume flags and supported permission modes, by Waylon
  Brown (#1). Codex uses an isolated `CODEX_HOME`; the other three use
  native shared login stores. Login, usage, transcript browsing and
  MCP/plugin administration stay CLI-managed for these harnesses; see
  docs/CLI_HARNESSES.md.
- Making them fit the rest of GodTerm: voice approvals only press keys read
  off the prompt (never Claude's 1 / 2 / Esc guess); a slot of these CLIs
  does not inherit the global `bypass` default (it would switch off their
  sandbox or approvals), only its own `permission_mode = "bypass"` does;
  session ids that could be read as an option are refused in resume flags.
- Checked against the real CLIs (codex 0.153.4, cursor-agent 2026.10.19,
  agy 1.4.4, opencode 2.0.32) with captured screens as test fixtures:
  - OpenCode 2.x rejects `--agent` ("Unrecognized flag"), so plan mode is
    now a per-launch config file (`OPENCODE_CONFIG`, `default_agent`) read
    by a private `--standalone` server; `bypass` maps to `--auto`; a login
    tab runs `opencode auth login`.
  - Approvals, folder trust and "working / waiting / ready" are read from
    each CLI's own screen: Codex answers `y` / `p` / Esc (and 1-4 on its MCP
    prompt), Cursor `y` / Tab / `n` (never "Run Everything"), Antigravity
    1-4 and arrows for folder trust, with the command or tool named in the
    approvals queue.
  - Tab job and browser tools now reach Codex (`-c mcp_servers.godterm...`
    with the tab's identity from the environment, its tools pre-approved
    because GodTerm asks you itself, and the hint as
    `developer_instructions`), Cursor (a per-tab `--plugin-dir`) and
    OpenCode (a per-tab config file), without touching your own config.
    Antigravity only reads its global `mcp_config.json`, so its tabs keep
    refusing, and say why.
  - Their panes get the bar again, without a model picker: the loops and
    Browser buttons stay and the model is chosen in the CLI. The Browser
    button says what Antigravity cannot do.
  - A tab you quit (not one GodTerm closes) keeps the session id the CLI
    prints on exit, so Enter, a restart or "reopen closed tab" resume it.

### Fixed
- A prompt is never pasted into a question dialog. A screen that waits for
  Enter to select or Esc to cancel (a question from the agent, a menu) counts
  as needing an answer, and a delivery that finds one open fails with a
  reason instead of answering it.
- A scheduled job follows its tab when the tab is moved: the job takes the
  tab's new id and account.

### Release tooling
- Windows artifacts can be built on a GitHub hosted runner
  (`build-windows.yml`, `WINDOWS_FROM_CI=1 scripts/release/all.sh`) when the
  Windows build machine is not available.

### Menu bar
- The Approvals button in the menu bar turns brick red and blinks slowly (once a second) while anything waits for your yes, so it is hard to miss; a waiting tab's `!` marker is red too. `ui.blink_attention` (Settings > General, default on) turns the blinking off and leaves it solid red. It pauses while the terminal window is not in front.

### Browser (BrowserSkill)
- A Browser section in Settings and the tools `list_browsers` and
  `browser_status` show BrowserSkill (`bsk`): version, daemon state,
  `bsk doctor`, connected browsers, which Chrome profiles have the extension
  and which accounts have the skill. The look is passive and rate limited,
  runs on a background thread with a timeout on every call and never starts
  the daemon.
- `install_browser_skill` (and a Settings button) copies the BrowserSkill
  skill into chosen Claude and Grok accounts after one confirmation, keeps a
  newer copy unless forced, and allows `Bash(bsk:*)` in a Claude account's
  settings.json (backed up first, other keys kept). Tab agents get the two
  read only tools and a hint on how to pick a browser. GodTerm does not
  install `bsk` or the extension.
- A Browser button at the right end of each pane's bottom bar (next to the
  loops button), also Ctrl-a i and "browser: Chrome profile for this tab" in
  the palette. It opens a small dialog for that tab: the Chrome profiles
  with name, email and the extension's state (not installed, installed but
  off, on). Choosing a profile binds it to the tab and makes it the default
  profile for links. If the extension is missing the dialog offers (after a
  yes) to open its Chrome Web Store page in that profile, and says that
  Chrome makes you press Add to Chrome yourself; if it is installed but off
  it offers to open chrome://extensions there. GodTerm then looks every few
  seconds, only while the dialog or a pending enable is open, and marks the
  tab ready when the extension is on. The button shows the state: browser,
  browser: Work (add), browser: Work (turn on), and a check mark when ready.
  GodTerm only reads Chrome's profile files and never writes to them.
- The binding is per tab: saved with the tab, kept when the tab is moved or
  copied to another account, and scoped. A tab's own `list_browsers` and
  `browser_status` answer with its bound profile and the connected browser
  that is it (matched by the extension's Browser name, or the only one), and
  nothing about other tabs; the tab's hint says to use only that browser.
  The assistant sees it as `browser` in `get_state`.
- `[browser] default_profile` is the Chrome profile folder where links open
  when asked ("open this link in Chrome", `open_path` with http(s) links).
  The profile last chosen on a tab's Browser button sets it; Settings >
  Browser shows it with a Change button that steps through the profiles. A
  tab with its own profile uses it for links opened for that tab. Command:
  `open -na "Google Chrome" --args --profile-directory=<dir> <url>` on macOS,
  `google-chrome --profile-directory=<dir> <url>` on Linux, `start chrome`
  on Windows.

## [0.2.11] - 2026-10-15

Platforms: macOS (Apple silicon and Intel, universal), Linux (x86_64 and aarch64) and Windows (x64; built on GitHub Actions). The Windows installer and zip are not code signed, so SmartScreen will warn: click More info, then Run anyway.

Version 0.2.10 was never published, so this release also carries everything listed under 0.2.10 below.

### Loops & schedules
- GodTerm now owns scheduled jobs. A cron manager keeps them in
  `~/.godterm/jobs.json` and a scheduler tick fires them: it wakes closed
  and archived tabs, follows a catch-up policy for runs missed while GodTerm
  was off, warns before an end date and leaves a receipt for every run. The
  assistant has `create_job`, `list_jobs`, `update_job`, `delete_job`,
  `run_job_now` and `adopt_loop`.
- A Loops & schedules section above the assistant chat shows a detail card
  per row: the prompt in full, the cron in plain words, who runs it, the next
  and last run, the expiry and the id. Labeled buttons (Manage with GodTerm,
  Run now, Stop, Pause or Resume, Delete, Open tab, Close), rows grouped by
  tab with aligned columns, as many rows as fit with wheel and key
  scrolling ("3 to 9 of 14"), and duplicate loops shown once. The Loops view
  shows the same card.
- Each pane has a loops drawer: the tab's loops with next and last run, Run
  now, Cancel, Copy and New loop (Ctrl-a ;). Grok's scheduled tasks are read
  too, and `show_tab_loops` opens it by voice.
- The agents inside Claude and Grok tabs can manage their own GodTerm jobs.
  Each tab gets a `godterm` MCP server with only `create_job`, `list_jobs`,
  `update_job`, `delete_job` and `adopt_loop`, limited to that tab's jobs and
  loops (it cannot name another tab, and has none of the assistant's tools),
  and a hint to prefer them over CronCreate and /loop. Each new job or change
  asks you first, in a dialog against that tab. Switch: `tabs.jobs_tools`
  (default on), applied to tabs started or resumed from then on.

### Assistant
- The assistant panel is a window: width carets and Ctrl-a { }, edge drag,
  minimize to a strip, maximize and close, a quick action bar and a framed
  three row composer that grows. It starts open, and the mic starts off
  unless it was on at the last quit.
- Replies render markdown: tables as aligned grids (cards when narrow),
  lists, code, bold and line breaks. Speech never reads markup.
- `send_prompt` to a busy tab takes a mode: auto, queue, background or
  side. A question goes through Claude Code's /btw side channel and the
  answer is read back; a work request goes to a background session forked
  from the tab's session; queue stays explicit. The receipt names the mode.
- Deliveries to a tab that just opened wait up to 40 seconds for Claude's
  first turn before calling it dropped.
- `open_accounts` puts a hidden pane back on screen.
- The Loops & schedules section is set apart from the chat: a shaded
  title row, its lines and a divider rule, in rows the chat never uses at
  any panel height. The chat has its own scrollbar on the panel's right
  border (drag the thumb, click the track, or use the wheel), keeps its
  place while new lines arrive, and the wheel goes to the area under the
  pointer.

### Voice
- Direct mode: talk to one tab by voice and return to the assistant on a
  phrase or the wake word. A mic badge on the pane that holds the voice has
  a menu to move it.
- Brainstorm mode (Ctrl-a B or the quick bar): speech is drafted in the
  composer until you say send it, done or that's it. Cancel with undo, have
  it read back or organized. A longer end of speech follows a trailing
  connective (`voice.connective_hold_ms`).

### Tabs
- Archive a tab (Ctrl-a f, tab options, the Tabs menu): an Archived group in
  the tab list, double click to resume, kept in a persistent store. The
  assistant has `archive_tabs` (with a dry run and a confirm plan),
  `list_archived` and `restore_archived`.
- A model bar under each pane switches a tab's model with its own /model,
  queued until a busy tab is idle (Ctrl-a #, `set_tab_model`).

### Panes
- Every pane header has `<` `>` carets like the assistant panel's: narrower
  and wider by a tenth of the row, the neighbour giving or taking the space,
  kept per shape like a border drag. Shift-click or double click for equal
  widths; "pane wider", "pane narrower" and "equal pane widths" in the
  command palette. Hidden when zoomed or with no neighbour.

### Fixed
- The wheel scrolls a pane's history right after a click or a tab switch.
  An agent that turned mouse reporting on from the main screen took every
  wheel event and scrolled nothing, so scrolling only came back after
  typing. On the main screen the history is GodTerm's to scroll (the
  agent still gets the wheel when there is none); on the alternate screen
  the agent gets mouse events, or arrow keys if it did not ask for them.
- The wheel scrolls the tab list by drawn rows anywhere in the list, and
  wheel bursts are coalesced.

## [0.2.10] - 2026-10-12

### Assistant
- The assistant input edits like a text field: a caret that moves by
  character, word, line and row, selection with Shift and the mouse, and
  typing and pasting at the caret. Image chips stay whole.
- The assistant chat takes images: Ctrl-V, an empty paste or a dragged
  image path attaches one, the assistant sees it, and `send_prompt` and
  `open_tab` pass it on to tabs. Images are kept owner only and removed
  after 7 days.

### Resume
- A resume blocked because Claude Code runs that session in the
  background now offers attach, stop or fork, in that same tab (the
  assistant's `fix_resume`). Sessions are matched by their id,
  `open_session` resumes a session on the account where it lives, and
  `read_tab` reads exactly the tab asked for.

### Panes
- Collapse a pane to a one row strip (Ctrl-a _, a click, the View menu
  or by voice) or sideways to a narrow column (Ctrl-a \ or the caret at
  the left of its header). The panes next to it take the space and its
  tabs keep running. View > Expand all brings every pane back, and
  `expand_collapsed_on_focus` makes a collapsed pane open when it gets
  the focus.
- Each pane header has window controls at the top right: minimize (to a
  strip), maximize (zoom, Ctrl-a z) and close (asks first; the account
  stays logged in).
- iTerm2: its cursor guide is turned off while GodTerm runs, so no band
  crosses every pane at the cursor row.

### Tabs
- Tab lists have a search field (fuzzy, by name, session title, folder
  and status) and a Sort menu (including most active and by status);
  `search_tabs` does the same by voice.
- Tab lists resize with Ctrl-a < and >, by dragging, by voice or with
  the `tab_list_columns` setting, up to 64 columns. From 36 columns each
  tab also shows its session's own title.
- Double click a tab for one panel with Move on top and every tab option
  below (rename, session title, color, pin, group, move, duplicate, copy
  session, restart, interrupt, folder actions, close), the same list as
  the right click menu.

### Fixed
- A tab keeps its history when you switch to it from a tab list, so the
  mouse wheel scrolls back through it again.

## [0.2.9] - 2026-10-11

### Terminal
- The UI never waits on the terminal: frames go out through a writer
  thread, so a stalled or slow terminal no longer freezes GodTerm.

### Supervisor
- `godterm supervisor install` adds a launchd watchdog that opens GodTerm
  again after a crash, kill or closed window (every tab comes back, the
  assistant resumes), restarts a GodTerm whose control socket stopped
  answering for 2 minutes, and gives up with a notification after 3
  relaunches in 10 minutes. `godterm supervisor status` and `uninstall`;
  `supervisor.enabled = false` pauses it. macOS.

### Restart
- A restart continues the tabs that were working, and only those. A tab
  waiting on an approval mid turn is continued too and asks again.

### Assistant
- Remote Control tells the truth: turning it on waits for the real
  answer (the link, or why it failed), retries, and heals itself.
  `assistant.remote_control = true` keeps it on and brings it back when
  the bridge drops; the panel title shows Remote while connected.
- The assistant keeps its own claude session across launches, crashes
  and updates (`assistant.resume`, on by default), so the chat continues
  where it was.
- It watches the account it runs on: a warning once per window under
  `assistant.switch_warn_pct`, and under `assistant.switch_at_pct` (or on
  a usage limit) it moves itself to the account of the same provider
  with the most left, conversation and Remote Control kept
  (`assistant.auto_switch`: auto, ask or off).
- It looks inside projects: subprojects, READMEs and recently touched
  files in find, `search_projects` for deeper looks, saved project
  descriptions, and `rename_session` for Claude and Grok sessions.
- A finished request starts with "Done:", then what changed and a short
  explanation. Spoken replies end where they end, with no offer to read
  the rest.
- Answers from tabs always come back, and updates wait for a quiet
  moment instead of cutting in.
- Requests to tabs get receipts: received, started, done, or dropped and
  interrupted, said out loud with an offer to resend.
- A question asked through `open_tab` is reported back.
- The assistant never interrupts a working tab unless you ask it to.
- It knows everything GodTerm already told you, so it does not repeat
  it, and tabs are named by their project.

### Tabs
- Tab rows carry a status label: finished, background, blocked, confirm,
  limit, error or queued. A tab that handed its work to background
  agents shows background, and finished is said only once that work is
  done.

### Voice
- Barge-in needs words, not noise: the reply ducks while the overlap is
  transcribed and stops only on a command word or enough real words.
- Headphone tap (`voice.headphone_tap`, off by default): play/pause on
  headphones or the media key turns the mic on or off, and a tap on opens
  open mic so the next words need no wake word. It never cuts off a
  reply. GodTerm holds Now Playing while it is on, so Music and Spotify
  do not get the press; off or quitting gives it back. With the headset's
  own mic, its mute gesture stops open mic and closing the mic lets the
  headset leave call mode. `godterm tap-test` shows the presses. macOS.
- Voice comes back as it ran: the mode (open mic, wake word, push to
  talk) is restored after a restart. A sleeping open mic stays open mic,
  and the wake word wakes it into open mic again.
- A Bluetooth headset mic that gave no audio through Apple capture is
  recorded with ffmpeg.
- No reopen loop after a settings reload, and no recorder outlives
  GodTerm on quit.

### Fixed
- Windows: saved project descriptions and tab names find their folder
  however the path is spelled; process trees stop at a parent loop.
- Linux and Windows builds: macOS only media key and supervisor parts
  are left out there.

## [0.2.8] - 2026-10-08

### Panes
- Select text inside a pane: drag over it, double click a word, triple
  click a line. The selection stays in that pane and holds while the view
  scrolls; dragging past the top or bottom edge scrolls the history.
  Letting go copies it (with OSC 52 over SSH or when there is no
  clipboard program) and says how many characters were copied. Trailing
  spaces are dropped and wrapped lines join back. Tabs that ask for the
  mouse still get it; hold Shift (or Option) while dragging to select
  there. `Ctrl-a c` copies the selection again. The assistant's
  conversation selects the same way.

### Updates
- A calm chip at the right of the menu bar says when a new version is
  out or ready ("↑ v0.2.8", "↑ Update ready"); it shows in the status
  bar when the menu bar is hidden, and stays still with `splash_motion`
  off or Reduce Motion.
- A click on it, or `Ctrl-a D`, opens Updates: this version, the latest,
  and what's new in every newer version. Download (with the usual
  signature and checksum checks), Restart to update (asks first; every
  tab comes back and resumes its conversation), Release page and Later.
  Homebrew and system installs get their upgrade command instead.
- The assistant can answer "is there an update?" and "what's new?", and
  restarting to update always takes one yes.

### Assistant
- History, Admin, Rules and Prompt have a "‹ Conversation" button (and
  Esc) back to the conversation going on, with a dot or a count for
  replies that came while you were away. History pins the current
  conversation at the top.
- Questions that need your yes (a move, a close) show Yes and No
  buttons. Every dialog's default button, what Enter does, is marked.
- A click outside the assistant panel gives the keys to what was
  clicked: a tab, a pane header, the menu bar. Clicks on the floating
  panel or a dialog never reach the tab behind it.
- The Grok assistant answers faster (a plain reply or a tool call takes
  about 30% less time) and keeps GodTerm's instructions from the first
  turn.

### Splash
- In Terminal.app the start up crystal is drawn with whole cells, so it
  shows without gaps between the rows.

### Fixed
- Prompt delivery presses Enter only once the tab shows the paste, so a
  prompt sent to a busy tab is submitted instead of left in its input.
- Voice "send to tab" and pasting a login code do the same: Enter goes
  once the tab shows the paste.

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

[Unreleased]: https://github.com/daniel-farina/godterm/compare/v0.2.11...HEAD
[0.2.11]: https://github.com/daniel-farina/godterm/compare/v0.2.9...v0.2.11
[0.2.10]: https://github.com/daniel-farina/godterm/compare/v0.2.9...v0.2.10
[0.2.9]: https://github.com/daniel-farina/godterm/compare/v0.2.8...v0.2.9
[0.2.8]: https://github.com/daniel-farina/godterm/compare/v0.2.7...v0.2.8
[0.2.7]: https://github.com/daniel-farina/godterm/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/daniel-farina/godterm/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/daniel-farina/godterm/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/daniel-farina/godterm/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/daniel-farina/godterm/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/daniel-farina/godterm/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/daniel-farina/godterm/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/daniel-farina/godterm/releases/tag/v0.2.0
[0.1.0]: https://github.com/daniel-farina/godterm/commits/v0.2.0
