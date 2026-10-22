#!/usr/bin/env bash
# image-upgrade-smoke.sh <new-image-ref> [options] — boot the pinned PREVIOUS
# production release on a fresh named volume until healthy, stop it, then boot
# <new-image-ref> on the SAME volume until healthy.
#
# Why this exists (release-pipeline gate, image-pipeline.md § Image tags &
# channels): a fresh-boot smoke cannot catch the migration-ordering-on-old-data
# class — a schema step that only fails against a /data written by an earlier
# release (content_fts_map's index-before-reconcile, 2026-08-03; the
# "no such column" outage). This script manufactures deterministically the exact
# upgrade every long-lived box (the organisation's nests, every production deployment) will execute
# when the new image ships, BEFORE any tag moves.
#
# The "previous release" is read from tests/e2e-unified/pinned-previous-build.toml
# (the pinned-previous-build convention, version-compatibility.md § Dimension 6),
# so the scenario tracks reality: exactly the hop production is about to make.
# The public pipeline passes --previous-image instead: its production boxes run
# the public :latest, so that is the hop they are about to make (image-pipeline.md
# § Image tags & channels, the two-pipelines design part 4).
#
# Usage:
#   scripts/image-upgrade-smoke.sh ghcr.io/faunasocial/nest:sha-<sha>-amd64 \
#     [--pin-file tests/e2e-unified/pinned-previous-build.toml | --previous-image <ref>] \
#     [--platform linux/amd64] [--expect-commit <sha>] [--timeout 120]
#
# Exit 0: the new image boots healthy on the previous release's /data — or the
#         pinned image is not in the registry (no previous build; printed) — or the
#         pin predates a ratified break that makes this hop expected-incompatible
#         (helpers/prev_build.py::RATIFIED_INPLACE_BREAKS, the `image_upgrade_smoke`
#         cell; version-compatibility.md § Dimension 6), which is printed and
#         skipped. The window clears itself once a release past the break moves
#         the pin. Needs the full history (ancestry), so the caller checks out
#         with fetch-depth 0. With --previous-image the ref is resolved to its
#         digest and booted, with no window check (the window is keyed on pin
#         ancestry); a ref the registry does not hold, or will not let an
#         anonymous caller read, is "no previous release" (printed) — every box
#         pulls :latest anonymously, so one no anonymous caller can read is one
#         no box is upgrading from.
# Exit 1: it does not (logs of the failing container are dumped) — do NOT ship.
set -euo pipefail

usage() {
    sed -n '2,24p' "$0" >&2
    exit 2
}

NEW_IMAGE="${1:-}"
[ -n "$NEW_IMAGE" ] || usage
shift

PIN_FILE="tests/e2e-unified/pinned-previous-build.toml"
PREVIOUS_IMAGE=""
PLATFORM="linux/amd64"
EXPECT_COMMIT=""
TIMEOUT=120
while [ $# -gt 0 ]; do
    case "$1" in
        --pin-file) PIN_FILE=$2; shift 2 ;;
        --previous-image) PREVIOUS_IMAGE=$2; shift 2 ;;
        --platform) PLATFORM=$2; shift 2 ;;
        --expect-commit) EXPECT_COMMIT=$2; shift 2 ;;
        --timeout) TIMEOUT=$2; shift 2 ;;
        *) usage ;;
    esac
done

if [ -n "$PREVIOUS_IMAGE" ]; then
    OLD_IMAGE=$PREVIOUS_IMAGE
    OLD_COMMIT=""
else
    OLD_IMAGE=$(sed -n 's/^nest_image = "\(.*\)"$/\1/p' "$PIN_FILE")
    if [ -z "$OLD_IMAGE" ]; then
        echo "[upgrade-smoke] FAIL: no nest_image in $PIN_FILE" >&2
        exit 1
    fi
    OLD_COMMIT=$(sed -n 's/^commit = "\(.*\)"$/\1/p' "$PIN_FILE")
fi

# A pin naming an image the registry no longer holds has no previous release to
# upgrade from (the 2026-10-11 package reset deleted every build the pin could
# name): say so and pass — the next verified build re-records the pin. Only the
# registry's not-found answer means that; any other inspect failure (an outage,
# a refused credential) fails the smoke rather than reading as a first run.
set +e
INSPECT=$(docker buildx imagetools inspect "$OLD_IMAGE" --format '{{.Manifest.Digest}}' 2>&1)
INSPECT_RC=$?
set -e
if [ "$INSPECT_RC" -ne 0 ]; then
    case "$INSPECT" in
        *"not found"*|*"manifest unknown"*|*"MANIFEST_UNKNOWN"*)
            echo "[upgrade-smoke] SKIP: no previous build — $OLD_IMAGE is not in the registry"
            exit 0
            ;;
    esac
    # GHCR refuses an anonymous token for a package that is absent or not
    # public — the two are indistinguishable from outside, and neither is a
    # :latest any box pulls. The pin path keeps the strict rule.
    if [ -n "$PREVIOUS_IMAGE" ]; then
        case "$INSPECT" in
            *"failed to fetch anonymous token"*"403 Forbidden"*)
                echo "[upgrade-smoke] SKIP: no previous release — $OLD_IMAGE is absent or not publicly readable"
                exit 0
                ;;
        esac
    fi
    echo "[upgrade-smoke] FAIL: could not resolve $OLD_IMAGE:" >&2
    printf '%s\n' "$INSPECT" >&2
    exit 1
fi

if [ -n "$PREVIOUS_IMAGE" ]; then
    # Boot exactly the manifest resolved here: a promote moving the tag mid-run
    # must not swap the previous release between this check and the pull.
    repo="${PREVIOUS_IMAGE%%@*}"
    case "${repo##*/}" in *:*) repo="${repo%:*}" ;; esac
    OLD_IMAGE="$repo@$INSPECT"
    echo "[upgrade-smoke] previous release $PREVIOUS_IMAGE is $OLD_IMAGE"
fi

# An open expected-incompatible window for this hop skips the smoke, loudly. Exit 3
# from the helper means "no window" and the smoke runs; anything else is a failure
# (an unresolvable break SHA must never read as "no window"). The window is keyed
# on the pin's ancestry, so it is the pin path's alone.
WINDOW_RC=3
if [ -z "$PREVIOUS_IMAGE" ]; then
    set +e
    WINDOW=$(uv run --no-project python tests/e2e-unified/helpers/prev_build.py \
        --expected-incompatible image_upgrade_smoke --pin "$OLD_COMMIT")
    WINDOW_RC=$?
    set -e
fi
case "$WINDOW_RC" in
    0)
        echo "[upgrade-smoke] SKIP: the pinned previous release cannot be upgraded in place:"
        printf '%s\n' "$WINDOW" | sed 's/^/[upgrade-smoke]   /'
        exit 0
        ;;
    3) ;;
    *)
        echo "[upgrade-smoke] FAIL: could not decide the expected-incompatible window (rc=$WINDOW_RC)" >&2
        exit 1
        ;;
esac

SUFFIX="$$-$(date +%s)"
VOLUME="fauna-upgrade-smoke-$SUFFIX"
OLD_C="upgrade-smoke-old-$SUFFIX"
NEW_C="upgrade-smoke-new-$SUFFIX"

cleanup() {
    docker rm -f "$OLD_C" "$NEW_C" >/dev/null 2>&1 || true
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# Boot $1 (image) as container $2 on the shared volume; wait until /api/v1/health
# reports status ok (bounded). Ephemeral host port: parallel-safe on dev boxes,
# harmless on a CI runner. FAUNA_PORT/FAUNA_MODE mirror build-nest-image.yml's fresh
# smoke: the nest's listener binds container loopback, so the probe goes through
# the SNI router's :443, the production path (deployment-threat-model.md § Concern 1a).
boot_and_wait_healthy() {
    local image=$1 cname=$2 port body
    docker run -d --name "$cname" \
        -v "$VOLUME":/data \
        -p 127.0.0.1::443 \
        -e FAUNA_PORT=3000 \
        -e FAUNA_MODE=public \
        "$image" >/dev/null
    port=$(docker port "$cname" 443/tcp | head -1 | sed 's/.*://')
    local deadline=$(( $(date +%s) + TIMEOUT ))
    while :; do
        body=$(curl -fsk "https://127.0.0.1:$port/api/v1/health" 2>/dev/null \
            || curl -fs "http://127.0.0.1:$port/api/v1/health" 2>/dev/null \
            || true)
        case "$body" in *'"status":"ok"'*) break ;; esac
        if [ "$(date +%s)" -ge "$deadline" ]; then
            # Diagnostics on stderr: callers capture this function's stdout.
            echo "[upgrade-smoke] FAIL: $cname ($image) not healthy after ${TIMEOUT}s (last health: '${body:-<no response>}')" >&2
            echo "[upgrade-smoke] ---- docker logs $cname ----" >&2
            docker logs "$cname" 2>&1 | tail -80 >&2
            return 1
        fi
        sleep 2
    done
    printf '%s' "$body"
}

echo "[upgrade-smoke] previous release: $OLD_IMAGE"
echo "[upgrade-smoke] candidate image: $NEW_IMAGE"
docker pull --platform "$PLATFORM" "$OLD_IMAGE" >/dev/null

echo "[upgrade-smoke] phase 1: boot the previous release on a fresh volume $VOLUME"
boot_and_wait_healthy "$OLD_IMAGE" "$OLD_C" >/dev/null
docker rm -f "$OLD_C" >/dev/null

echo "[upgrade-smoke] phase 2: boot the candidate on the SAME /data (the production upgrade hop)"
body=$(boot_and_wait_healthy "$NEW_IMAGE" "$NEW_C")

if [ -n "$EXPECT_COMMIT" ]; then
    commit=$(printf '%s' "$body" | sed -n 's/.*"commit":"\([^"]*\)".*/\1/p')
    # Prefix-tolerant either way: workflow builds stamp the full 40-char sha,
    # dev pushes the short 8 (routes.rs::health serves it verbatim).
    match=0
    if [ -n "$commit" ]; then
        case "$commit" in "$EXPECT_COMMIT"*) match=1 ;; esac
        case "$EXPECT_COMMIT" in "$commit"*) match=1 ;; esac
    fi
    if [ "$match" != 1 ]; then
        echo "[upgrade-smoke] FAIL: upgraded container reports commit '$commit', expected $EXPECT_COMMIT"
        exit 1
    fi
fi

echo "[upgrade-smoke] PASS: $NEW_IMAGE boots healthy on ${OLD_IMAGE##*:}'s /data"
