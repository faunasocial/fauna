#!/usr/bin/env bash
# The iOS App Store build and upload, one command (installers/ios.md § Upload
# mechanics, steps 1–3; release-integrity.md § Release signing → *A store upload
# is built from a recorded public commit*).
#
#   just ios-store-upload <version> [export|upload]
#
# 1. A clean scratch clone of the PUBLIC repository at tag `v<version>` into this
#    checkout's ignored `target/ios-store-<version>/` — never the private tree.
#    An existing clone is reused only when it is still clean and at that tag
#    (the store-safe FFI build is ~30 min cold, so a re-run keeps it).
# 2. That clone's own `just apple-ffi-store-safe`, under a build slot.
# 3. The signed Release archive of the `Fauna-iOS` scheme with the two excision
#    flags `just apple-ios-store-safe-check` passes. Signing is manual from the
#    profiles on disk: no `-allowProvisioningUpdates`, ever.
# 4. `-exportArchive` with the clone's tracked, export-only
#    `ExportOptions-AppStore.plist` (the `app-store-connect` method), then
#    `codesign --verify` of all four bundles, each signed by the distribution
#    identity, version `<version>` build `1`, and the `.ipa`'s sha256.
# 5. `upload` only: a SCRATCH copy of those options with `destination = upload`
#    and `uploadSymbols = true` — the tracked file is never edited
#    (test_apple_identifier_pins.py pins it export-only) — and the exporter's
#    upload destination, authenticated with the App Store Connect API key read as
#    `just ios-export-compliance` reads it: the FAUNA_NOTARY_KEY_FILE / _KEY_ID /
#    _ISSUER triple when all three are set, else the local store
#    `~/.config/fauna/asc/` (exactly one `AuthKey_<id>.p8` and a one-line
#    `issuer`). Then the `just store-review` lines are PRINTED, never run: the
#    record is written from what the console shows (store-review.md § The record
#    → *The writing rule*).
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
BUNDLES=(
    "Fauna-iOS.app"
    "Fauna-iOS.app/PlugIns/Fauna-iOS-FileProvider.appex"
    "Fauna-iOS.app/PlugIns/Fauna-iOS-FileProviderUI.appex"
    "Fauna-iOS.app/PlugIns/Fauna-iOS-Widget.appex"
)

say() { echo "ios-store-upload: $*"; }
die() { local code="$1"; shift; echo "ios-store-upload: $*" >&2; exit "$code"; }

VERSION="${1:-}"
MODE="${2:-export}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die 2 "usage FAILED — the version is MAJOR.MINOR.PATCH (e.g. 0.1.6), got '${VERSION}'"
case "$MODE" in
    export|upload) ;;
    *) die 2 "usage FAILED — the mode is export or upload, got '${MODE}'" ;;
esac
TAG="v${VERSION}"
ROOT="$(git rev-parse --show-toplevel)"
CLONE="${ROOT}/target/ios-store-${VERSION}"
OUT="${ROOT}/target/ios-store-${VERSION}.out"
SLOT=(uv run --no-project python "${ROOT}/scripts/build-slot.py" --pool build)
LOGS="${OUT}/logs"

# --- step 1: the source tree is the public commit at the tag -----------------
clean_at_tag() {  # $1 = clone; succeeds when HEAD is exactly $TAG and nothing tracked changed
    local at
    at="$(git -C "$1" describe --tags --exact-match HEAD 2>/dev/null || true)"
    [ "$at" = "$TAG" ] || { echo "HEAD is '${at:-not a tag}', not ${TAG}"; return 1; }
    [ -z "$(git -C "$1" status --porcelain "${@:2}")" ] || { echo "the checkout is dirty"; return 1; }
}
if [ -d "$CLONE/.git" ]; then
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
PBXPROJ="${CLONE}/apps/fauna-apple/Fauna.xcodeproj/project.pbxproj"
versions="$(grep -o 'MARKETING_VERSION = [^;]*;' "$PBXPROJ" | sed 's/MARKETING_VERSION = //; s/;//' | sort -u | tr '\n' ' ')"
[ "$versions" = "${VERSION} " ] \
    || die 2 "version FAILED — the tree at ${TAG} carries MARKETING_VERSION ${versions:-none}, not ${VERSION}"
say "version OK — MARKETING_VERSION ${VERSION}"

rm -rf "$OUT"; mkdir -p "$LOGS"

# --- step 2: the store-safe device FFI, built by the clone's own recipe ------
"${SLOT[@]}" --disk-gb 16 -- just --justfile "$CLONE/justfile" --working-directory "$CLONE" \
        apple-ffi-store-safe >"$LOGS/ffi.log" 2>&1 \
    || { tail -40 "$LOGS/ffi.log" >&2; die 1 "ffi FAILED — the clone's apple-ffi-store-safe; log ${LOGS}/ffi.log"; }
say "ffi OK — apple-ffi-store-safe in the clone"

# --- step 3: the signed store-safe archive -----------------------------------
ARCHIVE="${OUT}/${SCHEME}.xcarchive"
"${SLOT[@]}" --disk-gb 10 -- xcodebuild archive \
        -project "${CLONE}/apps/fauna-apple/Fauna.xcodeproj" \
        -scheme "$SCHEME" \
        -configuration Release \
        -destination generic/platform=iOS \
        -archivePath "$ARCHIVE" \
        -skipPackagePluginValidation -skipMacroValidation \
        "OTHER_SWIFT_FLAGS=${EXCISE_FLAGS}" >"$LOGS/archive.log" 2>&1 \
    || { tail -40 "$LOGS/archive.log" >&2; die 1 "archive FAILED — log ${LOGS}/archive.log"; }
say "archive OK — ${ARCHIVE}"

# --- step 4: export to disk and verify every bundle --------------------------
EXPORT_OPTIONS="${CLONE}/apps/fauna-apple/ExportOptions-AppStore.plist"
xcodebuild -exportArchive \
        -archivePath "$ARCHIVE" \
        -exportPath "${OUT}/export" \
        -exportOptionsPlist "$EXPORT_OPTIONS" >"$LOGS/export.log" 2>&1 \
    || { tail -40 "$LOGS/export.log" >&2; die 1 "export FAILED — log ${LOGS}/export.log"; }
shopt -s nullglob
ipas=("${OUT}"/export/*.ipa)
[ "${#ipas[@]}" -eq 1 ] || die 1 "export FAILED — expected one .ipa in ${OUT}/export, found ${#ipas[@]}"
IPA="${ipas[0]}"
ditto -x -k "$IPA" "${OUT}/ipa"
PAYLOAD="${OUT}/ipa/Payload"
codesign --verify --deep --strict "${PAYLOAD}/Fauna-iOS.app" 2>"$LOGS/verify.log" \
    || die 1 "verify FAILED — codesign --verify of Fauna-iOS.app; log ${LOGS}/verify.log"
for rel in "${BUNDLES[@]}"; do
    bundle="${PAYLOAD}/${rel}"
    [ -d "$bundle" ] || die 1 "verify FAILED — ${rel} is missing from the .ipa"
    codesign --verify --strict "$bundle" 2>>"$LOGS/verify.log" || die 1 "verify FAILED — ${rel} does not verify"
    authority="$(codesign -dvv "$bundle" 2>&1 | sed -n 's/^Authority=//p' | head -1)"
    [ "$authority" = "$IDENTITY" ] || die 1 "verify FAILED — ${rel} is signed by '${authority}', not '${IDENTITY}'"
    short="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "${bundle}/Info.plist")"
    build="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "${bundle}/Info.plist")"
    [ "$short" = "$VERSION" ] && [ "$build" = "1" ] \
        || die 1 "verify FAILED — ${rel} is version ${short} build ${build}, not ${VERSION} build 1"
done
SHA="$(shasum -a 256 "$IPA" | cut -d' ' -f1)"
say "export OK — ${#BUNDLES[@]} bundles signed by ${IDENTITY}, version ${VERSION} build 1; ${IPA} sha256 ${SHA}"

[ "$MODE" = "upload" ] || { say "done — export only; nothing left this machine"; exit 0; }

# --- step 5: upload through the exporter, with a scratch options copy --------
if [ -n "${FAUNA_NOTARY_KEY_FILE:-}" ]; then
    [ -n "${FAUNA_NOTARY_KEY_ID:-}" ] && [ -n "${FAUNA_NOTARY_ISSUER:-}" ] \
        || die 3 "key FAILED — FAUNA_NOTARY_KEY_FILE is set without FAUNA_NOTARY_KEY_ID and FAUNA_NOTARY_ISSUER"
    KEY_FILE="$FAUNA_NOTARY_KEY_FILE"; KEY_ID="$FAUNA_NOTARY_KEY_ID"; ISSUER="$FAUNA_NOTARY_ISSUER"
    key_source="environment"
else
    STORE="${HOME}/.config/fauna/asc"
    keys=("${STORE}"/AuthKey_*.p8)
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

UPLOAD_OPTIONS="${OUT}/ExportOptions-upload.plist"
cp "$EXPORT_OPTIONS" "$UPLOAD_OPTIONS"
plutil -replace destination -string upload "$UPLOAD_OPTIONS"
plutil -replace uploadSymbols -bool YES "$UPLOAD_OPTIONS"
xcodebuild -exportArchive \
        -archivePath "$ARCHIVE" \
        -exportPath "${OUT}/upload" \
        -exportOptionsPlist "$UPLOAD_OPTIONS" \
        -authenticationKeyPath "$KEY_FILE" \
        -authenticationKeyID "$KEY_ID" \
        -authenticationKeyIssuerID "$ISSUER" >"$LOGS/upload.log" 2>&1 \
    || { tail -40 "$LOGS/upload.log" >&2; die 1 "upload FAILED — App Store Connect refused it; log ${LOGS}/upload.log"; }
say "upload OK — ${TAG} build 1 handed to App Store Connect (sha256 ${SHA}); log ${LOGS}/upload.log"

today="$(date +%Y-%m-%d)"
note="Built from public ${TAG}, store-safe, ipa sha256 ${SHA:0:12}."
cat <<EOF
ios-store-upload: record — once App Store Connect shows the build, run from that console reading:
  just store-review app-store ${VERSION} built --read ${today} --track testflight-internal --build 1 --flavour store-safe --note "${note}"
  just store-review app-store ${VERSION} uploaded --read ${today}
EOF
