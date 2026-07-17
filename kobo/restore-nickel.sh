#!/bin/sh
# Emergency recovery over SSH/telnet if a launcher is interrupted.
killall -CONT nickel hindenburg sickel 2>/dev/null || true
exit 0
