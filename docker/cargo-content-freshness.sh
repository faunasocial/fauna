#!/bin/sh
# Content-keyed mtimes for a cargo build over a shared target cache mount.
#
#   cargo-content-freshness.sh <target-dir> <src-dir>
#
# Run it at the top of a compile RUN, before cargo, on the RUN's own mount.
#
# WHY: the Dockerfile's `rust-target-*` cache mounts are shared machine-wide, by
# every checkout that builds the image. Cargo's default freshness compares a
# source file's mtime with its cached fingerprint's. COPY keeps the host mtime,
# so a file whose content changed but whose mtime is older than another
# checkout's fingerprint reads "fresh", and cargo serves that checkout's crate.
# `CARGO_UNSTABLE_CHECKSUM_FRESHNESS` was meant to close this hole. Measured
# 2026-11-11, cargo inside the image records no checksums: its `dep-lib-*`
# fingerprints carry no `blake3=` entry, so it silently falls back to mtime. A
# caldav compile then ran against another tree's fauna-client-drafts and failed.
# Had the stale crate still compiled, the image would have shipped another
# checkout's code.
#
# HOW: the mount keeps a manifest, one `<sha256> <mtime> <path>` line per source
# file, recording each path's content and the mtime this script gave that
# content. Each run, for every file under <src-dir> (except <target-dir>):
#   - same content as recorded → the recorded mtime is put back;
#   - new or changed content   → the mtime is set to now, and recorded.
# Then cargo's mtime check is right whatever COPY left:
#   - A unit compiled from content C has a fingerprint newer than C's recorded
#     mtime, so C again reads fresh. That is correct, and a cancelled build's
#     finished crates stay finished.
#   - Any unit built from other content predates the moment this content was
#     first recorded, so it reads stale and is rebuilt.
# The manifest is written before cargo runs, so the RUN needs no closing step,
# and a build that fails or is cancelled leaves the manifest true. The mount is
# `sharing=locked`, so one build at a time reads and writes it. Only the first
# run, with no manifest, touches every file, a one-time full workspace
# recompile. Third-party crate sources live in the registry mount and are never
# touched.
#
# The mount's lock is held per RUN, so a later RUN of the same build may find
# another checkout's units on the mount. rust-wasm's wasm-pack RUN, which must
# find every unit fresh, therefore never mounts it: the compile RUN copies its
# target into its own layer and the wasm-pack RUN reads that copy (the
# Dockerfile's rust-wasm stage comment).
set -eu

[ $# -eq 2 ] || { echo "usage: $0 <target-dir> <src-dir>" >&2; exit 2; }
target=$1
src=$2
manifest="$target/.fauna-content-mtimes"
# FAUNA_FRESHNESS_NOW: the test's clock (tests/docker/test_cargo_content_freshness.py);
# the Dockerfile never sets it.
now=${FAUNA_FRESHNESS_NOW:-$(date +%s)}
work=$(mktemp -d)

mkdir -p "$target"
[ -f "$manifest" ] || : > "$manifest"

# `<sha256>  ./<path>`, one per file. Paths are git-tracked tree paths and never
# hold a newline.
(cd "$src" && find . -path "./${target#"$src"/}" -prune -o -type f -print0 \
    | xargs -0 -r sha256sum) > "$work/now"

# The old manifest first (ARGV[1]; NR == FNR would misread an empty one), then
# this tree. Writes the new manifest
# and `<mtime> <path>` for every file to stamp.
awk -v now="$now" -v out="$work/manifest" -v stamp="$work/stamp" '
    FILENAME == ARGV[1] {
        h = $1; t = $2
        p = substr($0, length($1) + length($2) + 3)
        hash[p] = h; when[p] = t
        next
    }
    {
        h = $1
        p = substr($0, length($1) + 3)
        t = (p in hash && hash[p] == h) ? when[p] : now
        if (t == now) changed++
        print h, t, p > out
        print t, p > stamp
    }
    END { print changed + 0 > "/dev/stderr" }
' "$manifest" "$work/now" 2> "$work/counts"

total=$(wc -l < "$work/now")
changed=$(cat "$work/counts")

# One `touch` per distinct mtime: few values, since each run records one `now`.
(cd "$src" && for t in $(cut -d' ' -f1 "$work/stamp" | sort -u); do
    awk -v t="$t" '$1 == t { print substr($0, length($1) + 2) }' "$work/stamp" \
        | tr '\n' '\0' | xargs -0 -r touch -m -d "@$t" --
done)

mv "$work/manifest" "$manifest"
rm -rf "$work"
echo "cargo-content-freshness: $changed of $total files are new or changed content since the mount last saw them; the rest keep their recorded mtimes"
