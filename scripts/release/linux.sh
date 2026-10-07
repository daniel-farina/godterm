#!/usr/bin/env bash
# Linux release: x86_64 and aarch64 (glibc) builds, packaged as tar.gz, .deb
# .rpm and AppImage. Each target builds natively inside the packaging/linux/Dockerfile
# image (Ubuntu 22.04), so the result is the same from a Mac or from CI.
# Idempotent.
#
#   scripts/release/linux.sh                 both architectures (Docker)
#   LINUX_TARGETS=aarch64-unknown-linux-gnu scripts/release/linux.sh
#   NATIVE=1 scripts/release/linux.sh        no Docker: build for this Linux
#                                            host's arch with the tools installed
#
# Why not zig cross linking: ort links a prebuilt onnxruntime built against
# GNU libstdc++, which zig's libc++ cannot satisfy. Why not musl: onnxruntime
# has no prebuilt static library for musl. See packaging/PORTING.md.
#
# Runtime needs: glibc 2.34+, libstdc++6, libasound2 (ALSA).
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

read -r -a TARGETS <<< "${LINUX_TARGETS:-x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu}"
IMAGE="${IMAGE:-godterm-build}"

arch_of() {
  case "$1" in
    x86_64-unknown-linux-gnu) echo "amd64 x86_64 amd64/ubuntu:22.04" ;;
    aarch64-unknown-linux-gnu) echo "arm64 aarch64 arm64v8/ubuntu:22.04" ;;
    *) die "unsupported target $1" ;;
  esac
}

# Build and package one target on this (Linux) machine.
inside() { # inside <target>
  local t="$1" arch rpmarch bin name d j
  read -r arch rpmarch _ <<< "$(arch_of "$t")"
  j="$(njobs)"
  log "cargo build --profile $PROFILE --target $t (-j $j)"
  cargo build --locked --profile "$PROFILE" --target "$t" -j "$j"
  bin="$CARGO_TARGET_DIR/$t/$PROFILE/godterm"
  "$bin" --version

  name="godterm-$VERSION-$t"
  local stage; stage="$(mktemp -d)"
  d="$stage/$name"
  mkdir -p "$d"
  cp "$bin" "$d/godterm"
  cp README.md LICENSE CHANGELOG.md packaging/linux/godterm.desktop "$d/"
  cp assets/icon_256.png "$d/godterm.png"
  tar -C "$stage" -czf "$DIST/$name.tar.gz" "$name"
  rm -rf "$stage"

  log "cargo deb ($arch)"
  cargo deb --no-build --profile "$PROFILE" --target "$t" \
    --deb-version "$VERSION" -o "$DIST/godterm_${VERSION}_${arch}.deb"
  log "cargo generate-rpm ($rpmarch)"
  # Requires by soname (distro neutral: Fedora, RHEL, openSUSE). The
  # automatic scan emits "libc.so.6(GLIBC_2.x)[WEAK]" on x86_64, which no
  # distro provides, so it is off.
  cargo generate-rpm --profile "$PROFILE" --target "$t" --target-dir "$CARGO_TARGET_DIR" \
    --arch "$rpmarch" --auto-req disabled \
    -s 'requires = { "libasound.so.2()(64bit)" = "*", "libstdc++.so.6()(64bit)" = "*", "libc.so.6(GLIBC_2.34)(64bit)" = "*" }' \
    -o "$DIST/godterm-$VERSION-1.$rpmarch.rpm"

  # AppImage: one file, chmod +x and run, on most distros. Bundles
  # libasound (absent on minimal systems); glibc and libstdc++ come from
  # the host, as usual for AppImages.
  if command -v appimagetool >/dev/null; then
    log "appimagetool ($rpmarch)"
    local appdir; appdir="$(mktemp -d)/GodTerm.AppDir"
    mkdir -p "$appdir/usr/bin" "$appdir/usr/lib" "$appdir/usr/share/icons/hicolor/256x256/apps"
    cp "$bin" "$appdir/usr/bin/godterm"
    cp -L "$(ldconfig -p | awk '/libasound\.so\.2 / {print $NF; exit}')" "$appdir/usr/lib/libasound.so.2"
    cp packaging/linux/godterm.desktop "$appdir/godterm.desktop"
    cp assets/icon_256.png "$appdir/godterm.png"
    cp assets/icon_256.png "$appdir/usr/share/icons/hicolor/256x256/apps/godterm.png"
    cat > "$appdir/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$HERE/usr/bin/godterm" "$@"
APPRUN
    chmod 755 "$appdir/AppRun"
    env -u SOURCE_DATE_EPOCH ARCH="$rpmarch" appimagetool --appimage-extract-and-run --no-appstream "$appdir" \
      "$DIST/GodTerm-$VERSION-$rpmarch.AppImage" >/dev/null
    rm -rf "$(dirname "$appdir")"
    "$DIST/GodTerm-$VERSION-$rpmarch.AppImage" --appimage-extract-and-run --version
  fi
}

if [[ "${1:-}" == --inside ]]; then
  inside "$2"
  # The container runs as root: give the packages back to the caller.
  [[ -n "${HOST_UID:-}" ]] && chown "$HOST_UID:${HOST_GID:-$HOST_UID}" "$DIST"/godterm* "$DIST"/GodTerm-*.AppImage 2>/dev/null || true
  exit 0
fi

if [[ -n "${NATIVE:-}" ]]; then
  [[ "$(uname)" == Linux ]] || die "NATIVE=1 needs a Linux host"
  inside "$(uname -m)-unknown-linux-gnu"
  exit 0
fi

command -v docker >/dev/null && docker info >/dev/null 2>&1 || die "Docker is not running (or use NATIVE=1 on Linux)"
mkdir -p "$CARGO_TARGET_DIR/linux-amd64" "$CARGO_TARGET_DIR/linux-arm64" "$CARGO_TARGET_DIR/linux-registry"
for t in "${TARGETS[@]}"; do
  read -r arch _ base <<< "$(arch_of "$t")"
  log "image $IMAGE:$arch (linux/$arch)"
  docker build --platform "linux/$arch" --build-arg "BASE=$base" -q -t "$IMAGE:$arch" \
    -f packaging/linux/Dockerfile packaging/linux >/dev/null
  [[ "$(docker image inspect "$IMAGE:$arch" --format '{{.Architecture}}')" == "$arch" ]] \
    || die "$IMAGE:$arch was built for the wrong architecture"
  log "building $t in linux/$arch"
  docker run --rm --platform "linux/$arch" \
    -v "$ROOT:/src" -v "$DIST:/dist" \
    -v "$CARGO_TARGET_DIR/linux-$arch:/target" \
    -v "$CARGO_TARGET_DIR/linux-registry:/usr/local/cargo/registry" \
    -e CARGO_TARGET_DIR=/target -e DIST=/dist -e PROFILE="$PROFILE" -e JOBS="${JOBS:-}" \
    -e GODTERM_GIT_SHA="$GODTERM_GIT_SHA" -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
    -e BUILD_NUMBER="$BUILD_NUMBER" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
    -w /src "$IMAGE:$arch" scripts/release/linux.sh --inside "$t"
done
ls -lh "$DIST"/godterm-*linux*.tar.gz "$DIST"/godterm_*.deb "$DIST"/godterm-*.rpm "$DIST"/GodTerm-*.AppImage
log "Linux done"
