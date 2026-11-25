#!/usr/bin/env bash
#
# An independent RTMP receiver for developing this worker. Nothing about it
# touches production: it listens on localhost and writes to a local file.
#
#   ./scripts/test-rtmp-sink.sh 1935 /tmp/received.flv 30
#
# Then point the worker at rtmp://127.0.0.1:1935/live/test. When it finishes,
# check what actually arrived rather than trusting an exit code:
#
#   ffprobe -v error -show_entries stream=codec_type,width,height -of default=nw=1 /tmp/received.flv
#
set -Eeuo pipefail
PORT=${1:-1935}
OUT=${2:-/tmp/received.flv}
SECS=${3:-30}
echo "listening on rtmp://127.0.0.1:${PORT}/live/test for ${SECS}s → ${OUT}"
# `-c copy` so the file holds exactly what was sent, not a re-encode of it.
exec ffmpeg -hide_banner -loglevel warning -y \
  -listen 1 -timeout "$((SECS + 15))" -i "rtmp://127.0.0.1:${PORT}/live/test" \
  -c copy -t "$SECS" "$OUT"
