#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
MOUNT=${1:-/Volumes/KOBOeReader}
BUNDLE=${2:-"$ROOT/dist/riddle-kobo"}

[[ -d "$MOUNT/.kobo" ]] || { echo "Not a Kobo volume: $MOUNT" >&2; exit 2; }
[[ -x "$BUNDLE/.adds/riddle-kobo/riddle" ]] || { echo "Build bundle first: scripts/build-kobo.sh" >&2; exit 2; }

mkdir -p "$MOUNT/.adds/riddle-kobo" "$MOUNT/.adds/nm"
cp -R "$BUNDLE/.adds/riddle-kobo/." "$MOUNT/.adds/riddle-kobo/"
cp "$BUNDLE/.adds/nm/riddle-kobo" "$MOUNT/.adds/nm/riddle-kobo"
sync

echo "Installed riddle-kobo to $MOUNT"
echo "Eject the Kobo safely. Open NickelMenu → Riddle Diary."
