#!/usr/bin/env bash
# Version-less copies of every download in dist/, so links of the form
# https://github.com/daniel-farina/godterm/releases/latest/download/<name>
# keep working across releases (the site and install.sh use them).
#   GodTerm-0.2.1-macos-universal.dmg    -> GodTerm-macos-universal.dmg
#   godterm-0.2.1-macos-arm64.zip        -> godterm-macos-arm64.zip
#   godterm_0.2.1_amd64.deb              -> godterm_amd64.deb
#   godterm-0.2.1-1.x86_64.rpm           -> godterm-x86_64.rpm
#   GodTerm-0.2.1-x86_64.AppImage        -> GodTerm-x86_64.AppImage
#   godterm-0.2.1-<target>.tar.gz / .zip -> godterm-<target>.tar.gz / .zip
#   GodTerm-0.2.1-windows-x64-setup.exe  -> GodTerm-windows-x64-setup.exe
# SHA256SUMS (made after this) lists both names.
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
cd "$DIST"
v="$VERSION"
n=0
for f in *"$v"*; do
  [[ -f "$f" ]] || continue
  case "$f" in
    *.dmg | *.zip | *.tar.gz | *.deb | *.rpm | *.AppImage | *.exe) ;;
    *) continue ;;
  esac
  a="$f"
  a="${a/-$v-1./-}"   # rpm: godterm-0.2.1-1.x86_64.rpm -> godterm-x86_64.rpm
  a="${a/_${v}_/_}"   # deb: godterm_0.2.1_amd64.deb -> godterm_amd64.deb
  a="${a/-$v-/-}"     # the rest
  a="${a/-$v./.}"     # GodTerm-0.2.1.x (not used today, for safety)
  [[ "$a" != "$f" ]] || continue
  cp -f "$f" "$a"
  echo "$f -> $a"
  n=$((n + 1))
done
(( n > 0 )) || die "no versioned artifacts in $DIST"
