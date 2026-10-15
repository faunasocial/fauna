#!/usr/bin/env bash
# deploy-verify-serving.sh <base-url> --expect-commit <sha> [options] — the
# image pipeline's verify gate (serving half), and the standardized form of
# the old ad-hoc "gates 4-5" serving proofs.
#
# Asserts, against a live box, that:
#   1. /api/v1/health reports status=="ok" AND commit matching <sha>
#      (prefix-tolerant either way: workflow builds stamp the full 40-char sha,
#      `just docker-push-dev` the short 8 — routes.rs::health serves it
#      verbatim; a degraded boot reports "needs_update" and correctly fails);
#   1b. and, when --expect-build-id is given, that the box reports THAT BUILD —
#      matched exactly. This is what binds the gate to an ARTIFACT rather than
#      to a commit string: two builds of one commit are otherwise
#      indistinguishable here, so a container this pipeline never built could
#      satisfy the gate while :latest moved to the one it did build
#     . Fail-closed: health
#      that reports no build_id at all FAILS rather than degrading to
#      commit-only, since a missing stamp is least informative exactly where
#      provenance is least certain. Omit the flag for ad-hoc commit-only use.
#   2. that stays true across a soak window (re-polled; any flap, or the commit
#      changing — e.g. a concurrent dev push stealing the staging box — fails).
#      Note the ordering this buys: phase 1 cannot pass until the candidate
#      build is actually serving, so the soak can no longer start against the
#      OUTGOING artifact and then redden when the candidate arrives mid-window
#      (the same finding's leg D). The gate observes the transition instead of
#      inferring it from a poll interval, which is why it needs no knowledge of
#      the staging box's Watchtower cadence;
#   3. optionally, the TLS mail ports serve their banners (--mail-ports).
#
# Every wait is a bounded deadline poll (convention 14 — no settle-sleeps; a
# green run pays only the soak). Probe output goes to temp files, never through
# a pipe that a timeout kill could truncate (the 2026-07-23 openssl gotcha).
#
# Used by: .github/workflows/build-nest-image.yml + restage-nest.yml (the
# private pipeline's terminal gate on the :dev staging deploy — image-pipeline.md
# § Image tags & channels; `just release-verified` records a build, and advances
# the pinned-previous-build pin, only past it), tier_3-pinned by
# tests/e2e-unified/tests/api/test_deploy_verify_serving.py, and runnable ad hoc
# by sessions.
#
# Usage:
#   scripts/deploy-verify-serving.sh https://dev.fauna.build \
#     --expect-commit <sha> [--expect-build-id <id>] \
#     [--timeout 600] [--soak 300] [--interval 15] \
#     [--mail-ports dev.fauna.build:993,dev.fauna.build:465,dev.fauna.build:587] [--probe-timeout 180]
#
# Exit 0 = GATE PASS (the build is verified — `just release-verified` may record
# it). Exit 1 = GATE FAIL. Exit 2 = usage.
set -euo pipefail

usage() {
    sed -n '2,28p' "$0" >&2
    exit 2
}

BASE_URL="${1:-}"
[ -n "$BASE_URL" ] || usage
shift

EXPECT=""
EXPECT_BUILD_ID=""
TIMEOUT=600
SOAK=300
INTERVAL=15
MAIL_PORTS=""
PROBE_TIMEOUT=180
while [ $# -gt 0 ]; do
    case "$1" in
        --expect-commit) EXPECT=$2; shift 2 ;;
        --expect-build-id) EXPECT_BUILD_ID=$2; shift 2 ;;
        --timeout) TIMEOUT=$2; shift 2 ;;
        --soak) SOAK=$2; shift 2 ;;
        --interval) INTERVAL=$2; shift 2 ;;
        --mail-ports) MAIL_PORTS=$2; shift 2 ;;
        --probe-timeout) PROBE_TIMEOUT=$2; shift 2 ;;
        *) usage ;;
    esac
done
[ -n "$EXPECT" ] || usage

HEALTH_URL="${BASE_URL%/}/api/v1/health"

fetch_health() {
    curl -fsk --max-time 10 "$HEALTH_URL" 2>/dev/null || true
}

json_field() {
    printf '%s' "$1" | sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p"
}

# Exact match, or ≥7 chars on both sides and one a prefix of the other. The
# length floor keeps a locally-built binary's literal "dev" from prefix-matching
# a real sha.
commit_matches() {
    local got=$1
    [ "$got" = "$EXPECT" ] && return 0
    [ ${#got} -ge 7 ] && [ ${#EXPECT} -ge 7 ] || return 1
    case "$got" in "$EXPECT"*) return 0 ;; esac
    case "$EXPECT" in "$got"*) return 0 ;; esac
    return 1
}

# Exact match, and fail-closed on an absent stamp. Deliberately NOT
# prefix-tolerant like the commit conjunct: a build id is stamped whole by one
# producer, so a prefix relation between two of them (run 3078481625 vs
# 30784816252) means they are different builds.
build_id_matches() {
    [ -z "$EXPECT_BUILD_ID" ] && return 0
    [ "$1" = "$EXPECT_BUILD_ID" ]
}

health_ok() {
    local body=$1
    [ "$(json_field "$body" status)" = "ok" ] \
        && commit_matches "$(json_field "$body" commit)" \
        && build_id_matches "$(json_field "$body" build_id)"
}

fail() {
    echo "[serving-gate] GATE FAIL: $*"
    exit 1
}

echo "[serving-gate] waiting up to ${TIMEOUT}s for $HEALTH_URL: status=ok commit=$EXPECT${EXPECT_BUILD_ID:+ build_id=$EXPECT_BUILD_ID}"
if [ -z "$EXPECT_BUILD_ID" ]; then
    # Loud, because commit-only is the weaker gate: it cannot tell two builds
    # of one commit apart. The pipeline always passes --expect-build-id; this
    # branch is ad-hoc session use, or an artifact built before the stamp.
    echo "[serving-gate] NOTE: no --expect-build-id given — matching on commit only," \
         "which cannot distinguish two builds of the same commit"
fi
deadline=$(( $(date +%s) + TIMEOUT ))
while :; do
    body=$(fetch_health)
    if health_ok "$body"; then
        break
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
        fail "after ${TIMEOUT}s health is '$(printf '%s' "$body" | head -c 200)' (want status=ok commit=$EXPECT)"
    fi
    sleep "$INTERVAL"
done
echo "[serving-gate] target serves the expected ${EXPECT_BUILD_ID:+build (}commit${EXPECT_BUILD_ID:+)}; soaking ${SOAK}s (interval ${INTERVAL}s)"

soak_end=$(( $(date +%s) + SOAK ))
while [ "$(date +%s)" -lt "$soak_end" ]; do
    sleep "$INTERVAL"
    body=$(fetch_health)
    if ! health_ok "$body"; then
        fail "flap during soak: health is '$(printf '%s' "$body" | head -c 200)'"
    fi
done

# TLS mail-port banner probes. Each port class gets the dialogue that makes the
# server speak, stdin held open so s_client survives to read the reply (the
# "TLS ports need stdin held" gotcha); output to a file, read afterward.
probe_port() {
    local host=$1 port=$2 out=$3
    case "$port" in
        993)
            { printf 'a LOGOUT\r\n'; sleep 6; } \
                | timeout 20 openssl s_client -quiet -connect "$host:$port" >"$out" 2>&1 || true
            grep -q '^\* OK' "$out"
            ;;
        465)
            { printf 'QUIT\r\n'; sleep 6; } \
                | timeout 20 openssl s_client -quiet -connect "$host:$port" >"$out" 2>&1 || true
            grep -q '^220 ' "$out"
            ;;
        587 | 25)
            { printf 'EHLO serving-gate\r\nQUIT\r\n'; sleep 6; } \
                | timeout 20 openssl s_client -quiet -starttls smtp -connect "$host:$port" >"$out" 2>&1 || true
            grep -q '^250' "$out"
            ;;
        *)
            { sleep 6; } \
                | timeout 20 openssl s_client -quiet -connect "$host:$port" >"$out" 2>&1 || true
            # Any TLS session at all: s_client prints handshake errors on failure
            # and nothing forbids an empty banner, so assert the connect line.
            ! grep -qi 'connect:errno\|Connection refused' "$out"
            ;;
    esac
}

if [ -n "$MAIL_PORTS" ]; then
    probe_dir=$(mktemp -d)
    trap 'rm -rf "$probe_dir"' EXIT
    for hp in ${MAIL_PORTS//,/ }; do
        host=${hp%:*}
        port=${hp##*:}
        echo "[serving-gate] probing $host:$port"
        probe_deadline=$(( $(date +%s) + PROBE_TIMEOUT ))
        out="$probe_dir/probe-$port"
        until probe_port "$host" "$port" "$out"; do
            if [ "$(date +%s)" -ge "$probe_deadline" ]; then
                echo "[serving-gate] last probe output for $host:$port:"
                head -c 500 "$out" || true
                fail "$host:$port not serving after ${PROBE_TIMEOUT}s"
            fi
            sleep "$INTERVAL"
        done
    done
fi

echo "[serving-gate] GATE PASS: $BASE_URL serves commit $EXPECT${EXPECT_BUILD_ID:+ (build $EXPECT_BUILD_ID)} healthily${MAIL_PORTS:+ + mail ports ${MAIL_PORTS}}"
