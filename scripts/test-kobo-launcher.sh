#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/riddle-launcher-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT
APP="$TMP/app"
KOBO="$TMP/kobo"
MOCK="$TMP/mockbin"
EVENTS="$TMP/events"
mkdir -p "$APP" "$KOBO" "$MOCK"

cat >"$APP/riddle" <<'SH'
#!/bin/sh
case "${1:-}" in
  --kobo-capture-state)
    echo capture >>"$EVENTS"
    echo '32 0 1'
    ;;
  --kobo-restore-state)
    echo "restore $2 $3 $4" >>"$EVENTS"
    [ "${FAIL_RESTORE:-0}" -eq 0 ]
    ;;
  *)
    echo app >>"$EVENTS"
    exit 23
    ;;
esac
SH
cat >"$MOCK/killall" <<'SH'
#!/bin/sh
echo "killall $*" >>"$EVENTS"
exit 0
SH
cat >"$MOCK/pidof" <<'SH'
#!/bin/sh
exit 1
SH
cat >"$MOCK/sleep" <<'SH'
#!/bin/sh
exit 0
SH
for name in hindenburg nickel; do
cat >"$KOBO/$name" <<SH
#!/bin/sh
echo $name >>"\$EVENTS"
exit 0
SH
done
chmod +x "$APP/riddle" "$MOCK"/* "$KOBO"/*

run_launcher() {
    local expected=$1
    set +e
    EVENTS="$EVENTS" PATH="$MOCK:$PATH" RIDDLE_APP_DIR="$APP" \
        RIDDLE_KOBO_DIR="$KOBO" /bin/sh "$ROOT/kobo/launch.sh"
    local status=$?
    set -e
    [[ $status -eq $expected ]] || { echo "expected $expected, got $status" >&2; exit 1; }
    [[ ! -d "$APP/run.lock" ]]
    sleep 0.2
}

: >"$EVENTS"
run_launcher 23
grep -qx capture "$EVENTS"
grep -qx app "$EVENTS"
grep -qx 'restore 32 0 1' "$EVENTS"
grep -qx nickel "$EVENTS"
python3 - "$EVENTS" <<'PY'
import sys
lines=open(sys.argv[1]).read().splitlines()
assert lines.index('capture') < lines.index('app') < lines.index('restore 32 0 1') < lines.index('nickel'), lines
PY

: >"$EVENTS"
FAIL_RESTORE=1; export FAIL_RESTORE
run_launcher 23
grep -qx 'restore 32 0 1' "$EVENTS"
if grep -qx nickel "$EVENTS"; then
    echo 'Nickel restarted despite framebuffer restore failure' >&2
    exit 1
fi

echo launcher_crash_restore_tests=PASS
