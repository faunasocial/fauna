#!/bin/sh
# Re-publish the vetted third-party container images under Fauna's namespace.
#
# The Fauna-namespace mirror — docs/goal/architecture/release-integrity.md
# § Third-party container images, part 4. A box should pull its whole bundle
# from ONE trust root; today the sidecars come from three third-party accounts.
# This copies each vetted manifest to ghcr.io/faunasocial/<name> so the bundle
# can name only ghcr.io/faunasocial/*, and so a reviewed digest bump reaches
# every box through Fauna's own gates + Watchtower: the private pipeline STAGES the
# mirrors beside the nest image (`sha-<sha>`, then `dev`, before its verify gate);
# the public pipeline's stage moves the signed ones to `test,<version>` and its
# promote copies the verified ones to `latest`, both through
# release-nest-image.sh (image-pipeline.md § Image tags & channels).
#
# It is a MANIFEST COPY: no build, no docker pull, no dependency source read.
#
# Usage:
#   mirror-third-party-images.sh <dest-tags> [<source-tag>] [--allow-all-missing]
#
#   <dest-tags>           comma-separated tags to create on each mirror repo,
#                         e.g. "sha-abc123,dev" (a private stage) or
#                         "test,0.1.4" (the public pipeline's stage) or
#                         "latest" (its promote)
#   <source-tag>          optional. When given, copy from
#                         <mirror-repo>:<source-tag> (the manifest this
#                         pipeline already staged and verified). When omitted,
#                         copy from the vetted THIRD-PARTY digest — the
#                         `@sha256:` ref in the vetted table, never a floating
#                         tag.
#   --allow-all-missing   optional, only meaningful with <source-tag>. Permits
#                         an ALL-missing inspect result (every mirror image is
#                         absent for <source-tag>) to warn and exit 0 instead
#                         of red — the legitimate case is a historical
#                         rollback to a sha built before this mirror step
#                         existed, where there is genuinely nothing to roll
#                         the sidecars back to. Pass it ONLY at a call site
#                         that runs BEFORE its own verify gate: that gate
#                         (bundle-scanner-probe.py --mirror-tag) is what
#                         catches a genuine problem for that case. A call site
#                         running AFTER verify already confirmed <source-tag>
#                         exists must NOT pass it — there, all-missing can
#                         only mean something broke between that verify and
#                         this copy (the public pipeline's promote). A MIXED result (some images present, some
#                         not) is always red regardless of this flag: the
#                         mirror step's existence is one fact per sha, true
#                         for all vetted images at once or none of them, so a
#                         split result is never explained by sha age — only by
#                         a transient per-image registry error, and a partial
#                         copy is worse than a clean failure because
#                         nothing downstream can see the mismatch.
#
# Which images, and from which digest, is read from the vetted table in
# scripts/vetted_images.py (--print-mirror-images), so there is exactly one
# list: the one a human vetted. Never hard-code a second copy here.
set -eu

DEST_TAGS="${1:-}"
SOURCE_TAG="${2:-}"
ALLOW_ALL_MISSING="${3:-}"

if [ -z "$DEST_TAGS" ]; then
    sed -n '2,28p' "$0" >&2
    exit 2
fi

if [ -n "$ALLOW_ALL_MISSING" ] && [ "$ALLOW_ALL_MISSING" != "--allow-all-missing" ]; then
    echo "mirror: unrecognized third argument '$ALLOW_ALL_MISSING'" \
        "(only --allow-all-missing is accepted)" >&2
    exit 2
fi

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PLAN=$(mktemp)
INSPECT_ERR=$(mktemp)
EXIST_PLAN=$(mktemp)
trap 'rm -f "$PLAN" "$INSPECT_ERR" "$EXIST_PLAN"' EXIT

uv run --no-project python "$REPO_ROOT/scripts/vetted_images.py" \
    --print-mirror-images >"$PLAN"

# An empty plan would silently mirror nothing and still exit 0, leaving the
# bundle's tags unpublished — fail loudly instead.
if [ ! -s "$PLAN" ]; then
    echo "mirror: the vetted table produced an EMPTY mirror plan" >&2
    exit 1
fi

# Pass 1: resolve which sources exist, without creating anything yet — so a
# mixed or (ungated) all-missing result (checked below) can refuse the WHOLE
# copy instead of partially completing it before the gate is reached.
skipped=0
total=0
while IFS="$(printf '\t')" read -r repo vetted_source; do
    [ -n "$repo" ] || continue
    total=$((total + 1))

    if [ -n "$SOURCE_TAG" ]; then
        source="$repo:$SOURCE_TAG"
        # A sha built before the mirror step existed has no mirror tags — but
        # that is only DISTINGUISHABLE from a registry error (token expiry,
        # rate-limit, transient 5xx) by the caller's own context, checked
        # below after this pass. Capture inspect's stderr either way: today a
        # real registry error here is invisible in the run log.
        if ! docker buildx imagetools inspect "$source" >/dev/null 2>"$INSPECT_ERR"; then
            echo "::warning::mirror: $source does not exist — skipping (a build" \
                "from before the mirror step, or a table entry added since)." \
                "inspect said: $(tr '\n' ' ' <"$INSPECT_ERR")"
            skipped=$((skipped + 1))
            continue
        fi
    else
        source="$vetted_source"
    fi

    printf '%s\t%s\n' "$repo" "$source" >>"$EXIST_PLAN"
done <"$PLAN"

if [ -n "$SOURCE_TAG" ] && [ "$skipped" -gt 0 ]; then
    existing=$((total - skipped))
    if [ "$existing" -gt 0 ]; then
        # The mirror step's existence is one fact per sha, true for every
        # vetted image at once or none of them — a split result is never
        # explained by "this sha predates the mirror step", only by a
        # transient per-image registry error. Refuse ALL of them rather than
        # move :{$DEST_TAGS} for some sidecars and not others: a mixed bundle
        # is worse than a clean failure because nothing downstream can see it.
        echo "mirror: MIXED result for $SOURCE_TAG — $existing of $total source(s)" \
            "existed, $skipped did not. Refusing to copy ANY of them." >&2
        exit 1
    fi
    if [ "$ALLOW_ALL_MISSING" != "--allow-all-missing" ]; then
        echo "mirror: all $total source(s) for $SOURCE_TAG were missing, and this" \
            "call site requires them to exist. Pass --allow-all-missing only at a" \
            "call site that runs BEFORE its own verify gate (a historical rollback" \
            "re-stage) — see the script header." >&2
        exit 1
    fi
fi

# Pass 2: create. Only reached once pass 1 proved every remaining entry either
# exists, or absence is sanctioned (the all-missing carve-out above).
mirrored=0
while IFS="$(printf '\t')" read -r repo source; do
    [ -n "$repo" ] || continue

    # shellcheck disable=SC2086 # DEST_TAGS is deliberately word-split on commas
    tag_args=""
    saved_ifs="$IFS"
    IFS=,
    for tag in $DEST_TAGS; do
        tag_args="$tag_args -t $repo:$tag"
    done
    IFS="$saved_ifs"

    echo "[mirror] $source -> $repo:{$DEST_TAGS}"
    # shellcheck disable=SC2086 # tag_args is a built-up argument list
    docker buildx imagetools create $tag_args "$source"
    mirrored=$((mirrored + 1))
done <"$EXIST_PLAN"

echo "[mirror] $mirrored image(s) published to ghcr.io/faunasocial as {$DEST_TAGS}"
