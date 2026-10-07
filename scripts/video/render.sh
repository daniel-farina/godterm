#!/usr/bin/env bash
# Render the GodTerm promo video from the Remotion project (default ~/godterm-video).
#
# usage: scripts/video/render.sh [--music FILE | --no-music] [--only LIST] [--concurrency N]
#                                [--frames A-B] [--preview] [--project DIR]
#   --music FILE      use FILE as the music bed (converted to 48 kHz WAV, beats detected)
#   --no-music        render without music
#                     (default: assets/video/music.(mp3|wav|m4a) if present, else the generated placeholder pad)
#   --only LIST       comma list of: 4k,1080p,square,vertical,poster,srt (default: all)
#   --concurrency N   Remotion browser tabs (default: sized from free CPU and RAM, at most 10)
#   --frames A-B      render only a frame range (quick tests)
#   --preview         quick quarter-scale landscape preview only: out/godterm-demo-preview.mp4
#
# Outputs (~/godterm-video/out/, or $GODTERM_VIDEO_OUT): godterm-demo-4k.mp4, godterm-demo-1080p.mp4, godterm-demo-square.mp4,
#   godterm-demo-vertical.mp4, godterm-demo-poster.png, godterm-demo.srt
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROJ="${GODTERM_VIDEO_DIR:-$HOME/godterm-video}"
# Outside the repo and its release tree (dist/ is the release scripts' scratch).
OUT="${GODTERM_VIDEO_OUT:-$PROJ/out}"
MUSIC=""; NO_MUSIC=0; ONLY="4k,1080p,square,vertical,poster,srt"; CONC=""; FRAMES=""; PREVIEW=0

while [ $# -gt 0 ]; do
  case "$1" in
    --music) MUSIC="$2"; shift 2 ;;
    --no-music) NO_MUSIC=1; shift ;;
    --only) ONLY="$2"; shift 2 ;;
    --concurrency) CONC="$2"; shift 2 ;;
    --frames) FRAMES="$2"; shift 2 ;;
    --preview) PREVIEW=1; shift ;;
    --project) PROJ="$2"; shift 2 ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

want() { [[ ",$ONLY," == *",$1,"* ]]; }
log() { printf '\n== %s\n' "$*"; }
[ -d "$PROJ/node_modules" ] || { echo "Remotion project not found or not installed: $PROJ" >&2; exit 1; }
[ -f "$PROJ/public/edit.json" ] || { echo "missing $PROJ/public/edit.json" >&2; exit 1; }
[ -f "$PROJ/public/rec/term.mp4" ] || echo "warning: $PROJ/public/rec/term.mp4 is missing" >&2
mkdir -p "$OUT"

# concurrency from free resources: about 1.5 GB per 4K tab, keep 4 GB and a few cores spare
if [ -z "$CONC" ]; then
  ncpu=$(sysctl -n hw.ncpu 2>/dev/null || nproc)
  page=$(sysctl -n hw.pagesize 2>/dev/null || echo 16384)
  freep=$(vm_stat 2>/dev/null | awk '/Pages free|Pages inactive|Pages speculative/ {gsub("\\.","",$NF); s+=$NF} END {print s+0}')
  freegb=$(( freep * page / 1073741824 ))
  by_cpu=$(( ncpu > 4 ? ncpu - 4 : 2 ))
  by_mem=$(( freegb > 6 ? (freegb - 4) * 2 / 3 : 2 ))
  CONC=$(( by_cpu < by_mem ? by_cpu : by_mem )); [ "$CONC" -gt 10 ] && CONC=10; [ "$CONC" -lt 2 ] && CONC=2
  echo "cpus $ncpu, ~${freegb} GB free -> concurrency $CONC"
fi

# ---- music
cd "$PROJ"
if [ "$NO_MUSIC" = 1 ]; then
  MUSIC_FILE=null
else
  if [ -z "$MUSIC" ]; then
    for ext in mp3 wav m4a; do [ -f "$REPO/assets/video/music.$ext" ] && { MUSIC="$REPO/assets/video/music.$ext"; break; }; done
  fi
  if [ -n "$MUSIC" ]; then
    [ -f "$MUSIC" ] || { echo "music file not found: $MUSIC" >&2; exit 1; }
    log "music: $MUSIC"
    ffmpeg -loglevel error -y -i "$MUSIC" -vn -ar 48000 -ac 2 -c:a pcm_s16le public/audio/music.wav
    MUSIC_FILE='"audio/music.wav"'
  else
    log "music: generated placeholder pad"
    [ -f public/audio/placeholder-pad.wav ] || python3 scripts/make_pad.py public/audio/placeholder-pad.wav 150
    MUSIC_FILE='"audio/placeholder-pad.wav"'
  fi
fi
python3 - "$MUSIC_FILE" <<'PY'
import json, sys
f = json.loads(sys.argv[1]); p = "public/edit.json"
e = json.load(open(p)); e.setdefault("music", {})["file"] = f
json.dump(e, open(p, "w"), indent=2)
print("edit.json music.file =", f)
PY
if [ "$MUSIC_FILE" != null ]; then
  python3 scripts/beats.py "public/$(echo "$MUSIC_FILE" | tr -d '"')" public/audio/beats.json || echo "beat detection failed (non fatal)"
fi

# ---- bundle once (copies public/ into the bundle)
log "bundling"
rm -rf work/bundle
npx remotion bundle --out-dir work/bundle --log=error
BUNDLE="$PROJ/work/bundle"
FR=(); [ -n "$FRAMES" ] && FR=(--frames="$FRAMES")
RFLAGS=(--concurrency="$CONC" --codec=h264 --hardware-acceleration=if-possible --audio-codec=aac --audio-bitrate=320k)
timed() { local s=$SECONDS; "$@"; echo "   took $((SECONDS - s)) s"; }
# Final mix to -14 LUFS integrated (true peak -1 dBTP), two pass, linear;
# the video stream is copied untouched.
loud() {
  local f="$1" m
  m=$(ffmpeg -hide_banner -i "$f" -vn -af loudnorm=I=-14:TP=-1:LRA=11:print_format=json -f null - 2>&1 \
      | python3 -c 'import sys,json,re; t=sys.stdin.read(); j=json.loads(t[t.rindex("{"):t.rindex("}")+1]); print(":".join(f"{k}={j[v]}" for k,v in [("measured_I","input_i"),("measured_TP","input_tp"),("measured_LRA","input_lra"),("measured_thresh","input_thresh"),("offset","target_offset")]))')
  ffmpeg -loglevel error -y -i "$f" -c:v copy -af "loudnorm=I=-14:TP=-1:LRA=11:linear=true:$m,aresample=48000" \
    -c:a aac -b:a 320k -movflags +faststart "${f%.mp4}.loud.mp4" && mv "${f%.mp4}.loud.mp4" "$f"
  echo "   loudness: $(ffmpeg -hide_banner -i "$f" -af ebur128 -f null - 2>&1 | awk '/I:/{v=$2} END{print v}') LUFS"
}

if [ "$PREVIEW" = 1 ]; then
  log "preview (quarter scale)"
  timed npx remotion render "$BUNDLE" Promo "$OUT/godterm-demo-preview.mp4" "${RFLAGS[@]}" --scale=0.25 --video-bitrate=4M ${FR[@]+"${FR[@]}"}
  exit 0
fi

if want 4k; then
  log "4K landscape"
  timed npx remotion render "$BUNDLE" Promo "$OUT/godterm-demo-4k.mp4" "${RFLAGS[@]}" --video-bitrate=40M ${FR[@]+"${FR[@]}"}
  loud "$OUT/godterm-demo-4k.mp4"
fi
if want 1080p; then
  log "1080p"
  if [ -f "$OUT/godterm-demo-4k.mp4" ] && want 4k; then
    timed ffmpeg -loglevel error -y -i "$OUT/godterm-demo-4k.mp4" -vf scale=1920:1080:flags=lanczos \
      -c:v h264_videotoolbox -b:v 16M -profile:v high -pix_fmt yuv420p -c:a copy -movflags +faststart "$OUT/godterm-demo-1080p.mp4"
  else
    timed npx remotion render "$BUNDLE" Promo "$OUT/godterm-demo-1080p.mp4" "${RFLAGS[@]}" --scale=0.5 --video-bitrate=16M ${FR[@]+"${FR[@]}"}
  fi
fi
if want square; then
  log "square 1080x1080"
  timed npx remotion render "$BUNDLE" PromoSquare "$OUT/godterm-demo-square.mp4" "${RFLAGS[@]}" --video-bitrate=12M ${FR[@]+"${FR[@]}"}
  loud "$OUT/godterm-demo-square.mp4"
fi
if want vertical; then
  log "vertical 1080x1920"
  timed npx remotion render "$BUNDLE" PromoVertical "$OUT/godterm-demo-vertical.mp4" "${RFLAGS[@]}" --video-bitrate=16M ${FR[@]+"${FR[@]}"}
  loud "$OUT/godterm-demo-vertical.mp4"
fi
if want poster; then
  log "poster"
  PF=$(python3 -c "import json;e=json.load(open('public/edit.json'));i=[s for s in e['scenes'] if s['type']=='intro'];t=e.get('poster', (i[0]['start']+i[0]['dur']-0.5) if i else 3);print(round(t*e.get('fps',30)))")
  npx remotion still "$BUNDLE" Promo "$OUT/godterm-demo-poster.png" --frame="$PF" --props='{"layout":"landscape","hideCaptions":true}' --log=error
  echo "   frame $PF"
fi
if want srt; then
  log "srt"
  node --no-warnings scripts/export-srt.ts public/edit.json public/audio/narration.json "$OUT/godterm-demo.srt"
fi
log "done: $OUT"
ls -la "$OUT"
