#!/usr/bin/env bash
# usage: scripts/win-stage-dll.sh <built-dll> <staged-dll>
#
# Copy a freshly built dll into its staging slot even while some process still
# has the OLD one mapped. Windows refuses to overwrite or delete a mapped image
# but does allow a rename, so the old file is moved aside first and the copy
# lands in the freed name; the holder keeps running on the renamed image.
# Every aside is then deleted best-effort — this pass's and any earlier one's;
# one still mapped refuses the delete and is simply retried next pass.
#
# Called from INSIDE `_windows-ffi-flavor`'s gated build command, so it only
# ever runs for a build that recipe just performed
# (build-target-layout-windows.md § Cargo target dir layout (win)).
set -euo pipefail
src=$1
dst=$2
if [ -e "$dst" ]; then
    mv -f "$dst" "$dst.old.$$.$RANDOM"
fi
cp "$src" "$dst"
for old in "$dst".old.*; do
    [ -e "$old" ] && rm -f "$old" 2>/dev/null || true
done
