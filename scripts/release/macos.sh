#!/usr/bin/env bash
# macOS release: GodTerm.app and the godterm CLI for three variants,
# universal, arm64 (Apple silicon) and x86_64 (Intel), each signed with the
# Developer ID Application identity (hardened runtime, timestamp), notarized
# and stapled, in a signed DMG, plus a notarized CLI zip (for Homebrew).
# Idempotent: every run starts from a clean stage.
#
#   scripts/release/macos.sh                       build, sign, notarize, verify
#   SKIP_NOTARIZE=1 scripts/release/macos.sh       sign only (fast local check)
#   SKIP_BUILD=1 ...                               reuse the last cargo builds
#   MAC_VARIANTS="universal" ...                   fewer variants
#
# Notarization is submitted in two parallel batches (all apps, then all DMGs
# and zips) so three variants cost about as much wall time as one.
#
# Environment:
#   SIGN_IDENTITY   codesign identity ("Developer ID Application: ...")
#   KEYCHAIN        keychain holding it (CI uses a temporary one)
#   NOTARY_PROFILE or APPLE_API_KEY/_ID/_ISSUER, see common.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

[[ "$(uname)" == Darwin ]] || die "macos.sh runs on macOS"

[[ -n "${SIGN_IDENTITY:-}" ]] || die "set SIGN_IDENTITY (a Developer ID Application identity) in ~/.config/godterm/release.env or the environment"
BUNDLE_ID="${BUNDLE_ID:-dev.godterm.launcher}"
MIN_MACOS="${MIN_MACOS:-12.0}"
read -r -a VARIANTS <<< "${MAC_VARIANTS:-universal arm64 x86_64}"
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
PKG="$ROOT/packaging/macos"
OUT="$STAGE/macos"

keychain_args=()
[[ -n "${KEYCHAIN:-}" ]] && keychain_args=(--keychain "$KEYCHAIN")

security find-identity -v -p codesigning ${KEYCHAIN:+"$KEYCHAIN"} | grep -qF "\"$SIGN_IDENTITY\"" \
  || die "signing identity not found: $SIGN_IDENTITY"

rm -rf "$OUT"
for v in "${VARIANTS[@]}"; do rm -f "$DIST/GodTerm-$VERSION-macos-$v.dmg" "$DIST/godterm-$VERSION-macos-$v.zip"; done
mkdir -p "$OUT/bin"

# 1. Rust for both architectures.
if [[ -z "${SKIP_BUILD:-}" ]]; then
  J="$(njobs)"
  for t in "${TARGETS[@]}"; do
    log "cargo build --profile $PROFILE --target $t (-j $J)"
    MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS" cargo build --locked --profile "$PROFILE" --target "$t" -j "$J"
  done
fi
cp "$CARGO_TARGET_DIR/aarch64-apple-darwin/$PROFILE/godterm" "$OUT/bin/godterm-arm64"
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/$PROFILE/godterm" "$OUT/bin/godterm-x86_64"
lipo -create -output "$OUT/bin/godterm-universal" "$OUT/bin/godterm-arm64" "$OUT/bin/godterm-x86_64"

# 2. The Apple speech helper (SpeechAnalyzer needs macOS 26 at run time; the
#    app checks before using it), both architectures.
SPEECH_SRC="$ROOT/helpers/godterm-speech/main.swift"
HAVE_SPEECH=""
if [[ -f "$SPEECH_SRC" ]]; then
  HAVE_SPEECH=1
  for a in arm64 x86_64; do
    log "swiftc godterm-speech ($a)"
    xcrun swiftc -O -target "$a-apple-macos26.0" "$SPEECH_SRC" -o "$OUT/bin/godterm-speech-$a" \
      -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist \
      -Xlinker "$ROOT/helpers/godterm-speech/Info.plist"
  done
  lipo -create -output "$OUT/bin/godterm-speech-universal" "$OUT/bin/godterm-speech-arm64" "$OUT/bin/godterm-speech-x86_64"
fi

# 3. The launcher (the app's Mach-O main executable), per variant.
log "clang launcher"
for v in "${VARIANTS[@]}"; do
  case "$v" in
    universal) archs=(-arch arm64 -arch x86_64) ;;
    *) archs=(-arch "$v") ;;
  esac
  xcrun clang -O2 -Wall "${archs[@]}" -mmacosx-version-min="$MIN_MACOS" "$PKG/launcher.c" -o "$OUT/bin/launcher-$v"
done

sign() { # sign <path> <identifier> [entitlements]
  local ent=()
  [[ -n "${3:-}" ]] && ent=(--entitlements "$3")
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" \
    ${keychain_args[@]+"${keychain_args[@]}"} --identifier "$2" ${ent[@]+"${ent[@]}"} "$1"
}

# 4. Bundles and CLI folders, signed inside out.
for v in "${VARIANTS[@]}"; do
  log "assembling GodTerm.app $VERSION ($BUILD_NUMBER) for $v"
  APP="$OUT/$v/GodTerm.app"
  C="$APP/Contents"
  mkdir -p "$C/MacOS" "$C/Resources"
  cp "$OUT/bin/launcher-$v" "$C/MacOS/GodTerm"
  cp "$OUT/bin/godterm-$v" "$C/MacOS/godterm"
  [[ -n "$HAVE_SPEECH" ]] && cp "$OUT/bin/godterm-speech-$v" "$C/MacOS/godterm-speech"
  cp "$ROOT/assets/godterm.icns" "$C/Resources/godterm.icns"
  cp "$PKG/launch.sh" "$PKG/run.sh" "$C/Resources/"
  chmod 755 "$C/Resources/launch.sh" "$C/Resources/run.sh"
  sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$BUILD_NUMBER/g" \
      -e "s/@BUNDLE_ID@/$BUNDLE_ID/g" -e "s/@MIN_MACOS@/$MIN_MACOS/g" \
      "$PKG/Info.plist.in" > "$C/Info.plist"
  plutil -lint "$C/Info.plist" >/dev/null
  printf 'APPL????' > "$C/PkgInfo"
  xattr -cr "$APP"
  [[ -n "$HAVE_SPEECH" ]] && sign "$C/MacOS/godterm-speech" dev.godterm.speech "$PKG/speech.entitlements"
  sign "$C/MacOS/godterm" dev.godterm.cli "$PKG/cli.entitlements"
  sign "$APP" "$BUNDLE_ID" "$PKG/app.entitlements"
  codesign --verify --deep --strict "$APP"

  CLIDIR="$OUT/$v/godterm-$VERSION-macos-$v"
  mkdir -p "$CLIDIR"
  cp "$OUT/bin/godterm-$v" "$CLIDIR/godterm"
  [[ -n "$HAVE_SPEECH" ]] && cp "$OUT/bin/godterm-speech-$v" "$CLIDIR/godterm-speech"
  [[ -n "$HAVE_SPEECH" ]] && sign "$CLIDIR/godterm-speech" dev.godterm.speech "$PKG/speech.entitlements"
  sign "$CLIDIR/godterm" dev.godterm.cli "$PKG/cli.entitlements"
  cp "$ROOT/README.md" "$ROOT/LICENSE" "$ROOT/CHANGELOG.md" "$CLIDIR/"
done

# Submit every file, then wait for every one. Apple's queue can take well
# over an hour; a timeout does not cancel a submission (check it later with
# `xcrun notarytool info <id>`; ids are printed right away).
notarize_all() { # notarize_all <file>...
  local f json id ids=() status failed=""
  for f in "$@"; do
    json="$(xcrun notarytool submit "$f" "${NOTARY_AUTH[@]}" --output-format json)"
    id="$(printf '%s' "$json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')"
    [[ -n "$id" ]] || die "notarytool gave no submission id for $(basename "$f")"
    echo "submitted $(basename "$f"): $id"
    ids+=("$id")
  done
  log "waiting for ${#ids[@]} notarizations (up to ${NOTARY_TIMEOUT:-3h})"
  local i=0
  for id in "${ids[@]}"; do
    f="${*:$((i + 1)):1}"; i=$((i + 1))
    xcrun notarytool wait "$id" "${NOTARY_AUTH[@]}" --timeout "${NOTARY_TIMEOUT:-3h}" >/dev/null || true
    json="$(xcrun notarytool info "$id" "${NOTARY_AUTH[@]}" --output-format json || true)"
    status="$(printf '%s' "$json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))' 2>/dev/null || true)"
    echo "notarytool: ${status:-no status} $(basename "$f") (submission $id)"
    if [[ "$status" != Accepted ]]; then
      xcrun notarytool log "$id" "${NOTARY_AUTH[@]}" || true
      failed+=" $(basename "$f")"
    fi
  done
  [[ -z "$failed" ]] || die "notarization failed for:$failed"
}

NOTARY_AUTH=()
if [[ -z "${SKIP_NOTARIZE:-}" ]]; then
  notary_args || die "no notary credentials: set NOTARY_PROFILE, or APPLE_API_KEY/APPLE_API_KEY_ID/APPLE_API_ISSUER, or write ~/.config/godterm/notary.env"
  # 5. Notarize and staple the apps themselves, so a copy dragged out of a
  #    DMG passes Gatekeeper offline.
  zips=()
  for v in "${VARIANTS[@]}"; do
    ditto -c -k --keepParent "$OUT/$v/GodTerm.app" "$OUT/$v/GodTerm-notarize.zip"
    zips+=("$OUT/$v/GodTerm-notarize.zip")
  done
  notarize_all "${zips[@]}"
  for v in "${VARIANTS[@]}"; do
    xcrun stapler staple -q "$OUT/$v/GodTerm.app"
    rm -f "$OUT/$v/GodTerm-notarize.zip"
  done
fi

# 6. DMGs (the app, an Applications link, a volume icon) and CLI zips.
for v in "${VARIANTS[@]}"; do
  DMG="$DIST/GodTerm-$VERSION-macos-$v.dmg"
  log "hdiutil $(basename "$DMG")"
  DMGROOT="$OUT/$v/dmg"
  mkdir -p "$DMGROOT"
  ditto "$OUT/$v/GodTerm.app" "$DMGROOT/GodTerm.app"
  ln -s /Applications "$DMGROOT/Applications"
  cp "$ROOT/assets/godterm.icns" "$DMGROOT/.VolumeIcon.icns"
  hdiutil create -quiet -volname "GodTerm $VERSION" -srcfolder "$DMGROOT" -fs HFS+ -format UDRW -ov "$OUT/$v/rw.dmg"
  MNT="$(mktemp -d)"
  hdiutil attach -quiet -nobrowse -noautoopen -mountpoint "$MNT" "$OUT/$v/rw.dmg"
  SetFile -a C "$MNT" 2>/dev/null || true
  hdiutil detach -quiet "$MNT"
  hdiutil convert -quiet "$OUT/$v/rw.dmg" -format UDZO -imagekey zlib-level=9 -o "$DMG"
  rm -f "$OUT/$v/rw.dmg"
  codesign --force --timestamp --sign "$SIGN_IDENTITY" ${keychain_args[@]+"${keychain_args[@]}"} --identifier "$BUNDLE_ID.dmg" "$DMG"

  CLIZIP="$DIST/godterm-$VERSION-macos-$v.zip"
  (cd "$OUT/$v" && ditto -c -k --keepParent "godterm-$VERSION-macos-$v" "$CLIZIP")
done

if [[ -z "${SKIP_NOTARIZE:-}" ]]; then
  files=()
  for v in "${VARIANTS[@]}"; do files+=("$DIST/GodTerm-$VERSION-macos-$v.dmg" "$DIST/godterm-$VERSION-macos-$v.zip"); done
  notarize_all "${files[@]}"
  for v in "${VARIANTS[@]}"; do xcrun stapler staple -q "$DIST/GodTerm-$VERSION-macos-$v.dmg"; done
fi

# 7. Verify every variant; run each binary on this Mac (x86_64 under Rosetta).
log "verify"
for v in "${VARIANTS[@]}"; do
  APP="$OUT/$v/GodTerm.app"
  DMG="$DIST/GodTerm-$VERSION-macos-$v.dmg"
  echo "--- $v"
  codesign --verify --deep --strict "$APP" && echo "codesign app: ok"
  codesign --verify --strict "$DMG" && echo "codesign dmg: ok"
  codesign --verify --strict "$OUT/$v/godterm-$VERSION-macos-$v/godterm" && echo "codesign cli: ok"
  echo "archs: $(lipo -archs "$APP/Contents/MacOS/godterm")"
  case "$v" in
    x86_64) arch -x86_64 "$APP/Contents/MacOS/godterm" --version ;;
    arm64) arch -arm64 "$APP/Contents/MacOS/godterm" --version ;;
    *) "$APP/Contents/MacOS/godterm" --version; arch -x86_64 "$APP/Contents/MacOS/godterm" --version ;;
  esac
  if [[ -z "${SKIP_NOTARIZE:-}" ]]; then
    spctl -a -vvv -t exec "$APP" 2>&1 | sed 's/^/spctl app: /'
    spctl -a -vvv -t open --context context:primary-signature "$DMG" 2>&1 | sed 's/^/spctl dmg: /'
    xcrun stapler validate -q "$APP" && echo "stapler app: ok"
    xcrun stapler validate -q "$DMG" && echo "stapler dmg: ok"
  fi
done
ls -lh "$DIST"/*macos*
log "macOS done"
