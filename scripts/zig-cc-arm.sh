#!/usr/bin/env bash
set -euo pipefail
exec zig cc -target arm-linux-musleabihf -mcpu=cortex_a7 "$@"
