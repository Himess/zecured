#!/usr/bin/env bash
# Assembles docs/assets/loan.gif from the frames `pnpm shots` writes (site/tests/shots/loan): the five steps of
# "Follow one loan", then the viewing key. Needs ffmpeg. One palette for all frames, 960 px wide, 2.2 s a frame.
set -euo pipefail
cd "$(dirname "$0")/.."
frames=site/tests/shots/loan
[ -f "$frames/frame-01.png" ] || { echo "no frames: run 'pnpm shots' in site/tests first" >&2; exit 1; }
ffmpeg -v error -y -framerate 1/2.2 -i "$frames/frame-%02d.png" \
  -vf "scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96:stats_mode=full[p];[b][p]paletteuse=dither=sierra2_4a" \
  -loop 0 docs/assets/loan.gif
ls -l docs/assets/loan.gif
