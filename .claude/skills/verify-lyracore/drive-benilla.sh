#!/usr/bin/env bash
# Run one scripted Benilla session against a LyraCore Realm and keep its evidence.
#
#   BENILLA_DIR=<benilla checkout> WOW_DATA=<1.12.1 Data dir> \
#   WOW_USER=TEST WOW_PASS=test123 WOW_CHAR=Tester \
#   WOW_PROBE_CHAT='.tele goldshire' WOW_PROBE_LUA='ProbeLog(GetZoneText())' \
#     .claude/skills/verify-lyracore/drive-benilla.sh [exit-after-secs]
#
# Every other WOW_* variable passes through to Benilla. Output: $EVIDENCE (default
# /tmp/lyracore-verify/<UTC stamp>) gets benilla.log, probe.log and shot.png. Exit code is Benilla's.
set -uo pipefail

: "${BENILLA_DIR:?set BENILLA_DIR to a Benilla checkout}"
: "${WOW_USER:?}" "${WOW_PASS:?}" "${WOW_CHAR:?}"
secs=${1:-60}
out=${EVIDENCE:-/tmp/lyracore-verify/$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$out"

# BENILLA_HOME keeps the run off the operator's own benilla-config (settings, saved variables).
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

lock=${LYRACORE_BUILD_LOCK:-$HOME/.cache/lyracore-build.lock}
flock -o "$lock" cargo build --release -q -p benilla --manifest-path "$BENILLA_DIR/Cargo.toml" ||
    { echo "drive-benilla: Benilla did not build" >&2; exit 1; }

cd "$BENILLA_DIR" || exit 1
BENILLA_HOME="$scratch" WOW_HOST="${WOW_HOST:-127.0.0.1:3724}" \
    WOW_UNATTENDED=1 WOW_NOSOUND=1 WOW_PROBE_EXIT_AT="$secs" \
    WOW_LIVE_SHOT="${WOW_LIVE_SHOT:-$out/shot.png}" \
    timeout $((secs + 90)) target/release/benilla >"$out/benilla.log" 2>&1
code=$?

sed -E 's/\x1b\[[0-9;]*m//g' "$out/benilla.log" |
    grep -E 'probe-log:|server says|ERROR' >"$out/probe.log"
echo "drive-benilla: exit=$code evidence=$out"
cat "$out/probe.log"
exit "$code"
