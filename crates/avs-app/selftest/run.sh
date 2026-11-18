#!/bin/bash
# Runs one editor self-test on a fresh, isolated fixture library.
#
#   crates/avs-app/selftest/run.sh <script-name> [--long]
#
# Needs ffmpeg and the built binaries:
#   cargo build -p avs-app --bins --examples --target-dir target
# The window must be painted, so the display has to be awake and unlocked
# (macOS does not draw windows while the screen is off); caffeinate keeps it awake.
# Nothing outside $WORK is read or written. "${ROOT}" in a script means the
# fixture folder.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
BIN="${AVS_TARGET_DIR:-$REPO/target}/debug"
WORK="${AVS_SELFTEST_WORK:-${TMPDIR:-/tmp}/avs-selftest}"
name="$1"; shift
mkdir -p "$WORK"
rm -rf "$WORK/run-$name" "$WORK/shots-$name"
"$BIN/examples/make_fixture" "$WORK/run-$name" "$@" > "$WORK/run-$name.fixture.txt" || { echo "fixture failed"; exit 3; }
caffeinate -u -d -i timeout 400 "$BIN/agent-video-studio" ${AVS_SELFTEST_APP_ARGS:-} --library "$WORK/run-$name" --self-test "$HERE/$name.json" --shots "$WORK/shots-$name" 2>&1 | grep -v "^step " | tail -30
status=${PIPESTATUS[0]}
echo "screenshots: $WORK/shots-$name"
exit "$status"
