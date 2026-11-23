#!/usr/bin/env bash
# Tar en bild av fönstret på en virtuell skärm. Kräver Xvfb och ImageMagick.
# Användning: scripts/skarmbild.sh <databasfil> <utfil.png> ["NTC KEDJA"] [relationsnummer]
set -euo pipefail
DB="$1"; OUT="$2"; CMD="${3:-}"; SEL="${4:-}"
ARGS=("$DB")
[ -n "$CMD" ] && ARGS+=(--run "$CMD")
[ -n "$SEL" ] && ARGS+=(--select "$SEL")
export SHOT_OUT="$OUT"
xvfb-run -a -s "-screen 0 1360x820x24" bash -c '
  "$0" "$@" >/dev/null 2>&1 &
  APP=$!
  sleep 10
  import -window root "$SHOT_OUT"
  kill $APP 2>/dev/null || true
' ./target/debug/term-ui "${ARGS[@]}"
