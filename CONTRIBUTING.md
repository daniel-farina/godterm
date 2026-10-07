# Contributing

Thanks for helping with GodTerm. Issues and pull requests are welcome at
https://github.com/daniel-farina/godterm. For anything large, open an issue
first so we can agree on the shape before you write the code.

## Build and test

```sh
cargo build                 # debug
cargo test                  # unit tests and PTY integration tests
cargo build --release       # what `godterm install` links to
```

Linux needs `libasound2-dev` and `pkg-config` (and `libssl-dev` for the
onnxruntime download at build time).

## Conventions

- Commits: one change each, an imperative summary line ("Voice: ..."), no
  generated attribution trailers.
- Code that only works on macOS must fail softly elsewhere (check for the
  tool, or `cfg!(target_os = "macos")`), so the Linux build keeps working.
  See `packaging/PORTING.md`.
- Never log tokens, OAuth credentials or file contents from account
  folders. See `SECURITY.md`.
- Add a line under `## [Unreleased]` in `CHANGELOG.md` for user visible
  changes.

## Releases

See `docs/RELEASING.md`.
