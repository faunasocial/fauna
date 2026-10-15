#!/usr/bin/env bash
# Measure the debug-only cleartext policy on a packed Android APK.
#
#   android-apk-nsc-check.sh <apk>                  assert ABSENT  (shipping artifacts)
#   android-apk-nsc-check.sh --expect-present <apk> assert PRESENT (decoding control: the debug APK)
#
# The debug source set's `res/xml/network_security_config.xml` permits cleartext to
# 127.0.0.1 for the e2e harness (testing.md § Default app and nest mode → Android's
# run venue). It must reach no shipping build type. `test_android_cleartext_debug_only.py`
# witnesses that on the build graph; this witnesses it on the artifact.
#
# The two distinguishing STRINGS are scanned for, never the boolean attribute: a
# compiled AXML `cleartextTrafficPermitted` is a typed int, unreadable to `strings`.
# AAPT2 may pool a compiled XML's strings as 8-bit or UTF-16LE, so both decodings
# are read (the `android-kids-check` manifest helper's precedent). `--expect-present`
# is how a caller proves the decoding actually catches the strings.
#
# A missing member FAILS: an absent file would otherwise pass for "no cleartext" —
# resource-path shortening renaming the member is the way that happens silently.
set -euo pipefail

expect=absent
if [ "${1:-}" = "--expect-present" ]; then
    expect=present
    shift
fi
APK="${1:?usage: android-apk-nsc-check.sh [--expect-present] <apk>}"
[ -f "$APK" ] || { echo "ERROR: no APK at $APK" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/android-apk-nsc-check.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# Release builds shorten resource paths (`res/8G.xml`), so the member is resolved
# through the APK's resource table rather than assumed at its source path.
AAPT2="$(ls -d "${ANDROID_HOME:-$HOME/android-sdk}"/build-tools/*/aapt2 2>/dev/null | sort -V | tail -n 1)"
[ -n "$AAPT2" ] || { echo "ERROR: no aapt2 under ${ANDROID_HOME:-$HOME/android-sdk}/build-tools" >&2; exit 1; }
MEMBER="$("$AAPT2" dump resources "$APK" | awk '/resource 0x[0-9a-f]+ xml\/network_security_config$/ {f=1; next} f && /\(file\)/ {if (!d) print $3; d=1; f=0}')"
[ -n "$MEMBER" ] || {
    echo "ERROR: $APK's resource table has no xml/network_security_config file entry —" >&2
    echo "  an absence assertion over nothing would pass vacuously." >&2
    exit 1
}
unzip -q -o "$APK" "$MEMBER" -d "$WORK"
CFG="$WORK/$MEMBER"
[ -f "$CFG" ] || { echo "ERROR: $MEMBER named by the resource table is not in $APK." >&2; exit 1; }
{ strings -a "$CFG"; strings -a -el "$CFG"; } > "$WORK/cfg.txt"

fail=0
for pat in '127\.0\.0\.1' 'domain-config'; do
    n="$(grep -c -- "$pat" "$WORK/cfg.txt" || true)"
    if [ "$expect" = absent ] && [ "$n" != "0" ]; then
        echo "ERROR: $APK's network security config carries '$pat' ($n line(s)) — the debug-only" >&2
        echo "  cleartext overlay reached a shipping artifact (testing.md § Android's run venue)." >&2
        fail=1
    elif [ "$expect" = present ] && [ "$n" = "0" ]; then
        echo "ERROR: $APK's network security config does not show '$pat' — the string decoding" >&2
        echo "  is not catching what the overlay carries, so the absence assertion is blind." >&2
        fail=1
    fi
done
[ "$fail" = 0 ] || exit 1
echo "android-apk-nsc-check: OK — network security config $expect of 127.0.0.1/domain-config in $APK"
