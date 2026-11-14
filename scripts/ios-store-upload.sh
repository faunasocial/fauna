#!/usr/bin/env bash
# The iOS App Store build, signing and upload, in two halves (installers/ios.md
# § Upload mechanics; installers/store-review.md § The store legs, *How the iOS
# store app is signed*; release-integrity.md § Release signing → *A store upload
# is built from a recorded public commit*).
#
#   just ios-store-upload <version> [export|upload|build|sign] [--tree DIR]
#        [--out DIR] [--profiles-dir DIR] [--keychain KC]
#
# The BUILD half (`build`), which holds no credential:
# 1. The source tree. By default a clean scratch clone of the PUBLIC repository
#    at tag `v<version>` into this checkout's ignored `target/ios-store-<version>/`
#    — never the private tree; an existing clone is reused only when it is still
#    clean and at that tag (the store-safe FFI build is ~30 min cold, so a re-run
#    keeps it). With `--tree DIR` (the release workflow's checkout, whose commit
#    the workflow has already checked) that clean tree instead.
# 2. That tree's own `just apple-ffi-store-safe`, under a build slot where this
#    box has the slot tool.
# 3. The UNSIGNED Release archive of the `Fauna-iOS` scheme (`CODE_SIGNING_ALLOWED=NO`)
#    with the two excision flags `just apple-ios-store-safe-check` passes; the
#    archived app goes to `<out>/unsigned/Fauna-iOS.app` (with the archive's
#    `Signatures/` beside it), its debug symbols to `<out>/dSYMs/`. No exporter:
#    nothing signs here.
#
# The SIGN half (`sign`), in the approved job or on macOS:
# 4. `scripts/ios-store-sign.py` — the tree's own signer — signs the unsigned
#    app bundle by bundle with the distribution identity over entitlements it
#    computes from the built tree's project, packages `<out>/Fauna-iOS.ipa` and
#    reads every bundle back. The profiles are `--profiles-dir`'s
#    `*.mobileprovision` (the workflow's, decoded from its secrets), else the ones
#    installed for Xcode; `--keychain` names the identity's keychain.
#
# `export` (the default) runs both halves; `send` uploads the package an earlier
# sign half left in `<out>` (the release workflow's step, after it has asked App
# Store Connect whether it already holds the build); `upload` also reads the
# acceptance set green first, runs both halves and then hands the signer's
# package to Apple's own upload
# tool (`xcrun altool --upload-package`, which validates it before it uploads),
# authenticated with an App Store Connect API key: the FAUNA_ASC_STORE_KEY_FILE /
# _KEY_ID / _ISSUER triple when set (the store key), else the FAUNA_NOTARY_KEY_FILE
# / _KEY_ID / _ISSUER triple, else the local store `~/.config/fauna/asc/`
# (exactly one `AuthKey_<id>.p8` and a one-line `issuer`). Then the
# `just store-review` lines are PRINTED, never run: the record is written from
# what the console shows (store-review.md § The record → *The writing rule*).
#
# One `ios-store-upload: <step> OK|FAILED — …` verdict line per step. Exit 0 on
# success, 2 on a usage error or a refused source tree, 1 on a failed step,
# 3 when `upload` finds no App Store Connect key on this box.
# Pinned by tests/e2e-unified/tests/test_ios_store_upload_pins.py.
set -euo pipefail

PUBLIC_REMOTE="https://github.com/faunasocial/fauna.git"
TEAM_ID="7457N3M72H"
IDENTITY="Apple Distribution: Fauna Social (${TEAM_ID})"
SCHEME="Fauna-iOS"
EXCISE_FLAGS='$(inherited) -D FAUNA_EXCISE_PAYMENTS -D FAUNA_EXCISE_P2P_SHARE'

say() { echo "ios-store-upload: $*"; }
die() { local code="$1"; shift; echo "ios-store-upload: $*" >&2; exit "$code"; }

VERSION="${1:-}"
MODE="${2:-export}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die 2 "usage FAILED — the version is MAJOR.MINOR.PATCH (e.g. 0.1.6), got '${VERSION}'"
case "$MODE" in
    export|upload|build|sign|send) ;;
    *) die 2 "usage FAILED — the mode is export, upload, build, sign or send, got '${MODE}'" ;;
esac
shift $(( $# < 2 ? $# : 2 ))
TREE_ARG="" OUT_ARG="" PROFILES_DIR="" KEYCHAIN=""
while [ $# -gt 0 ]; do
    case "$1" in
        --tree) TREE_ARG="${2:?--tree needs a directory}"; shift 2 ;;
        --out) OUT_ARG="${2:?--out needs a directory}"; shift 2 ;;
        --profiles-dir) PROFILES_DIR="${2:?--profiles-dir needs a directory}"; shift 2 ;;
        --keychain) KEYCHAIN="${2:?--keychain needs a keychain}"; shift 2 ;;
        *) die 2 "usage FAILED — unknown argument '$1'" ;;
    esac
done
TAG="v${VERSION}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLONE="${TREE_ARG:-${ROOT}/target/ios-store-${VERSION}}"
OUT="${OUT_ARG:-${ROOT}/target/ios-store-${VERSION}.out}"
LOGS="${OUT}/logs"
UNSIGNED_APP="${OUT}/unsigned/Fauna-iOS.app"
IPA="${OUT}/Fauna-iOS.ipa"
# The slot tool exists on the dev machines only; the release workflow's runner is its own machine.
SLOT_TOOL="${ROOT}/scripts/build-slot.py"
slot() { local gb="$1"; shift
    if [ -f "$SLOT_TOOL" ]; then uv run --no-project python "$SLOT_TOOL" --pool build --disk-gb "$gb" -- "$@"; else "$@"; fi; }

# --- step 0 (upload only): the acceptance set is green before the string is spent
# An accepted upload spends the version at the store (product-version.md § Bumping,
# *A release is never given up*): the promote's acceptance set is read green first,
# off the release record the project's own checkout keeps. A checkout without that
# release tooling has no record to read, and uploads nothing on the project's behalf.
ACCEPTANCE_GATE="${ROOT}/scripts/publish/promote_ready.py"
if [ "$MODE" = "upload" ] && [ -f "$ACCEPTANCE_GATE" ]; then
    uv run --no-project python "$ACCEPTANCE_GATE" "$VERSION" --gate \
        || die 2 "acceptance FAILED — the acceptance set is not green at ${TAG}'s candidate; read it with \`fauna-publish promote-ready ${VERSION}\` and fix any red inside ${VERSION}"
    say "acceptance OK — the acceptance set is green at ${TAG}'s candidate"
fi

# --- step 1: the source tree is the public commit at the tag -----------------
clean_at_tag() {  # $1 = clone; succeeds when HEAD is exactly $TAG and nothing tracked changed
    local at
    at="$(git -C "$1" describe --tags --exact-match HEAD 2>/dev/null || true)"
    [ "$at" = "$TAG" ] || { echo "HEAD is '${at:-not a tag}', not ${TAG}"; return 1; }
    [ -z "$(git -C "$1" status --porcelain "${@:2}")" ] || { echo "the checkout is dirty"; return 1; }
}
if [ -n "$TREE_ARG" ]; then
    [ -d "$CLONE/.git" ] || [ -f "$CLONE/.git" ] || die 2 "tree FAILED — ${CLONE} is not a git checkout"
    [ -z "$(git -C "$CLONE" status --porcelain --untracked-files=no)" ] \
        || die 2 "tree FAILED — ${CLONE} has tracked changes"
    say "tree OK — ${CLONE} at $(git -C "$CLONE" rev-parse --short=12 HEAD), clean"
elif [ "$MODE" = "sign" ] || [ "$MODE" = "send" ]; then
    [ -d "$CLONE/.git" ] || die 2 "tree FAILED — no built tree at ${CLONE}; run the build half first"
elif [ -d "$CLONE/.git" ]; then
    why="$(clean_at_tag "$CLONE" --untracked-files=no)" \
        || die 2 "clone FAILED — ${CLONE} exists but ${why}; remove it and run again"
    say "clone OK — reusing ${CLONE} (clean, at ${TAG})"
else
    rm -rf "$CLONE"
    git clone --quiet --depth 1 --branch "$TAG" "$PUBLIC_REMOTE" "$CLONE" \
        || die 1 "clone FAILED — no ${TAG} on ${PUBLIC_REMOTE}"
    why="$(clean_at_tag "$CLONE")" || die 2 "clone FAILED — ${why}"
    say "clone OK — ${PUBLIC_REMOTE} ${TAG} at $(git -C "$CLONE" rev-parse --short=12 HEAD) in ${CLONE}"
fi
PROJECT="${CLONE}/apps/fauna-apple/Fauna.xcodeproj"
versions="$(grep -o 'MARKETING_VERSION = [^;]*;' "${PROJECT}/project.pbxproj" | sed 's/MARKETING_VERSION = //; s/;//' | sort -u | tr '\n' ' ')"
[ "$versions" = "${VERSION} " ] \
    || die 2 "version FAILED — the tree carries MARKETING_VERSION ${versions:-none}, not ${VERSION}"
say "version OK — MARKETING_VERSION ${VERSION}"

build_half() {
    rm -rf "$OUT"; mkdir -p "$LOGS"

    # --- step 2: the store-safe device FFI, built by the tree's own recipe ---
    slot 16 just --justfile "$CLONE/justfile" --working-directory "$CLONE" \
            apple-ffi-store-safe >"$LOGS/ffi.log" 2>&1 \
        || { tail -40 "$LOGS/ffi.log" >&2; die 1 "ffi FAILED — the tree's apple-ffi-store-safe; log ${LOGS}/ffi.log"; }
    say "ffi OK — apple-ffi-store-safe in the tree"

    # --- step 3: the UNSIGNED store-safe archive ------------------------------
    local archive="${OUT}/${SCHEME}.xcarchive"
    slot 10 xcodebuild archive \
            -project "$PROJECT" \
            -scheme "$SCHEME" \
            -configuration Release \
            -destination generic/platform=iOS \
            -archivePath "$archive" \
            -skipPackagePluginValidation -skipMacroValidation \
            CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO \
            "OTHER_SWIFT_FLAGS=${EXCISE_FLAGS}" >"$LOGS/archive.log" 2>&1 \
        || { tail -40 "$LOGS/archive.log" >&2; die 1 "archive FAILED — log ${LOGS}/archive.log"; }
    local app="${archive}/Products/Applications/Fauna-iOS.app"
    [ -d "$app" ] || die 1 "archive FAILED — no Fauna-iOS.app in ${archive}"
    mkdir -p "${OUT}/unsigned"
    ditto "$app" "$UNSIGNED_APP"
    # The licence texts and the app's notices file at the bundle's root — an
    # iOS bundle's resource directory — staged before the signer seals the
    # bundle (shipped-notices.md § Per channel, the iOS row), by the TREE's own
    # recipe and generator, so the notices describe the tree that was built (the
    # manifest's `ios-app`: the store-safe device FFI). A generator that refuses
    # fails the build half.
    just --justfile "$CLONE/justfile" --working-directory "$CLONE" \
            shipped-notices-stage ios-app "$UNSIGNED_APP" >"$LOGS/notices.log" 2>&1 \
        || { tail -40 "$LOGS/notices.log" >&2; die 1 "notices FAILED — the tree's shipped-notices-stage ios-app; log ${LOGS}/notices.log"; }
    for f in LICENSE-MIT LICENSE-APACHE THIRD-PARTY-NOTICES; do
        [ -s "${UNSIGNED_APP}/${f}" ] || die 1 "notices FAILED — the unsigned app lacks ${f}"
    done
    say "notices OK — the licence texts and the notices file in ${UNSIGNED_APP}"
    ditto "${archive}/dSYMs" "${OUT}/dSYMs"
    if [ -d "${archive}/Signatures" ]; then ditto "${archive}/Signatures" "${OUT}/unsigned/Signatures"; fi
    say "archive OK — unsigned ${UNSIGNED_APP}; symbols ${OUT}/dSYMs"
}

sign_half() {
    [ -d "$UNSIGNED_APP" ] || die 1 "sign FAILED — no unsigned app at ${UNSIGNED_APP}; run the build half first"
    mkdir -p "$LOGS"
    local dir="${PROFILES_DIR:-${HOME}/Library/Developer/Xcode/UserData/Provisioning Profiles}"
    local profiles=() p
    shopt -s nullglob
    for p in "$dir"/*.mobileprovision; do profiles+=(--profile "$p"); done
    shopt -u nullglob
    [ "${#profiles[@]}" -gt 0 ] || die 1 "sign FAILED — no *.mobileprovision in ${dir}"
    local keychain=() signatures=()
    [ -z "$KEYCHAIN" ] || keychain=(--keychain "$KEYCHAIN")
    [ ! -d "${OUT}/unsigned/Signatures" ] || signatures=(--signatures "${OUT}/unsigned/Signatures")
    if ! uv run --no-project python "${ROOT}/scripts/ios-store-sign.py" \
            --app "$UNSIGNED_APP" --project "$PROJECT" --identity "$IDENTITY" \
            "${profiles[@]}" "${keychain[@]+"${keychain[@]}"}" "${signatures[@]+"${signatures[@]}"}" \
            --version "$VERSION" --out "$IPA" 2>&1 | tee "$LOGS/sign.log"; then
        die 1 "sign FAILED — the signer refused; log ${LOGS}/sign.log"
    fi
    say "sign OK — ${IPA}"
}

# --- step 5: upload the signer's package with Apple's own tool ---------------
send_half() {
[ -f "$IPA" ] || die 1 "upload FAILED — no signed package at ${IPA}; run the sign half first"
if [ -n "${FAUNA_ASC_STORE_KEY_FILE:-}" ]; then
    [ -n "${FAUNA_ASC_STORE_KEY_ID:-}" ] && [ -n "${FAUNA_ASC_STORE_ISSUER:-}" ] \
        || die 3 "key FAILED — FAUNA_ASC_STORE_KEY_FILE is set without FAUNA_ASC_STORE_KEY_ID and FAUNA_ASC_STORE_ISSUER"
    KEY_FILE="$FAUNA_ASC_STORE_KEY_FILE"; KEY_ID="$FAUNA_ASC_STORE_KEY_ID"; ISSUER="$FAUNA_ASC_STORE_ISSUER"
    key_source="the store key's environment"
elif [ -n "${FAUNA_NOTARY_KEY_FILE:-}" ]; then
    [ -n "${FAUNA_NOTARY_KEY_ID:-}" ] && [ -n "${FAUNA_NOTARY_ISSUER:-}" ] \
        || die 3 "key FAILED — FAUNA_NOTARY_KEY_FILE is set without FAUNA_NOTARY_KEY_ID and FAUNA_NOTARY_ISSUER"
    KEY_FILE="$FAUNA_NOTARY_KEY_FILE"; KEY_ID="$FAUNA_NOTARY_KEY_ID"; ISSUER="$FAUNA_NOTARY_ISSUER"
    key_source="environment"
else
    STORE="${HOME}/.config/fauna/asc"
    shopt -s nullglob
    keys=("${STORE}"/AuthKey_*.p8)
    shopt -u nullglob
    [ "${#keys[@]}" -eq 1 ] \
        || die 3 "key FAILED — ${STORE} holds ${#keys[@]} AuthKey_<id>.p8 files (needs exactly one) and FAUNA_NOTARY_KEY_FILE is unset"
    KEY_FILE="${keys[0]}"
    KEY_ID="$(basename "$KEY_FILE" .p8)"; KEY_ID="${KEY_ID#AuthKey_}"
    [ -r "${STORE}/issuer" ] || die 3 "key FAILED — ${STORE}/issuer is missing (the key's issuer id, one line)"
    ISSUER="$(tr -d '[:space:]' <"${STORE}/issuer")"
    key_source="the local store"
fi
[ -r "$KEY_FILE" ] || die 3 "key FAILED — ${KEY_FILE} is not readable"
say "key OK — App Store Connect key ${KEY_ID} from ${key_source}"

SHA="$(shasum -a 256 "$IPA" | cut -d' ' -f1)"
xcrun altool --upload-package "$IPA" \
        --api-key "$KEY_ID" --api-issuer "$ISSUER" --p8-file-path "$KEY_FILE" >"$LOGS/upload.log" 2>&1 \
    || { tail -40 "$LOGS/upload.log" >&2; die 1 "upload FAILED — App Store Connect refused it; log ${LOGS}/upload.log"; }
say "upload OK — ${TAG} handed to App Store Connect (sha256 ${SHA}); log ${LOGS}/upload.log"

today="$(date +%Y-%m-%d)"
# No hash in the note: installer/store-review.toml ships, and the identifier
# ratchet refuses a hex digest in a shipped file (the .ipa sha256 is printed
# above, for the session's own record).
note="Built from public ${TAG}, store-safe."
cat <<EOF
ios-store-upload: record — once App Store Connect shows the build, run from that console reading:
  just store-review app-store ${VERSION} built --read ${today} --track testflight-internal --build 1 --flavour store-safe --note "${note}"
  just store-review app-store ${VERSION} uploaded --read ${today}
EOF
}

case "$MODE" in
    build) build_half; say "done — build half only; nothing signed"; exit 0 ;;
    sign) sign_half; say "done — sign half only; nothing left this machine"; exit 0 ;;
    export) build_half; sign_half; say "done — export only; nothing left this machine"; exit 0 ;;
    send) send_half ;;
    upload) build_half; sign_half; send_half ;;
esac
