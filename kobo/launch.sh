#!/bin/sh
set -u

APP_DIR=/mnt/onboard/.adds/riddle-kobo
LOG="$APP_DIR/riddle.log"
NICKEL_PAUSED=0

resume_nickel() {
    if [ "$NICKEL_PAUSED" -eq 1 ]; then
        killall -CONT nickel hindenburg sickel 2>/dev/null || true
        NICKEL_PAUSED=0
    fi
}
trap 'resume_nickel' EXIT INT TERM HUP

mkdir -p "$APP_DIR/data/memories"
chmod 755 "$APP_DIR/riddle" 2>/dev/null || true

# Keep Nickel alive but frozen. This is faster and safer than killing and
# reconstructing the full Kobo UI stack, and EXIT always resumes it.
killall -STOP nickel hindenburg sickel 2>/dev/null || true
NICKEL_PAUSED=1
sleep 1

export RIDDLE_MEMORY_DIR="$APP_DIR/data/memories"
if [ -f "$APP_DIR/oracle.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$APP_DIR/oracle.env"
    set +a
fi

printf '\n=== launch %s ===\n' "$(date)" >>"$LOG"
"$APP_DIR/riddle" >>"$LOG" 2>&1
STATUS=$?
printf '=== exit %s status=%s ===\n' "$(date)" "$STATUS" >>"$LOG"
exit "$STATUS"
