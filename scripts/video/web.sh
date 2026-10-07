#!/usr/bin/env bash
# Web and social files from the rendered promo (run after render.sh).
#   scripts/video/web.sh [--site DIR]
# Writes ~/godterm-video/out/web/: godterm-demo-1080p-web.mp4, godterm-demo-720p-web.mp4,
# godterm-demo-poster.jpg, hero-loop.mp4/.webm, teaser.mp4/.gif, godterm-x.mp4.
# With --site, copies the web MP4s, poster, hero loop and SRT to DIR/public/video
# (as godterm-demo-1080p.mp4, -720p.mp4, -poster.jpg, godterm-hero-loop.*, godterm-demo.srt).
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROJ="${GODTERM_VIDEO_DIR:-$HOME/godterm-video}"
IN="${GODTERM_VIDEO_OUT:-$PROJ/out}"; OUT="$IN/web"; SITE=""
[ "${1:-}" = "--site" ] && SITE="$2"
mkdir -p "$OUT"
ff() { ffmpeg -loglevel error -y "$@"; }
SRC="$IN/godterm-demo-4k.mp4"

# full film for the site and GitHub: x264 two pass, ~7 Mb/s, moov first
ff -i "$SRC" -vf scale=1920:1080:flags=lanczos -c:v libx264 -preset slow -profile:v high -b:v 6.5M -maxrate 9M -bufsize 14M \
   -pass 1 -passlogfile "$OUT/x264" -an -f mp4 /dev/null
ff -i "$SRC" -vf scale=1920:1080:flags=lanczos -c:v libx264 -preset slow -profile:v high -b:v 6.5M -maxrate 9M -bufsize 14M \
   -pass 2 -passlogfile "$OUT/x264" -pix_fmt yuv420p -c:a aac -b:a 128k -movflags +faststart "$OUT/godterm-demo-1080p-web.mp4"
ff -i "$SRC" -vf scale=1280:720:flags=lanczos -c:v libx264 -preset slow -profile:v high -crf 26 -maxrate 2.6M -bufsize 5M \
   -pix_fmt yuv420p -c:a aac -b:a 96k -movflags +faststart "$OUT/godterm-demo-720p-web.mp4"
rm -f "$OUT"/x264*
# poster
ff -i "$IN/godterm-demo-poster.png" -vf scale=1920:1080:flags=lanczos -q:v 5 "$OUT/godterm-demo-poster.jpg"

# hero loop: the Live map shot without captions, 8 s, cross faded into itself, muted
MAP="$PROJ/public/rec/s08_map.mp4"
ff -ss 3.5 -t 9 -i "$MAP" -filter_complex \
  "[0:v]scale=1920:1080:flags=lanczos,split[a][b];[a]trim=0:8,setpts=PTS-STARTPTS[m];[b]trim=8:9,setpts=PTS-STARTPTS[t];[t][m]xfade=transition=fade:duration=1:offset=0,format=yuv420p[v]" \
  -map "[v]" -t 8 -an -c:v libx264 -preset slow -profile:v high -crf 27 -movflags +faststart "$OUT/hero-loop.mp4"
ff -i "$OUT/hero-loop.mp4" -an -c:v libvpx-vp9 -b:v 0 -crf 40 -row-mt 1 "$OUT/hero-loop.webm"

# README teaser: the move, one click (8 s, captions burned in by the film)
T0=$(python3 -c "import json;e=json.load(open('$PROJ/public/edit.json'));s=[x for x in e['scenes'] if x.get('src','').endswith('s05_move.mp4')][0];print(s['start']+0.3)")
ff -ss "$T0" -t 8 -i "$SRC" -vf scale=1280:720:flags=lanczos -an -c:v libx264 -preset slow -crf 24 -pix_fmt yuv420p -movflags +faststart "$OUT/teaser.mp4"
ff -i "$OUT/teaser.mp4" -vf "fps=12,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=128[p];[s1][p]paletteuse=dither=bayer:bayer_scale=4" "$OUT/teaser.gif"

# X / Twitter: the square cut (captions burned in), x264 high, AAC
ff -i "$IN/godterm-demo-square.mp4" -c:v libx264 -preset slow -profile:v high -crf 20 -maxrate 12M -bufsize 20M -pix_fmt yuv420p \
   -c:a aac -b:a 160k -movflags +faststart "$OUT/godterm-x.mp4"

if [ -n "$SITE" ]; then
  mkdir -p "$SITE/public/video"
  V="$SITE/public/video"
  cp "$OUT/godterm-demo-1080p-web.mp4" "$V/godterm-demo-1080p.mp4"
  cp "$OUT/godterm-demo-720p-web.mp4" "$V/godterm-demo-720p.mp4"
  cp "$OUT/godterm-demo-poster.jpg" "$V/godterm-demo-poster.jpg"
  cp "$OUT/hero-loop.mp4" "$V/godterm-hero-loop.mp4"
  cp "$OUT/hero-loop.webm" "$V/godterm-hero-loop.webm"
  cp "$IN/godterm-demo.srt" "$V/godterm-demo.srt"
fi
ls -la "$OUT"
