# Embedded browser

A browser inside GodTerm for building and checking sites locally: dev
servers, local apps and tools, and public pages that need no login. It
opens beside the terminal in the web app, the user sees and clicks it
live, and the tab's agent drives the same page with the `embedded_browser`
tool. One headless Chrome is shared by all tabs; every tab owns its own
pages in it and cannot see or touch another tab's. Off by default
(`web.embedded_browser`).

Pages that need the user's accounts (Gmail, X, signed-in dashboards) stay
on BrowserSkill (`bsk`) in the user's real Chrome. The agent tool's
description says so, and so does the tab hint.

## Phase 1: what was checked, and the architecture chosen

Facts verified on this Mac (Chrome 157.0.8103.79, bsk 0.5.0):

- Headless Chrome started with `--headless=new --remote-debugging-port=0
  --user-data-dir=<temp>` writes `DevToolsActivePort` in its profile dir
  (the port, on 127.0.0.1) and serves `/json/list` with a page target and
  its `webSocketDebuggerUrl`. Over that socket this was shown to work:
  `Page.startScreencast` frames (JPEG, base64, with `deviceWidth` and
  `deviceHeight`), `Input.dispatchMouseEvent` clicks, `Runtime.evaluate`,
  console events (`Runtime.consoleAPICalled`, `Log.entryAdded`) and
  `Network.responseReceived` / `loadingFailed`, `Page.captureScreenshot`.
- Chrome's debugging HTTP endpoint refuses HTTP/1.0 (empty answer) and
  answers HTTP/1.1.
- BrowserSkill cannot target a CDP endpoint. `bsk session start` and
  `bsk open` take `--browser <instance id or label>`; the instances listed
  by `bsk browsers` are Chrome profiles where the BrowserSkill extension is
  installed and paired with the bsk daemon (`bsk daemon pair`, a local
  pairing link pasted into the extension; extension protocol messages like
  `session.start`, `tool.click` travel over the daemon's local websocket).
  There is no flag or environment variable for a debugging port or a
  websocket URL (checked in `--help` of every command and in the binary's
  strings).
- GodTerm does not install that extension. `src/browser_skill.rs` copies
  the agent skill, and `src/browser.rs` only detects the extension's folder
  inside a Chrome profile (`Extensions/<EXTENSION_ID>`).

### Can BrowserSkill drive the embedded browser?

Not as built, and not without shipping something GodTerm does not have:

1. It has no CDP attach mode. The only route is its extension.
2. The extension comes from the Chrome Web Store and GodTerm holds no copy
   of its source or package, so it cannot be loaded into a managed
   Chromium (`--load-extension` also needs an unpacked folder; branded
   Chrome 137 and later ignores that switch unless a feature flag turns it
   back on).
3. Even with the extension loaded, the browser would have to be paired
   with the user's bsk daemon through `bsk daemon pair`, and it would then
   show up in `bsk browsers` next to the user's real profiles, where a
   mistaken `--browser` choice could point an agent at the wrong one.

This was not tried by loading anything, because that would touch the live
daemon and pairings. If BrowserSkill gains a CDP endpoint option, the
managed Chrome here already exposes exactly that (loopback port in its own
profile), and the `embedded_browser` tool could be retired in favour of it.

### Candidates

(a) Managed Chromium streamed with CDP screencast. Works on every site
(no frame-ancestors or X-Frame-Options limits), the same pictures reach a
phone over Tailscale, input goes back as mouse and key events, and the
agent gets console, network, DOM text and screenshots from the same
connection. Costs one Chrome (about 500 MB for the browser itself and
about 100 MB per simple page, see Memory).

(b) iframe. Free and instant, but only for pages that allow framing: most
dev servers do, many sites do not, and the agent could not read console,
network or screenshots across origins, nor click for the user. Not
enough alone.

(c) BrowserSkill attached to the managed Chromium. Not possible, see above.

Chosen: (a). A hybrid with an iframe fast path was considered and not
built: it would give the agent and the user different views of the same page
and would break the same-view guarantee for little gain, since localhost
streaming at 70 ms intervals is already fast. The iframe path can be
added later for the user-only case.

## Design

Rust (`src/embedded.rs`, `src/app_embedded.rs`):

- One shared Chrome for all of GodTerm, started lazily on first use. Every
  GodTerm tab (chat session) that uses the embedded browser owns a set of
  pages in it, up to 6 page tabs (`MAX_PAGES`). Before this, every tab had
  its own Chrome; see Memory below for what that cost.
- The Chrome runs headless with `--remote-debugging-port=0` and
  `--remote-debugging-address=127.0.0.1`, `--no-startup-window` (it has no
  page until a tab asks for one and stays alive with none), one temp
  `--user-data-dir` under `<tmp>/godterm-embedded/p<pid>-b<n>/`, no
  extensions (also no component extensions), no sync, downloads denied.
  The user's profiles, cookies and keychain are never used.
- One CDP socket is opened on the browser target, and one thread reads it.
  Pages are attached with flattened sessions (`Target.setAutoAttach` with
  `waitForDebuggerOnStart`, so a new window is set up before its first
  script runs, then resumed). Every page GodTerm opens is its own window
  (`Target.createTarget` with `newWindow`), and focus is emulated per page,
  so the active pages of several tabs render and screencast at the same
  time.
- Ownership. The `Browser` keeps a `Book`: every page target is tagged with
  the uid of the GodTerm tab that owns it, and its CDP session id maps back
  to that target. Events are routed to the owner by that tag. Every command
  an owner sends goes through `Browser::call_for` (page commands, checked by
  session id), `Browser::target_for` and `Browser::close_for` (activate and
  close, checked by target id); all three ask `Book::owns` first and answer
  "that page is not one of this tab's pages" otherwise. That is the one
  place where ownership is enforced. On top of it, page ids are unique over
  all tabs and never reused, and a tab's agent tool and web pane only look
  ids up in that tab's own list, so another tab's id is simply "no tab with
  that id".
- Popups (`window.open`, `target=_blank`) belong to the owner of the page
  that opened them (`openerId` of the new target, which Chrome also sets
  for `noopener` links) and become that owner's active page tab. A seventh
  page tab of one owner is closed again at once, and so is a popup over the
  global limit. A page target nobody claims (not opened by GodTerm and with
  no known opener) is closed after 5 seconds.
- Limits: 6 page tabs per owner, asleep or not, and a global safety limit
  on pages awake over all owners (`web.embedded_browser_max_pages`, default
  40, 6 to 200). Sleeping pages do not count against the global limit. At
  the limit a new tab, a popup and a wake are refused with a reason.
- Cookies and storage: all pages live in ONE browser context, as asked (a
  single Chrome session with several tabs). So pages of different chat
  sessions share cookies and local storage in this throwaway profile: a
  login made in one tab's page is visible to another tab's page on the same
  site. The profile still has none of the user's real Chrome logins, and it
  is deleted when the browser exits. Switching to one isolated context per
  owner later is a small change: `Browser::context_for(uid)` is the hook
  (it returns none today) and `create_for` already passes its answer as
  `browserContextId`; it would call `Target.createBrowserContext` once per
  owner, remember the id, and dispose of it when the owner ends. Popups
  inherit the context of their opener by themselves.
- Per owner (`Session`), unchanged from the outside: exactly one page is
  active, and the address bar and every agent action follow it. Console
  and network buffers are per page. The viewport is per owner and applied
  to all of its pages. The hit test, edit and source ops and the address
  policy work on every page as before. Closing the active page activates
  its left neighbour; closing the last one leaves a fresh blank page, so an
  owner never has zero pages. A page that closes itself (`window.close`)
  is handled the same way.
- Screencast: only owners with a web pane attached are screencast, and
  only their active page. `Session::cast` is the single place that starts,
  moves and stops it (a pane attaches or leaves, the active page changes,
  the viewport changes). Frames are acked at once, the latest one is pushed
  to viewers at most every 70 ms through the existing web outbox (one
  picture in flight per client, so a slow phone drops frames rather than
  queueing them) as binary kind 7. One thread per owner keeps the address
  bar facts (url, title, back and forward, loading) fresh.
- Address policy (`check_url`): only http and https. `file:`, `chrome:`,
  `devtools:`, `view-source:`, `javascript:`, `data:`, `about:` (except
  `about:blank`) and the like are refused with a reason, before any browser
  starts. A bare `localhost:3000` becomes `http://localhost:3000`, other
  bare hosts become https. A page that navigates itself to a disallowed
  scheme is sent back to `about:blank` and the event is logged to the
  console buffer. The same policy applies to every page of every owner.
  JavaScript dialogs are answered automatically (alerts accepted, the rest
  dismissed) and logged.
- No listener beyond loopback exists: the CDP port is Chrome's own on
  127.0.0.1, the web app only relays frames through the normal
  authenticated websocket. Any local process of the same user can reach a
  loopback CDP port, the same as any dev tool. `--remote-debugging-pipe`
  would remove even that. It was not done here: the reader is built on a
  websocket with read timeouts, the pipe needs inherited descriptors 3 and
  4 on Unix and handle passing on Windows, and that is more than a small
  step. With one browser there is now one such port instead of one per
  tab.

### Page sleep

The aim is that page tabs are still there when the user comes back, without
keeping every renderer alive for ever.

- A page nobody has viewed or driven for a while
  (`web.embedded_browser_sleep_minutes`, default 30, 0 never) is put to
  sleep: GodTerm reads its address, title and scroll position, then closes
  the page target, which ends its renderer process. The page tab entry
  stays with the same id, its place in the owner's strip, address, title,
  scroll position and its console and network buffers. Closing the target
  was chosen over navigating to `about:blank` because only that ends the
  renderer; measured, it frees about 92 MB for a simple page.
- Never put to sleep: the page shown in an attached web pane, a page that
  is loading, and a page used within the window (an agent action or a pane
  message counts as use of the active page). An owner's active page sleeps
  last, only once its other pages are asleep, and not while an agent action
  is running.
- Waking is automatic: any agent action on the page (all act on the active
  page; `switch_tab` wakes its target), the web pane showing it (attaching,
  any pane message, clicking the tab) or the pane still being attached
  after the browser was lost. The page is created again at its address,
  the scroll position is restored best effort, and then the action runs.
  `tabs`, `status`, `console` and `network` only read what GodTerm keeps
  and do not wake.
- The trade-off: a woken page is a reload. Form input, script state,
  session history (back and forward) and anything else not saved by the
  page itself are gone. The first agent result after a wake carries `woke:
  true` and a `note` saying so, once. `tabs` lists a sleeping page with
  `asleep: true`, `status` reports `owned_pages` and `asleep_pages` (and
  `asleep: true` when the active page sleeps), and the pane's meta carries
  `asleep` per tab.
- In the web pane a sleeping tab is dimmed and italic with a tooltip, and a
  click wakes it.

### Lifecycle

- Closing a GodTerm tab closes that owner's pages only, asleep or not
  (`reap`), and so do the tool's `close` and the pane's Stop.
- The browser itself exits when no page has been awake for 3 minutes, when
  the setting is turned off, and when GodTerm quits (`shutdown_all`).
  Chrome runs in its own process group and the whole group is killed; the
  temp profile is removed. Owners keep their sleeping page tabs when the
  browser exits for idleness, and the next use starts it again.
- A crashed or killed Chrome is noticed when its socket closes (and by a
  check of the process every 2 seconds). Every page that was awake becomes
  a sleeping page tab, its console gets a line about it, and the browser is
  started again on the next use; the first agent result of every affected
  owner carries `woke: true` and a note that the browser had stopped and
  its pages were reloaded.
- A housekeeping thread (`housekeep`, every 2 seconds, off the UI thread)
  does the sleeping, the idle exit and the crash check.
- If GodTerm is killed hard, the next browser start sweeps directories
  whose owner pid is dead (`sweep_stale`, which also checks the process
  command line before killing anything). Screenshots live in
  `<tmp>/godterm-embedded/p<pid>-shots/t<uid>/`, are removed with their
  owner, and are swept the same way.

### Memory

Measured on a Mac with Chrome 157, resident memory summed over the Chrome
process tree, four owners with one simple page each (the local test site's
home page):

| | RSS |
| --- | --- |
| before: one Chrome per tab (4 browsers, 40 processes) | about 3690 MB |
| now: one shared Chrome (4 pages) | about 1024 MB |
| now, one of the four pages asleep | about 931 MB |
| now, all four asleep (before the idle exit) | about 508 MB |

About 240 MB of the saving per browser is the
`--disable-component-extensions-with-background-pages` switch (without it
the shared browser measured 1264 MB for the four pages); the rest is
sharing the browser, GPU, network and storage processes. RSS counts shared
memory once per process, so the real numbers are lower, but the ratio
holds. The old numbers come from starting four Chromes with the old
switches on the same page; the new ones from the ignored test
`memory_of_four_tabs_in_one_browser`.

Agent tool `embedded_browser` (in the tab tool set, own tab only; the
socket thread runs it, after the App thread checked the setting and the
tab, so a slow page never stalls the UI): `open`, `navigate`, `back`,
`forward`, `reload`, `screenshot` (also returned as an MCP image block,
plus a file path), `snapshot` (numbered clickable elements, refs for
click and type), `click` (ref, selector, text or x and y), `type`,
`press`, `scroll`, `hover`, `text`, `html`, `eval`, `wait`, `console`,
`network`, `resize` (device phone, tablet, desktop), `tabs` (id, url,
title, active of every page tab this tab owns, `asleep` when it sleeps),
`new_tab` (url optional), `switch_tab` (id), `close_tab` (id), `status`
(also `owned_pages` and `asleep_pages`), `close` (closes all of this tab's
pages). All other actions act on the active tab. Every answer carries the
active page url and title, and `woke: true` with a `note` once after the
page was reloaded from sleep. The tool description tells agents that pages
are limited and can sleep; it no longer mentions a limit on browsers.

Web (`web/js/ebv.js`, `web/js/term.js`, `web/js/manage.js`): each pane has
a split area. A compact tab strip sits above the address bar: one tab per
page (title, a close button), a plus button (off at 6 tabs), horizontal
scroll when crowded, the active tab kept in view. Clicking a tab switches
the live view; the strip follows the agent when it opens or switches tabs.
Tab messages (`tab_new` with `url`, `tab_switch` and `tab_close` with
`page`) are answered with the new meta, which carries `tabs` and `active`.
The browser sits right of the terminal with a drag divider
on desktop and stacks under it on phones (the divider is horizontal
there). Address bar, back, forward, reload or stop, a Keyboard button for
touch devices, hide and stop. Taps click, drags scroll, the wheel scrolls,
keys and paste are forwarded. The pane size is sent as the browser's
viewport, so a 390 px pane shows the site at 390 px. The Browser button in
the pane head (desktop) and the Embedded browser row in the tab menu
(phone) toggle it per device; when an agent opens a browser the pane shows
by itself unless the user hid it for that tab.

Settings: `web.embedded_browser` (toggle, default off),
`web.embedded_browser_max_pages` (pages awake over all tabs, default 40),
`web.embedded_browser_sleep_minutes` (default 30, 0 never) and
`web.embedded_browser_bin` (program, empty finds Chrome, Chromium, Edge or
Brave). The old `web.embedded_browser_max` (browsers at once) is gone from
the settings list; a config file that still has it loads fine and the key
is ignored.

## Not done or not verified

- BrowserSkill driving this browser (see above).
- The TUI has no pane for it; settings and the agent tool work, the view is
  the web app only.
- Not run against the real GodTerm app and a real phone; the web pane was
  checked against the mock server (`web/dev/mock.py`, `web/dev/ebv.mjs`,
  WebKit at 1280 and 390 px), and the Rust side against a real headless
  Chrome in a test. The mock renders its pictures with a headless Chrome
  whose minimum window width is about 500 px, so the 390 px mock picture
  looks cropped; the real path sets the viewport itself.
- Page tabs were checked against a real headless Chrome (target=_blank,
  window.open, a refused scheme, the cap, closing the last tab) and the web
  strip against the mock at 1280 and 390 px; not run against a real phone.
- The shared browser was checked against a real headless Chrome
  (`src/embedded_shared_tests.rs`: two owners in one Chrome each see only
  their pages and get their own pictures at the same time, ownership
  refusals through the tool, the pane and the low level calls, popup
  ownership, closing one owner, the global limit, sleep and wake by agent
  and by pane with the same id, address and scroll position, `woke` once,
  the viewed page not sleeping, the idle exit with sleeping tabs and the
  restart, a killed Chrome). Not run inside the real GodTerm app.
- A renderer crash of a single page is not handled specially (the page
  stays listed until it is closed or navigated).
- Isolated cookies per owner, and `--remote-debugging-pipe` (see Design).
- Nothing is put to sleep early under memory pressure; only the idle
  window and the global limit apply.
- Linux and Windows paths for finding the browser are written but were not
  run here.

## Open the page in Chrome or Safari

Two buttons in the address bar row open the page now shown in the user's
own browser: a Chrome button (a dropdown of the Chrome profiles by name,
with the account label when Chrome records one; one profile or none opens
directly) and a Safari button (macOS only, opens directly). Each is hidden
when that browser is not installed (macOS: Google Chrome.app or Safari.app
in /Applications or ~/Applications; Linux and Windows: best effort from
Chrome's data folder or the usual program names on PATH).

- Web frame `{t: "oext", op: "list"}` answers `chrome`, `safari` and
  `profiles` (`dir`, `name`, `account`); `{t: "oext", op: "open", browser,
  profile, url}` answers `ok` and a `message` or `error`. Code: `src/open_ext.rs`
  and `web/js/ebv_open.js`.
- Profile names come from the existing profile listing used by the tab
  Browser feature (the bsk snapshot, else Chrome's `Local State` names).
  Nothing else in a profile is read.
- The address must be http or https (the pane's `check_url` policy, and
  `about:blank` is refused). The program runs with an argv array, never a
  shell string: `open -na "Google Chrome" --args --profile-directory=<dir>
  <url>` and `open -a Safari <url>` on macOS.
- User only: it is a web UI message, not an agent tool. It opens on the
  computer running GodTerm, so from a phone over Tailscale the page opens
  on that computer; the tooltip and the dropdown footer say so.
- The launcher is injectable (`open_with`), so the unit tests in
  `src/open_ext_tests.rs` never start a browser. The mock answers the same
  frames (`web/dev/mock_ebv.py`) and `web/dev/ebv.mjs` checks the dropdown
  at desktop and phone width.

## Context menu on the live view

A right click (desktop) or a long press (touch, 500 ms, cancelled by moving more than 8 px) on the live view opens a small menu next to the pointer. The page's own native menu never shows, and a right click is not forwarded to the page (so a page's custom context menu does not fire and the selection stays). On macOS a ctrl+click counts as a right click.

- Hit test: the screencast only gives viewport coordinates, so the web app sends `{t: "ebv", op: "hit", fx, fy}` (fractions of the picture, like mouse ops). The Rust side runs one `Runtime.evaluate` (`src/embedded_hit.rs`) that takes `document.elementFromPoint`, descends through open shadow roots and same-origin iframes (a cross-origin frame is reported as `frame: true`, nothing inside it), and walks up for a link (`a` or `area` with `href`, made absolute), an `img`, whether the target is an editable field (input, textarea, contenteditable; `readonly`, `password`), and the selected text when it lies under the pointer (or the selected range of the field). The answer is parsed and sanitised by `parse_hit`: texts are cut, a password field never returns its selection, `data:` and `blob:` image addresses and very long addresses are dropped.
- URL policy: `open_policy` accepts only http and https and then runs `check_url`. Other schemes (mailto, tel, javascript, file, view-source) are reported with `open: false` and a reason; the menu shows them as a disabled line with that reason and offers only Copy link address. Every open (same tab, new tab, Chrome, Safari) goes through that policy, the Chrome and Safari ones again in `src/open_ext.rs`.
- Edits: `{op: "edit", what: selectall | delete | paste | focus, fx, fy, text?}` focuses the field under that point first (the right click did not), then selects all, deletes the selection (Cut) or inserts text (Paste). Only editable fields are accepted.
- Source: `{op: "source"}` answers the page's doctype and `outerHTML` (cut at 120000 characters). The web app shows it read only in a panel inside the browser pane (Copy and Close; Escape closes). `view-source:` stays blocked.
- These are web UI ops only. None of this is an agent tool, and agents cannot pick the user's browser.
- Items, shown only where they apply and grouped by separators: link (Open link, Open link in new tab, Copy link address, Open link in Chrome, Open link in Safari), image (Open image in new tab, Copy image address), selection or field (Cut, Copy, Paste, Select all, Search the web for "...", which opens a plain `https://duckduckgo.com/?q=...` in a new tab), and always the page (Back, Forward, Reload, Copy page address, Open page in Chrome or Safari, View page source). Chrome with several profiles opens a list inside the same menu (Back row, ArrowLeft returns); one profile opens directly. New tab items are disabled at 6 tabs.
- Copy uses the Clipboard API from the click handler, then `execCommand('copy')`, and if both are refused shows a selectable text box. Paste is offered only when the page is a secure context with `navigator.clipboard.readText` and the permission is not denied; if a read is refused the item disappears afterwards. No dead item is shown.
- Speed: one CDP round trip. The menu opens when the answer arrives; if that takes longer than 150 ms the page items show at once and the link, image and field items are filled in on arrival.
- The popup is the dropdown component of `web/js/ebv_open.js` (`popMenu`); the items are built in `web/js/ebv_menu.js`. Keyboard: arrows, Home, End, Enter, Escape (focus returns to the view), Tab or an outside click closes. It is clamped inside the pane (or the screen when the pane is smaller) and rows are finger sized at phone width.
- Tests: unit tests for the parsing and policy and a real headless Chrome test (`src/embedded_hit_tests.rs`: link, mailto, data image, selection, field, password, shadow root, iframe, select all, paste, cut, source); the mock (`web/dev/mock_ebv.py`, `/dev/ebv-hit?kind=`) and `web/dev/ebv.mjs` check the menu on a link, image, selection, field and page, the Chrome profile list, copy and its fallback, source, keyboard, Escape, clamping, and a long press at 390 px.
- Not verified: a real phone's long press (iOS has no native contextmenu for this; the 500 ms timer is used), Safari's real clipboard permission prompts, and pages with cross-origin iframes (nothing inside them is described).
