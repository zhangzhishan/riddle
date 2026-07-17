#!/bin/sh
# Emergency recovery over SSH/telnet after an interrupted launcher.
set -u

APP_DIR=${RIDDLE_APP_DIR:-/mnt/onboard/.adds/riddle-kobo}
KOBO_DIR=${RIDDLE_KOBO_DIR:-/usr/local/Kobo}
BIN="$APP_DIR/riddle"
STATE="$APP_DIR/framebuffer.state"
LOG="$APP_DIR/riddle.log"

chmod 755 "$BIN" 2>/dev/null || true
if [ ! -s "$STATE" ]; then
    echo "No saved framebuffer state: $STATE" >&2
    exit 2
fi
set -- $(cat "$STATE")
if [ "$#" -ne 3 ] || ! "$BIN" --kobo-restore-state "$1" "$2" "$3" >>"$LOG" 2>&1; then
    echo "Framebuffer restore failed; power-cycle the Kobo." >&2
    exit 3
fi

killall -TERM nickel hindenburg sickel 2>/dev/null || true
sleep 1
if [ -x "$KOBO_DIR/hindenburg" ]; then
    "$KOBO_DIR/hindenburg" >>"$LOG" 2>&1 &
fi
LIBC_FATAL_STDERR_=1 "$KOBO_DIR/nickel" -platform kobo -skipFontLoad >>"$LOG" 2>&1 &
rm -f "$APP_DIR/run.lock/pid"
rmdir "$APP_DIR/run.lock" 2>/dev/null || true
echo "Framebuffer restored and Nickel restarted."
