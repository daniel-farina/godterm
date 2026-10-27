#!/usr/bin/env bash
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
