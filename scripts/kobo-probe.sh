#!/bin/sh
# Read-only probe for an Elipsa 2E. Redirect output to a file on /mnt/onboard.
set -u

echo '== system =='
date
uname -a
printf 'firmware='
awk -F, 'NR==1 {print $3}' /mnt/onboard/.kobo/version 2>/dev/null || echo unknown

echo '== framebuffer =='
for f in /sys/class/graphics/fb0/name /sys/class/graphics/fb0/virtual_size \
         /sys/class/graphics/fb0/bits_per_pixel /sys/class/graphics/fb0/rotate \
         /sys/class/graphics/fb0/stride; do
    [ -r "$f" ] && printf '%s=' "$(basename "$f")" && cat "$f"
done
command -v fbink >/dev/null 2>&1 && fbink -e 2>&1 || true

echo '== input devices =='
for n in /sys/class/input/event*/device/name; do
    [ -r "$n" ] || continue
    printf '%s=' "$(basename "$(dirname "$(dirname "$n")")")"
    cat "$n"
done
sed -n '1,240p' /proc/bus/input/devices 2>/dev/null || true

echo '== power/wifi =='
for f in /sys/class/power_supply/*/status /sys/class/power_supply/*/capacity; do
    [ -r "$f" ] && printf '%s=' "$f" && cat "$f"
done
ip addr show wlan0 2>/dev/null | sed -n '1,30p' || true
