#!/bin/sh
# release-nest-image.sh stage <sha-tag> <stage-tag> <version>
# release-nest-image.sh promote <sha-tag>
#
# The public repository's two release acts on the nest image and its mirrored
# sidecars (image-pipeline.md § Image tags & channels → *The UAT channel*, and
# *The two pipelines on GitHub-hosted runners*, parts 1 and 4). One script, two
# call sites, both on the public repository only:
#
#   stage   — UAT entry, run by the `stage` job of build-nest-image.yml and
#             restage-nest.yml once :<sha-tag> is pushed. Refuses a :<version>
#             that already names another digest (staging a numbered build
#             spends the version string; a re-stage of the same build is
#             idempotent), signs every digest keyless, verifies the signatures
#             against this repository's workflow identity, and only then moves
#             :<stage-tag> + :<version> on the nest image and the mirrors.
#   promote — the release, run by the `promote` job inside the `promote-nest`
#             Environment after a green verify. Signs nothing: it refuses any
#             digest the stage did not sign, then copies :latest — the only
#             writer of that tag.
#
# Every digest is resolved from :<sha-tag> first; any absence is a break, never
# a skip, so no channel tag ever names an unsigned manifest.
#
# Needs: a registry login that can write the four packages, cosign on PATH, uv
# (the vetted table lists the mirrors); `stage` also an OIDC token
# (`id-token: write`).
set -eu

MODE="${1:-}"
SHA_TAG="${2:-}"
STAGE_TAG="${3:-}"
VERSION="${4:-}"
usage() {
    sed -n '2,3p' "$0" >&2
    exit 2
}
case "$MODE" in
    stage) [ -n "$SHA_TAG" ] && [ -n "$STAGE_TAG" ] && [ -n "$VERSION" ] || usage ;;
    promote) [ -n "$SHA_TAG" ] && [ -z "$STAGE_TAG" ] || usage ;;
    *) usage ;;
esac

IMAGE="${IMAGE:-ghcr.io/faunasocial/nest}"
REPOSITORY="${GITHUB_REPOSITORY:-faunasocial/fauna}"
ISSUER="https://token.actions.githubusercontent.com"
# The two workflows that run this script, on `main` of this repository only.
REPO_RE=$(printf '%s' "$REPOSITORY" | sed 's/[.]/\\./g')
IDENTITY_RE="^https://github\\.com/$REPO_RE/\\.github/workflows/(build-nest-image|restage-nest)\\.yml@refs/heads/main\$"

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PLAN=$(mktemp)
INSPECT_ERR=$(mktemp)
trap 'rm -f "$PLAN" "$INSPECT_ERR"' EXIT
TAB="$(printf '\t')"

digest_of() {
    docker buildx imagetools inspect "$1" --format '{{.Manifest.Digest}}' </dev/null
}

# Resolve the nest manifest and every mirror at :<sha-tag> to a digest.
resolve() {
    printf '%s\t%s\n' "$IMAGE" "$(digest_of "$IMAGE:$SHA_TAG")" >"$PLAN"
    uv run --no-project python "$REPO_ROOT/scripts/vetted_images.py" --print-mirror-images \
        | cut -f1 | while read -r repo; do
            [ -n "$repo" ] || continue
            printf '%s\t%s\n' "$repo" "$(digest_of "$repo:$SHA_TAG")"
        done >>"$PLAN"
    if [ "$(wc -l <"$PLAN")" -lt 2 ]; then
        echo "release: the vetted table produced no mirrors" >&2
        exit 1
    fi
    while IFS="$TAB" read -r repo digest; do
        case "$digest" in
            sha256:*) echo "[release] $repo:$SHA_TAG is $repo@$digest" ;;
            *) echo "release: could not resolve $repo:$SHA_TAG (got '$digest')" >&2; exit 1 ;;
        esac
    done <"$PLAN"
    NEST_DIGEST=$(head -1 "$PLAN" | cut -f2)
}

# Every resolved digest verifies against this repository's workflow identity.
verify_all() {
    while IFS="$TAB" read -r repo digest; do
        if ! cosign verify "$repo@$digest" \
            --certificate-identity-regexp="$IDENTITY_RE" \
            --certificate-oidc-issuer="$ISSUER" >/dev/null </dev/null; then
            echo "release: $repo@$digest carries no signature from $IDENTITY_RE" >&2
            exit 1
        fi
    done <"$PLAN"
    echo "[release] every signature verifies against $IDENTITY_RE"
}

print_recipe() {
    recipe=$(cat <<EOF
cosign verify $IMAGE:$1 \\
  --certificate-identity-regexp='$IDENTITY_RE' \\
  --certificate-oidc-issuer='$ISSUER'
EOF
)
    echo "[release] $2 Check any of them with:"
    printf '%s\n' "$recipe"
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        {
            echo "### $2"
            echo
            echo "Every manifest (the nest image and its mirrors) is signed by this workflow. Verify:"
            echo
            echo '```sh'
            printf '%s\n' "$recipe"
            echo '```'
        } >>"$GITHUB_STEP_SUMMARY"
    fi
}

do_stage() {
    resolve
    # Staging a numbered build spends the version string: a :<version> that
    # names another digest is refused, an absent one or the same one is fine.
    # Only an explicit not-found counts as absent — a registry error is a red.
    while IFS="$TAB" read -r repo digest; do
        if existing=$(digest_of "$repo:$VERSION" 2>"$INSPECT_ERR"); then
            if [ "$existing" != "$digest" ]; then
                echo "release: $repo:$VERSION already names $existing, not this build's $digest —" \
                    "a staged version is spent; bump the version and stage a new build" >&2
                exit 1
            fi
            echo "[release] $repo:$VERSION already names this build (a re-stage)"
        elif grep -qiE 'not found|manifest unknown' "$INSPECT_ERR"; then
            echo "[release] $repo:$VERSION does not exist yet"
        else
            echo "release: could not tell whether $repo:$VERSION exists: $(tr '\n' ' ' <"$INSPECT_ERR")" >&2
            exit 1
        fi
    done <"$PLAN"

    # Sign each digest keyless (the workflow's Sigstore identity), then prove
    # the signatures verify — the recipe printed at the end — before anything
    # moves.
    while IFS="$TAB" read -r repo digest; do
        echo "[release] signing $repo@$digest"
        cosign sign --yes "$repo@$digest" </dev/null
    done <"$PLAN"
    verify_all

    # Move the UAT tags: the nest image from its resolved digest, the mirrors
    # from their :<sha-tag>, strict — every one was just resolved and signed.
    docker buildx imagetools create \
        -t "$IMAGE:$STAGE_TAG" \
        -t "$IMAGE:$VERSION" \
        "$IMAGE@$NEST_DIGEST"
    "$REPO_ROOT/scripts/mirror-third-party-images.sh" "$STAGE_TAG,$VERSION" "$SHA_TAG"
    print_recipe "$VERSION" "Staged \`$IMAGE:$STAGE_TAG\` + \`:$VERSION\` = \`$NEST_DIGEST\` (and the mirrors), signed."
}

do_promote() {
    resolve
    # The stage signed every digest; one it did not is a break, never signed here.
    verify_all
    docker buildx imagetools create \
        -t "$IMAGE:latest" \
        "$IMAGE@$NEST_DIGEST"
    "$REPO_ROOT/scripts/mirror-third-party-images.sh" "latest" "$SHA_TAG"
    print_recipe "latest" "Promoted \`$IMAGE:latest\` = \`$NEST_DIGEST\` (and the mirrors), signed at stage."
}

case "$MODE" in
    stage) do_stage ;;
    promote) do_promote ;;
esac
