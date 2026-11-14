#!/usr/bin/env bash
# Run one call to Apple's notary or ticket service, retrying when it fails.
#
# Usage: installer/macos/apple-retry.sh <command> [args...]
#   e.g. installer/macos/apple-retry.sh xcrun stapler staple build/Release/Fauna.app
#
# Why: the calls that reach Apple over the network — `notarytool submit --wait`
# and `stapler staple` — fail on a dropped connection as well as on a real
# refusal, and a dropped connection is common enough to be expected (0.1.6's
# release: the staple of Fauna.app lost its connection to Apple's ticket
# service after the notarization was accepted). Both are safe to repeat:
# stapling fetches and attaches the same ticket again, and a repeated submit
# files a second request for the same bytes. A real refusal fails every
# attempt the same way, so it still fails the step, only later.
#
# Attempts and waits are fixed here (not a knob): 5 attempts, waiting 20, 40,
# 80 and 160 seconds between them — about five minutes in all.
set -uo pipefail

[ "$#" -ge 1 ] || { echo "usage: $0 <command> [args...]" >&2; exit 2; }

attempts=5
wait_s=20
n=1
while :; do
    "$@"
    rc=$?
    [ "$rc" -eq 0 ] && exit 0
    if [ "$n" -ge "$attempts" ]; then
        echo "apple-retry: '$*' failed $n times (last exit $rc) — giving up" >&2
        exit "$rc"
    fi
    echo "apple-retry: '$*' failed (exit $rc), attempt $n of $attempts — retrying in ${wait_s}s" >&2
    sleep "$wait_s"
    n=$((n + 1))
    wait_s=$((wait_s * 2))
done
