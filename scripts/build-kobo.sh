#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://rsproxy.cn}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-https://rsproxy.cn/rustup}"
TARGET=armv7-unknown-linux-musleabihf
FBINK_COMMIT=83110d3d278cf9cd44cc1d16237e284a89f72633
FBINK_DIR="$ROOT/target/fbink-src"
DIST="$ROOT/dist/riddle-kobo"

command -v zig >/dev/null || { echo "zig is required (brew install zig)" >&2; exit 2; }
command -v cargo-zigbuild >/dev/null || { echo "cargo-zigbuild is required (cargo install cargo-zigbuild)" >&2; exit 2; }
chmod +x "$ROOT/scripts/zig-cc-arm.sh"
rustup target add "$TARGET"

if [[ ! -d "$FBINK_DIR/.git" ]]; then
    git clone https://github.com/NiLuJe/FBInk.git "$FBINK_DIR"
fi
git -C "$FBINK_DIR" fetch --depth 1 origin "$FBINK_COMMIT"
git -C "$FBINK_DIR" checkout --detach FETCH_HEAD
git -C "$FBINK_DIR" submodule update --init --recursive --depth 1
make -C "$FBINK_DIR" clean >/dev/null 2>&1 || true
make -C "$FBINK_DIR" -j"$(sysctl -n hw.ncpu)" staticlib \
    KOBO=1 OUT_DIR=Release \
    CC="$ROOT/scripts/zig-cc-arm.sh" \
    AR="zig ar" RANLIB="zig ranlib"

test -f "$FBINK_DIR/Release/libfbink.a"
RIDDLE_FBINK_DIR="$FBINK_DIR" cargo zigbuild --release --target "$TARGET" --features kobo

rm -rf "$DIST"
mkdir -p "$DIST/.adds/riddle-kobo/data/memories" "$DIST/.adds/nm"
install -m 755 "$ROOT/target/$TARGET/release/riddle" "$DIST/.adds/riddle-kobo/riddle"
install -m 755 "$ROOT/kobo/launch.sh" "$DIST/.adds/riddle-kobo/launch.sh"
install -m 755 "$ROOT/kobo/restore-nickel.sh" "$DIST/.adds/riddle-kobo/restore-nickel.sh"
install -m 755 "$ROOT/scripts/kobo-probe.sh" "$DIST/.adds/riddle-kobo/kobo-probe.sh"
install -m 644 "$ROOT/oracle.env.example" "$DIST/.adds/riddle-kobo/oracle.env.example"
install -m 644 "$ROOT/kobo/nm/riddle-kobo" "$DIST/.adds/nm/riddle-kobo"

(
    cd "$DIST"
    zip -qry "$ROOT/dist/riddle-kobo.zip" .
)

file "$DIST/.adds/riddle-kobo/riddle"
echo "Bundle: $DIST"
echo "Archive: $ROOT/dist/riddle-kobo.zip"
