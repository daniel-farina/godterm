# Releasing GodTerm

Every release starts as a DRAFT GitHub release on `daniel-farina/godterm`;
nothing is public until a maintainer publishes the draft.

## Artifacts

| File | What |
|---|---|
| `GodTerm-<v>-macos-{universal,arm64,x86_64}.dmg` | `GodTerm.app`, Developer ID signed, notarized and stapled, in a signed, notarized and stapled DMG (universal runs everywhere; arm64 for Apple silicon, x86_64 for Intel) |
| `godterm-<v>-macos-{universal,arm64,x86_64}.zip` | the `godterm` CLI and `godterm-speech` helper (signed, notarized) for Homebrew and manual installs |
| `godterm-<v>-x86_64-unknown-linux-gnu.tar.gz`, `...aarch64...` | Linux binaries with README, LICENSE, CHANGELOG, desktop file, icon |
| `godterm_<v>_amd64.deb`, `godterm_<v>_arm64.deb` | Debian and Ubuntu packages |
| `godterm-<v>-1.x86_64.rpm`, `godterm-<v>-1.aarch64.rpm` | Fedora, RHEL, openSUSE packages |
| `GodTerm-<v>-x86_64.AppImage`, `...aarch64.AppImage` | one file for any glibc 2.34+ distro (bundles libasound) |
| `godterm-<v>-x86_64-pc-windows-msvc.zip` | portable Windows build (godterm.exe, docs) |
| `GodTerm-<v>-windows-x64-setup.exe` | per user Windows installer (Inno Setup): Start menu entry, optional PATH and desktop icon, clean uninstall. Unsigned (no Authenticode certificate yet) |
| `SHA256SUMS` (+ `SHA256SUMS.minisig` or `.asc` when a key is set up) | checksums of everything above |

## Repositories

- `daniel-farina/godterm` (public): one squashed history. Never push local
  branches to it directly.
- `daniel-farina/godterm-private` (private): the full development history,
  the local `private` remote; `master` pushes to its `main`.

Publish the current `master` tree to the public repo as one new commit
(it refuses if the change contains secrets, personal emails, paths or
hosts; extra patterns in `~/.config/godterm/public-deny.txt`):

```sh
scripts/release/publish-public.sh "What changed"
```

Tag releases on the public commit (`git tag -a vX.Y.Z public-main`).

## Version

1. Set `version` in `Cargo.toml` (one place; the app's Info.plist, the
   packages and `godterm --version` all read it).
2. Move the `[Unreleased]` notes in `CHANGELOG.md` under a new
   `## [x.y.z] - YYYY-MM-DD` heading. The release notes are cut from that
   section (`scripts/release/notes.sh x.y.z`); a pre release tag such as
   `v0.2.0-rc1` falls back to the `0.2.0` section.
3. Commit, then tag: `git tag -a v0.2.0 -m "GodTerm 0.2.0"`.

## Local release

Prerequisites, once:

- A "Developer ID Application" identity in the login keychain
  (`security find-identity -v -p codesigning`), named in
  `~/.config/godterm/release.env` as `SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"`.
- Notary credentials, one of:
  - `NOTARY_PROFILE=<name>` for a `xcrun notarytool store-credentials` profile, or
  - an App Store Connect API key: `~/.config/godterm/notary.env` (mode 600)
    with `APPLE_API_KEY=<path to AuthKey_XXXX.p8>`, `APPLE_API_KEY_ID=`,
    `APPLE_API_ISSUER=`.
- Xcode 26 (for the `godterm-speech` helper), Rust with
  `aarch64-apple-darwin` and `x86_64-apple-darwin`, and Docker running (Linux
  builds happen in `packaging/linux/Dockerfile`).

Then:

```sh
REF=v0.2.0 scripts/release/all.sh            # everything into dist/
DRAFT=1 REF=v0.2.0 scripts/release/all.sh    # and a draft GitHub release
ONLY=macos REF=HEAD scripts/release/all.sh   # one platform
SKIP_NOTARIZE=1 scripts/release/macos.sh     # quick signing check, this tree
```

`all.sh` builds from a clean `git worktree` of `REF`, so uncommitted work
in the checkout never lands in a release. Release builds use
`target-release-eng/`, never `target/`, so `target/release/godterm` (which
a dev `godterm install` points at) is left alone. Expect about 10 minutes
of builds and 5 to 30 minutes of Apple notarization (three submissions: the
app, the DMG, the CLI zip).

Verify (the script runs these and fails on any error):

```sh
codesign --verify --deep --strict --verbose=2 dist/stage/macos/GodTerm.app
spctl -a -vvv -t exec dist/stage/macos/GodTerm.app        # source=Notarized Developer ID
spctl -a -vvv -t open --context context:primary-signature dist/GodTerm-*.dmg
xcrun stapler validate dist/GodTerm-*.dmg
shasum -a 256 -c dist/SHA256SUMS
```

## Build machines (how each platform is built for a local release)

| Platform | Where | How |
|---|---|---|
| macOS universal, arm64, x86_64 | a Mac | `scripts/release/macos.sh` (x86_64 cross compiles; checked under Rosetta with `arch -x86_64`) |
| Linux x86_64, aarch64 | GitHub runners (`build-linux.yml`, `LINUX_FROM_CI=1`) or local Docker | `scripts/release/linux.sh` in `packaging/linux/Dockerfile` (Ubuntu 22.04). Use the runners when local Docker cannot emulate the other architecture |
| Windows x86_64 | a Windows machine reachable over ssh (OpenSSH server, Rust MSVC, Inno Setup 6), set as `WIN_HOST`, `WIN_USER` and `WIN_SSH_KEY` in `~/.config/godterm/release.env` | `scripts/release/windows-remote.sh` copies the tree, runs `scripts/release/windows.ps1` (cargo + Inno Setup 6) and pulls the zip and installer back. Remote PowerShell goes as `-EncodedCommand`, which survives ssh quoting |

`scripts/release/all.sh` runs all three at once: Linux on the runners and
Windows on the Windows machine build while macOS builds and notarizes.

Smoke tests:

- Windows: `venv/bin/python scripts/release/smoke_windows.py dist/GodTerm-<v>-windows-x64-setup.exe`
  installs silently on the Windows machine, runs the installed godterm.exe in a ConPTY
  (the console layer Windows Terminal uses) over `ssh -tt` against a fake
  claude, opens a tab, quits (exit 0) and uninstalls. Needs `pip install pyte`.
- Linux: the CI `linux-package` job installs the .deb on Ubuntu and runs it;
  `docker run arm64v8/ubuntu:22.04` / `arm64v8/fedora:40` for the arm64 .deb and .rpm.
- macOS: `macos.sh` runs each variant's binary (x86_64 under Rosetta) and
  checks spctl and stapler.

Windows signing: there is no Authenticode certificate yet, so the zip and
installer ship unsigned (SmartScreen warns on first run). With a PFX, set
`WINDOWS_PFX` and `WINDOWS_PFX_PASSWORD` on the Windows machine and
`windows.ps1` signs godterm.exe and the installer with signtool (SHA-256,
RFC 3161 timestamp). An EV certificate (or Azure Trusted Signing) avoids
SmartScreen warnings from day one.

## CI release (GitHub Actions)

Pushing a `v*` tag runs `.github/workflows/release.yml`: macOS (signed and
notarized), Linux x86_64 and aarch64, then a DRAFT release with SHA256SUMS
and notes from CHANGELOG.md. `workflow_dispatch` with a tag rebuilds an
existing tag.

Repository secrets (Settings > Secrets and variables > Actions):

| Secret | Value |
|---|---|
| `MACOS_CERT_P12_BASE64` | the Developer ID Application certificate with its private key, exported from Keychain Access as .p12, then `base64 -i cert.p12 \| pbcopy` |
| `MACOS_CERT_PASSWORD` | the password chosen for that .p12 export |
| `MACOS_SIGN_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_API_KEY_P8` | the contents of `AuthKey_<id>.p8` (App Store Connect > Users and Access > Integrations > Keys; Developer role is enough) |
| `APPLE_API_KEY_ID` | the key id |
| `APPLE_API_ISSUER` | the issuer id (UUID) shown on that page |
| `MINISIGN_SECRET_KEY`, `MINISIGN_PASSWORD` | optional: sign SHA256SUMS |

Add them with `gh secret set NAME --repo daniel-farina/godterm` (it reads the
value from stdin, so nothing lands in shell history):

```sh
base64 -i DeveloperID.p12 | gh secret set MACOS_CERT_P12_BASE64 --repo daniel-farina/godterm
gh secret set MACOS_CERT_PASSWORD --repo daniel-farina/godterm
gh secret set APPLE_API_KEY_P8 --repo daniel-farina/godterm < ~/.appstoreconnect/private_keys/AuthKey_XXXX.p8
```

The workflow imports the certificate into a temporary keychain that is
deleted at the end of the job, and writes the API key to the runner's temp
dir with mode 600. Linux aarch64 builds run on GitHub's `ubuntu-24.04-arm`
runners; if those are not available to the account, change that job to
`ubuntu-latest` with `docker/setup-qemu-action` (slower).

## Homebrew

`daniel-farina/homebrew-godterm` holds `Formula/godterm.rb` (CLI) and
`Casks/godterm.rb` (app). After a release is published:

```sh
scripts/release/homebrew.sh v0.2.0    # rewrites both from the release's assets and pushes the tap
```

Users install with:

```sh
brew install --cask daniel-farina/godterm/godterm   # GodTerm.app
brew install daniel-farina/godterm/godterm          # the godterm command
```

## Publishing

1. Download the draft's artifacts on a second Mac (or a VM) and check the
   DMG opens with no Gatekeeper warning, `godterm --version` prints the
   tag's commit, and the .deb installs on Ubuntu 22.04 and 24.04.
2. Publish the draft (`gh release edit vX --draft=false`).
3. Run `scripts/release/homebrew.sh vX`.
4. Add a fresh `## [Unreleased]` heading to CHANGELOG.md.

## Release signing key (minisign)

Every release from v0.2.2 carries `SHA256SUMS.minisig`, signed with the GodTerm release
key; installed copies verify it against the public key built into the app
(`RWTNzCFSrTk4Erhp4n8mN0kO3VgXNUYQliJVGM6jC0jaVe07tPVATFcT`).

- The secret key lives on the release machine at `~/.config/godterm-release/minisign.key`
  (outside the repo, mode 0600).
- **For now, a backup of the key is stored in a separate private repository** (owner
  only). It is not in this repository and must never be committed here. If the key is
  lost, future updates cannot be signed with the key existing installs trust.
