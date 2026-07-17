#!/usr/bin/env bash
set -euo pipefail

MOUNT=${1:-/Volumes/KOBOeReader}
[[ -d "$MOUNT/.kobo" ]] || { echo "Not a Kobo volume: $MOUNT" >&2; exit 2; }

# Preserve the user's diary memories by default.
if [[ -d "$MOUNT/.adds/riddle-kobo/data" ]]; then
    mkdir -p "$MOUNT/.adds/riddle-kobo-data-backup"
    cp -R "$MOUNT/.adds/riddle-kobo/data/." "$MOUNT/.adds/riddle-kobo-data-backup/"
fi
STAMP=$(date +%Y%m%d-%H%M%S)
if [[ -d "$MOUNT/.adds/riddle-kobo" ]]; then
    mv "$MOUNT/.adds/riddle-kobo" "$MOUNT/.adds/riddle-kobo.removed-$STAMP"
fi
if [[ -f "$MOUNT/.adds/nm/riddle-kobo" ]]; then
    mv "$MOUNT/.adds/nm/riddle-kobo" "$MOUNT/.adds/nm/riddle-kobo.disabled-$STAMP"
fi
sync

echo "Disabled riddle-kobo. No files were deleted; memories were also copied to .adds/riddle-kobo-data-backup/."
