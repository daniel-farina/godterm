#!/bin/sh
# Barge-in test fixtures (16 kHz mono s16 WAV), made once and checked in:
# 3 s of pink noise (a vacuum or fan) and a short spoken sentence after
# 0.6 s of silence. Needs ffmpeg, and `say` (macOS) for the speech.
set -e
cd "$(dirname "$0")"
ffmpeg -loglevel error -y -f lavfi -i "anoisesrc=color=pink:amplitude=0.25:duration=3:seed=7" \
  -ar 16000 -ac 1 -c:a pcm_s16le -fflags +bitexact -flags:a +bitexact -map_metadata -1 pink_noise.wav
say -o speech.aiff "Wait, open the other tab."
ffmpeg -loglevel error -y -i speech.aiff -af "adelay=600:all=1" \
  -ar 16000 -ac 1 -c:a pcm_s16le -fflags +bitexact -flags:a +bitexact -map_metadata -1 speech.wav
rm -f speech.aiff
