#!/usr/bin/env bash
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"
cd "$DIST"
files=()
for f in *.dmg *.zip *.tar.gz *.deb *.rpm *.AppImage *.exe; do [[ -f "$f" ]] && files+=("$f"); done
(( ${#files[@]} )) || die "no artifacts in $DIST"
sha256 "${files[@]}" | sort -k2 > SHA256SUMS
cat SHA256SUMS
