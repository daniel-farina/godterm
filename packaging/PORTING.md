# Porting notes

Status of GodTerm on each platform, what is gated and how, and the exact
proposals for changes that are too large to make outside the feature work.

## macOS (supported, primary)

Universal (arm64 + x86_64), macOS 12 or newer. Apple on-device
recognition (`godterm-speech`) needs macOS 26 at run time and the macOS 26
SDK (Xcode 26) to build; the app checks before using it. The release ships
the helper signed inside `GodTerm.app/Contents/MacOS/`, and
`voice::apple_helper_path()` prefers that copy over `~/.godterm/bin`.

## Linux (supported: x86_64 and aarch64, glibc)

The crate compiles and links for Linux unchanged. Nothing needed `cfg`
gating: every macOS-only feature is a runtime call to a macOS tool that
fails softly when the tool is missing.

| Feature | macOS | Linux today |
|---|---|---|
| Credentials | keychain (`security`), then `<configDir>/.credentials.json` | `.credentials.json` only (`creds::load_creds` checks `cfg!(target_os = "macos")`), which is what claude itself uses on Linux |
| Voice capture | cpal on CoreAudio | cpal on ALSA (PulseAudio and PipeWire through their ALSA plugins). Needs `libasound2` |
| Speech to text | whisper-cli or Apple | whisper-cli (`whisper-cpp`) and `ffmpeg`; Apple engine unavailable |
| Text to speech | Kokoro (onnxruntime) or `say` | Kokoro works; `say` and `afplay` are missing (see proposal 1) |
| Notifications | `osascript` | none (see proposal 2) |
| Window placement, iTerm control | AppleScript | none; GodTerm runs in whatever terminal started it |
| `godterm install` | `GodTerm.app` + `~/.local/bin/godterm` | the app part is macOS-only; packages put `godterm` in `/usr/bin` |
| Open in Finder / `open` | `open` | `open` is missing (see proposal 3) |

Build notes:

- Builds run natively in `packaging/linux/Dockerfile` (Ubuntu 22.04). The
  binaries need glibc 2.34+, libstdc++6 and libasound2 (Ubuntu 22.04+,
  Debian 12+, Fedora 35+, RHEL 9+).
- Not zig cross linking: `ort` links a prebuilt onnxruntime that was built
  against GNU libstdc++; zig provides LLVM libc++, so the link fails
  (`std::condition_variable` and friends).
- Not musl: onnxruntime has no prebuilt static library for musl. A static
  musl build would need `ort` behind a cargo feature (proposal 4) or
  building onnxruntime from source.

### Proposal 1: Linux audio playback (small, builder's call)

`voice/tts.rs` and `voice/mod.rs` call `afplay` (and `say`). On Linux,
play through `paplay`, then `aplay`, then `ffplay -nodisp -autoexit`,
choosing the first one on `PATH`, and make `effective_engine` fall back
from `say` to Kokoro (or off) when `say` is missing. About 20 lines in one
helper:

```rust
/// The command that plays a WAV file on this machine.
pub fn wav_player() -> Option<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "macos") { return Some(("afplay", &[])); }
    for (bin, args) in [("paplay", &[][..]), ("aplay", &["-q"][..]), ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"][..])] {
        if which(bin) { return Some((bin, args)); }
    }
    None
}
```

### Proposal 2: Linux notifications

`notify.rs`: when not on macOS, run `notify-send <title> <body>` if it is
on `PATH`, else do nothing.

### Proposal 3: `open` on Linux

`control.rs` (open_path) and the folder actions: use `xdg-open` when not
on macOS.

### Proposal 4: a `voice` cargo feature (for musl and minimal builds)

Make `ort`, `ndarray` and `cpal` optional behind a default `voice`
feature, and gate `mod voice` and its callers with
`#[cfg(feature = "voice")]`, with stubs that report "voice is not built
into this binary". This touches `app_voice.rs`, `app_wake.rs`,
`app_openmic.rs`, `voice/*` and the settings screens, so it belongs with
the feature work. It would allow `x86_64-unknown-linux-musl` static
builds and much smaller binaries.

## Windows (supported: x86_64)

Windows 10 1803 or newer, built with the MSVC toolchain.

- Every Unix only call goes through `src/platform.rs` (permission bits are
  no ops, file locks are `std::fs::File` locks, process control uses
  OpenProcess / TerminateProcess, randomness the system RNG).
- The control socket is AF_UNIX on Windows too (the `uds_windows` crate).
- Panes run in ConPTY through portable-pty.
- Graceful stops attach to the agent's console and send Ctrl-C, then
  Ctrl-Break; without a console the stop is a confirmed force stop
  (`src/procs.rs`).
- Credentials come from `<configDir>/.credentials.json`, as claude stores
  them on Windows.
- Not on Windows: Apple speech, the `say` and `afplay` fallbacks (TTS is
  Kokoro or Grok), the app bundle, window placement.
- Not built yet: aarch64-pc-windows-msvc.
