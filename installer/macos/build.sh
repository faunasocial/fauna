#!/bin/bash
# Build Fauna.pkg — the Fauna macOS installer — for macOS (Apple Silicon).
#
# Usage:
#   ./installer/macos/build.sh                    # unsigned (dev/testing)
#   ./installer/macos/build.sh --sign             # signed + notarized (release)
#   ./installer/macos/build.sh --sign-only        # signed, NOT notarized
#   ./installer/macos/build.sh --profile dist     # shipping-sized build (any of the above)
#
# --sign-only produces a Developer-ID-signed .pkg without notarization: the
# TCC measurement matrix (docs/goal/architecture/installers/macos.md
# § Identifier domain) needs a *signed* artifact, and signing needs no notary
# credential. A locally-built .pkg carries no quarantine xattr, so it installs
# without Gatekeeper involvement; only a downloaded copy needs notarization.
#
# --profile (default `release`): the cargo PROFILE every Rust/Go binary this
# script builds is compiled at. `dist` is the size-optimized shipping variant
# (installers/macos.md § Size & build profile) — the
# same axis `just mac-app`/`mac-release`/`mac-dmg` already accept; this flag
# threads it through this script's OWN cargo invocations (the four service
# binaries) and the mail bridge's Go/cdylib build, which `mac-app` does not
# reach. `pkg`/`pkg-unsigned`/`pkg-sign-only` (justfile) forward their own
# `profile` parameter here.
#
# Required env vars for --sign (--sign-only needs only the first two):
#   FAUNA_SIGN_IDENTITY       Developer ID Application cert name
#   FAUNA_INSTALLER_IDENTITY  Developer ID Installer cert name
#   FAUNA_APPLE_ID            Apple ID for notarization
#   FAUNA_TEAM_ID             Apple Developer Team ID
#   FAUNA_NOTARY_PASSWORD     App-specific password for notarytool
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALLER_DIR="$REPO_ROOT/installer/macos"
BUILD_DIR="$REPO_ROOT/build/pkg"
SIGN=false
NOTARIZE=false
PROFILE=release

# A real flag loop (not a single-positional `case "${1:-}"`, which this script
# carried until): --profile takes a value, and --sign /
# --sign-only must still combine with it in one invocation.
while [ $# -gt 0 ]; do
    case "$1" in
        --sign)
            SIGN=true
            NOTARIZE=true
            : "${FAUNA_SIGN_IDENTITY:?Set FAUNA_SIGN_IDENTITY}"
            : "${FAUNA_INSTALLER_IDENTITY:?Set FAUNA_INSTALLER_IDENTITY}"
            : "${FAUNA_APPLE_ID:?Set FAUNA_APPLE_ID}"
            : "${FAUNA_TEAM_ID:?Set FAUNA_TEAM_ID}"
            : "${FAUNA_NOTARY_PASSWORD:?Set FAUNA_NOTARY_PASSWORD}"
            shift
            ;;
        --sign-only)
            SIGN=true
            : "${FAUNA_SIGN_IDENTITY:?Set FAUNA_SIGN_IDENTITY}"
            : "${FAUNA_INSTALLER_IDENTITY:?Set FAUNA_INSTALLER_IDENTITY}"
            shift
            ;;
        --profile)
            [ $# -ge 2 ] || { echo "--profile needs a value (release or dist)" >&2; exit 2; }
            PROFILE="$2"
            shift 2
            ;;
        *)
            # An unrecognized flag must fail, not silently build unsigned.
            echo "unknown argument: $1 (expected --sign, --sign-only, or --profile <release|dist>)" >&2
            exit 2
            ;;
    esac
done

case "$PROFILE" in
    release|dist) ;;
    *) echo "--profile must be 'release' or 'dist', not '$PROFILE'" >&2; exit 2 ;;
esac

# Extract version from workspace Cargo.toml
VERSION=$(grep '^version' "$REPO_ROOT/Cargo.toml" | head -1 \
    | sed 's/.*"\(.*\)".*/\1/')
echo "Building Fauna $VERSION"

# Clean build directory
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR/payloads/node/bin"
mkdir -p "$BUILD_DIR/payloads/sync"
mkdir -p "$BUILD_DIR/payloads/bridge"
mkdir -p "$BUILD_DIR/payloads/app"
mkdir -p "$BUILD_DIR/payloads/tui"
mkdir -p "$BUILD_DIR/scripts/nest"
mkdir -p "$BUILD_DIR/scripts/sync"
mkdir -p "$BUILD_DIR/scripts/bridge"
mkdir -p "$BUILD_DIR/resources"
mkdir -p "$BUILD_DIR/components"

# --- Step 1: Compile binaries ---
echo "==> Compiling Rust binaries..."
# fauna-nest-daemon = the macOS nest service shell the `social.fauna.nest`
# LaunchDaemon runs (NOT the standalone `fauna-nest`): it inherits the privileged
# :443 via launchd socket activation and runs the nest IN-PROCESS through the
# shared cross-OS serve loop, building its own private NestConfig + seeding the
# claim code from FAUNA_DATA_DIR (no config.toml / --config). The Windows analogue
# is `fauna-nest-service` (SCM shell). fauna-bridge-supervisor = the macOS MDA
# supervisor (pure Rust) the `social.fauna.bridge` LaunchDaemon runs; it spawns the
# Go MDA child. (The legacy fauna-bridge-daemon/fauna-bridge-imap pair was removed
# in the I6 cutover — so it is no longer built or staged.)
# fauna-sync-agent = the per-user sync agent the social.fauna.sync-agent LaunchAgent
# runs (A4 cutover, sync-agent.md § Packaging + lifecycle) — the sync component's
# one binary (the legacy daemon was removed, sync-agent.md § Headless
# deployment). fauna-tui = the
# terminal app, the .pkg's fifth component (social.fauna.tui → /usr/local/bin):
# the same binary the per-OS release archive carries, here Developer ID-signed
# with the rest (installers/tui.md § The ratified channel, user ruling 2026-09-26).
cargo build --profile "$PROFILE" --target aarch64-apple-darwin \
    -p fauna-nest-daemon -p fauna-sync-agent -p fauna-bridge-supervisor \
    -p fauna-tui

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
RUST_BIN="$TARGET_DIR/aarch64-apple-darwin/$PROFILE"

# The Go MDA (fauna-mail-bridge) is a cgo binary that dynamically links
# libfauna_ffi.dylib. `just mail-bridge-build "$PROFILE"` builds both for the
# host target: the binary lands at bins/fauna-bridges/fauna-mail-bridge and
# the dylib in the FLAVOR-PRIVATE slot that recipe owns (justfile's
# `mail-ffi-slot`, profile-keyed). Both are staged into the bridge payload
# (Step 3), with the dylib link paths rewritten for relocation (Step 3a).
#
# The dylib MUST come from the private slot, not the shared
# `$TARGET_DIR/release/libfauna_ffi.dylib` this read until 2026-08-29: `just
# mac-app release` below runs `apple-ffi-host release`, which builds fauna-ffi
# implicit-host with the app's DEFAULT features and so overwrites the shared slot
# — after this line's build and before Step 3 stages it. The .pkg therefore
# shipped the app-flavored cdylib (payments, zaps and the whole store-safe app
# surface) as the mail-bridge's FFI, in place of the server-side
# `--no-default-features --features labeler` build it links against. Silent: the
# app flavor is a strict superset, so it loads and runs
# .
echo "==> Building Go mail-bridge MDA (cgo + libfauna_ffi.dylib, profile=$PROFILE)..."
( cd "$REPO_ROOT" && just mail-bridge-build "$PROFILE" )

MDA_BIN="$REPO_ROOT/bins/fauna-bridges/fauna-mail-bridge"
MAIL_FFI_SLOT="$( cd "$REPO_ROOT" && just mail-ffi-slot "$PROFILE" )"
FFI_DYLIB="$TARGET_DIR/$MAIL_FFI_SLOT/libfauna_ffi.dylib"

# The desktop app component. `just mac-app release "$PROFILE"` assembles the
# build/Release/Fauna.app bundle from the swift-built FaunaMacOS executable
# (Contents/{MacOS/Fauna, Info.plist, PlugIns/*.appex, ...}) — SwiftPM
# emits a bare Mach-O, so this recipe is what produces a real .app (justfile § mac-app).
# The same signed bundle feeds both this all-in-one .pkg's app component AND the
# app-only `.dmg` (just mac-dmg). The app installs to /Applications, not /usr/local
# (Step 5a), so it stages into its own payload root. `mac-app`'s `config` stays
# `release` regardless of `$PROFILE` — `dist` is a shipping variant of a release
# BUILD, never of Xcode's debug CONFIGURATION (installers/macos.md § Size &
# build profile).
echo "==> Building Fauna.app (config=release, profile=$PROFILE)..."
( cd "$REPO_ROOT" && just mac-app release "$PROFILE" )
APP_BUNDLE="$REPO_ROOT/build/Release/Fauna.app"
[ -d "$APP_BUNDLE" ] || { echo "Fauna.app not built at $APP_BUNDLE" >&2; exit 1; }

# --- Step 2: Sign nest + sync binaries (if --sign) ---
# The bridge artifacts (supervisor, fauna-mail-bridge, libfauna_ffi.dylib) are
# signed AFTER staging in Step 3b — `install_name_tool` rewrites the MDA's dylib
# link paths there, which invalidates any prior signature, so they sign last.
if [ "$SIGN" = true ]; then
    echo "==> Signing nest-daemon + sync + tui binaries..."
    for bin in "$RUST_BIN/fauna-nest-daemon" "$RUST_BIN/fauna-tui"; do
        codesign --sign "$FAUNA_SIGN_IDENTITY" \
            --options runtime --timestamp "$bin"
    done
    # fauna-sync-agent signs with ITS OWN entitlements file — which names no app
    # group on purpose: the agent's state lives in the user domain and it never
    # opens the TCC-protected container (installers/macos.md § Identifier
    # domain, item 6). The same file signs the bundled copy inside Fauna.app
    # (sign-app-bundle.sh) so both exec shapes carry one entitlement set.
    codesign --sign "$FAUNA_SIGN_IDENTITY" \
        --options runtime --timestamp \
        --entitlements "$INSTALLER_DIR/fauna-sync-agent.entitlements" \
        "$RUST_BIN/fauna-sync-agent"
    # The app bundle: `mac-app` ad-hoc-signs it; for a notarizable .pkg the inner
    # .app must be Developer-ID-signed with the hardened runtime + entitlements,
    # replacing the ad-hoc signature — INSIDE-OUT, never `--deep` on the app
    # (that re-stamped the bundled agent with the app's entitlements; measured
    # 2026-08-25). Same script `mac-dmg` uses.
    echo "==> Signing Fauna.app (Developer ID, hardened runtime, inside-out)..."
    "$INSTALLER_DIR/sign-app-bundle.sh" "$APP_BUNDLE" "$FAUNA_SIGN_IDENTITY"
fi

# --- Step 3: Stage payloads ---
# Each payload dir contains only the binaries for that component.
# The uninstall script goes into every component's payload. The node component
# ships fauna-nest-daemon (the LaunchDaemon binary) — not the standalone
# fauna-nest; the daemon builds its config from FAUNA_DATA_DIR, so no default.toml
# is staged.
cp "$RUST_BIN/fauna-nest-daemon" "$BUILD_DIR/payloads/node/bin/"
cp "$INSTALLER_DIR/fauna-uninstall" "$BUILD_DIR/payloads/node/bin/"

cp "$RUST_BIN/fauna-sync-agent" "$BUILD_DIR/payloads/sync/"
cp "$INSTALLER_DIR/fauna-uninstall" "$BUILD_DIR/payloads/sync/"

# Terminal app component: fauna-tui → /usr/local/bin, beside the sync
# component's fauna-sync-agent (the app resolves the agent as its sibling).
cp "$RUST_BIN/fauna-tui" "$BUILD_DIR/payloads/tui/"
cp "$INSTALLER_DIR/fauna-uninstall" "$BUILD_DIR/payloads/tui/"

# Bridge component: the macOS MDA supervisor + the Go fauna-mail-bridge MDA + its
# libfauna_ffi.dylib. All three install to /usr/local/bin/ (the bridge component's
# install-location): the supervisor spawns fauna-mail-bridge from beside itself,
# and the MDA resolves the dylib from beside itself via an @loader_path rpath
# (set in Step 3a).
cp "$RUST_BIN/fauna-bridge-supervisor" "$BUILD_DIR/payloads/bridge/"
cp "$MDA_BIN" "$BUILD_DIR/payloads/bridge/"
cp "$FFI_DYLIB" "$BUILD_DIR/payloads/bridge/"
cp "$INSTALLER_DIR/fauna-uninstall" "$BUILD_DIR/payloads/bridge/"

# App component: the whole Fauna.app bundle. It installs to /Applications (Step 5a),
# so its payload root contains just the bundle (no /usr/local/bin binaries, no
# uninstall script — the app is removed by `fauna-uninstall` shipped in the service
# components, or by dragging it to the Trash for an app-only install). -R preserves
# the bundle's symlinks + the signature.
cp -R "$APP_BUNDLE" "$BUILD_DIR/payloads/app/"

# --- Step 3a: Relocate the MDA's libfauna_ffi.dylib linkage ---
# `just mail-bridge-build` links fauna-mail-bridge against the dylib by its
# ABSOLUTE build path (otool confirms both the binary's load command AND the
# dylib's own install-name are absolute target/release/deps paths), plus an rpath
# into the build target dir — none of which exist on a user's box. Rewrite both to
# @rpath / @loader_path so the installed binary finds the dylib staged beside it.
# This is the macOS twin of the Windows installer shipping fauna_ffi.dll beside
# fauna-mail-bridge.exe (Windows DLLs resolve from the same dir automatically;
# macOS dylibs need the rpath rewrite). The supervisor is pure Rust (no FFI), so
# only the Go MDA needs this.
STAGED_MDA="$BUILD_DIR/payloads/bridge/fauna-mail-bridge"
STAGED_DYLIB="$BUILD_DIR/payloads/bridge/libfauna_ffi.dylib"

# 1. the dylib's own install-name -> @rpath-relative
install_name_tool -id @rpath/libfauna_ffi.dylib "$STAGED_DYLIB"
# 2. rewrite the binary's absolute dylib reference -> @rpath-relative
OLD_DYLIB_REF=$(otool -L "$STAGED_MDA" \
    | awk '/libfauna_ffi\.dylib/ {print $1; exit}')
install_name_tool -change "$OLD_DYLIB_REF" \
    @rpath/libfauna_ffi.dylib "$STAGED_MDA"
# 3. resolve @rpath from beside the binary (both install to /usr/local/bin)
install_name_tool -add_rpath @loader_path "$STAGED_MDA"
# 4. drop the stale build-dir rpath (harmless on a user box, but leaks the dev path)
while IFS= read -r rpath; do
    case "$rpath" in
        */target/release | */target/*/release)
            install_name_tool -delete_rpath "$rpath" "$STAGED_MDA" \
                2>/dev/null || true
            ;;
    esac
done < <(otool -l "$STAGED_MDA" \
    | awk '/cmd LC_RPATH/{f=1} f&&/ path /{print $2; f=0}')

echo "==> MDA dylib linkage after relocation:"
otool -L "$STAGED_MDA" | grep -i fauna_ffi || true

# --- Step 3b: Sign bridge artifacts (if --sign) — AFTER install_name_tool ---
# install_name_tool invalidates code signatures, so the bridge Mach-O artifacts
# (dylib first, then the binaries) are signed here rather than at Step 2.
if [ "$SIGN" = true ]; then
    echo "==> Signing bridge artifacts (post-relocation)..."
    for bin in "$STAGED_DYLIB" \
               "$BUILD_DIR/payloads/bridge/fauna-bridge-supervisor" \
               "$STAGED_MDA"; do
        codesign --sign "$FAUNA_SIGN_IDENTITY" \
            --options runtime --timestamp "$bin"
    done
fi

# --- Step 4: Stage scripts ---
# Each component's scripts dir gets common.sh + its postinstall.
for component in nest sync bridge; do
    cp "$INSTALLER_DIR/scripts/common.sh" "$BUILD_DIR/scripts/$component/"
    cp "$INSTALLER_DIR/scripts/$component/postinstall" \
        "$BUILD_DIR/scripts/$component/"
    chmod +x "$BUILD_DIR/scripts/$component/postinstall"
done

# --- Step 5: Build component packages ---
echo "==> Building component packages..."

# Step 5a: app component — installs Fauna.app to /Applications (machine-wide, like
# the Windows DesktopApp feature). A component plist pins BundleIsRelocatable=false
# so the bundle always lands in /Applications rather than relocating onto a prior
# copy found elsewhere on disk. Generate the plist with `pkgbuild --analyze`, then
# flip BundleIsRelocatable false on EVERY entry.
#
# Every entry, not `0.` — the app now embeds `Contents/PlugIns/*.appex`
# (installers/macos.md § App extensions), so the payload holds nested bundles and
# `--analyze` is no longer guaranteed to emit the single dict this step used to
# hardcode an index into. Relocation is wrong for all of them regardless: nothing
# in this payload may land anywhere but the app's own place under /Applications.
APP_COMPONENT_PLIST="$BUILD_DIR/app-component.plist"
pkgbuild --analyze --root "$BUILD_DIR/payloads/app" "$APP_COMPONENT_PLIST"
APP_COMPONENT_COUNT="$(/usr/libexec/PlistBuddy -c 'Print' "$APP_COMPONENT_PLIST" \
    | /usr/bin/grep -c '^    Dict {')"
[ "$APP_COMPONENT_COUNT" -ge 1 ] || {
    echo "pkgbuild --analyze produced no component entries to pin" >&2
    /usr/bin/plutil -p "$APP_COMPONENT_PLIST" >&2
    exit 1
}
i=0
while [ "$i" -lt "$APP_COMPONENT_COUNT" ]; do
    plutil -replace "$i.BundleIsRelocatable" -bool false "$APP_COMPONENT_PLIST"
    i=$((i + 1))
done

pkgbuild --identifier social.fauna.app \
    --version "$VERSION" \
    --root "$BUILD_DIR/payloads/app" \
    --component-plist "$APP_COMPONENT_PLIST" \
    --install-location /Applications \
    "$BUILD_DIR/components/fauna-app.pkg"

pkgbuild --identifier social.fauna.nest \
    --version "$VERSION" \
    --scripts "$BUILD_DIR/scripts/nest" \
    --root "$BUILD_DIR/payloads/node" \
    --install-location /usr/local \
    "$BUILD_DIR/components/fauna-nest.pkg"

pkgbuild --identifier social.fauna.sync \
    --version "$VERSION" \
    --scripts "$BUILD_DIR/scripts/sync" \
    --root "$BUILD_DIR/payloads/sync" \
    --install-location /usr/local/bin \
    "$BUILD_DIR/components/fauna-sync.pkg"

pkgbuild --identifier social.fauna.bridge \
    --version "$VERSION" \
    --scripts "$BUILD_DIR/scripts/bridge" \
    --root "$BUILD_DIR/payloads/bridge" \
    --install-location /usr/local/bin \
    "$BUILD_DIR/components/fauna-bridge.pkg"

# The terminal app: no scripts (nothing to launch or register), just the binary
# on the same path the other CLI binaries use.
pkgbuild --identifier social.fauna.tui \
    --version "$VERSION" \
    --root "$BUILD_DIR/payloads/tui" \
    --install-location /usr/local/bin \
    "$BUILD_DIR/components/fauna-tui.pkg"

# --- Step 6: Compute install sizes and generate distribution.xml ---
size_app=$(du -sk "$BUILD_DIR/payloads/app" | awk '{print $1}')
size_node=$(du -sk "$BUILD_DIR/payloads/node" | awk '{print $1}')
size_sync=$(du -sk "$BUILD_DIR/payloads/sync" | awk '{print $1}')
size_bridge=$(du -sk "$BUILD_DIR/payloads/bridge" | awk '{print $1}')
size_tui=$(du -sk "$BUILD_DIR/payloads/tui" | awk '{print $1}')

sed -e "s/__VERSION__/$VERSION/g" \
    -e "s/__SIZE_APP__/$size_app/g" \
    -e "s/__SIZE_NODE__/$size_node/g" \
    -e "s/__SIZE_SYNC__/$size_sync/g" \
    -e "s/__SIZE_BRIDGE__/$size_bridge/g" \
    -e "s/__SIZE_TUI__/$size_tui/g" \
    "$INSTALLER_DIR/distribution.xml" > "$BUILD_DIR/distribution.xml"

cp "$INSTALLER_DIR/resources/welcome.html" "$BUILD_DIR/resources/"

# --- Step 7: Build distribution metapackage ---
echo "==> Building Fauna.pkg..."
SIGN_CMD=()
if [ "$SIGN" = true ]; then
    SIGN_CMD=(--sign "$FAUNA_INSTALLER_IDENTITY")
fi

productbuild \
    --distribution "$BUILD_DIR/distribution.xml" \
    --resources "$BUILD_DIR/resources" \
    --package-path "$BUILD_DIR/components" \
    ${SIGN_CMD[@]+"${SIGN_CMD[@]}"} \
    "$REPO_ROOT/build/Fauna-${VERSION}.pkg"

# --- Step 8: Notarize and staple (--sign only; --sign-only stops at signing) ---
if [ "$NOTARIZE" = true ]; then
    PKG="$REPO_ROOT/build/Fauna-${VERSION}.pkg"
    echo "==> Notarizing..."
    xcrun notarytool submit "$PKG" \
        --apple-id "$FAUNA_APPLE_ID" \
        --team-id "$FAUNA_TEAM_ID" \
        --password "$FAUNA_NOTARY_PASSWORD" \
        --wait

    echo "==> Stapling..."
    xcrun stapler staple "$PKG"
fi

# --- Step 9: Generate checksum ---
PKG="$REPO_ROOT/build/Fauna-${VERSION}.pkg"
shasum -a 256 "$PKG" > "${PKG}.sha256"

echo ""
echo "==> Done! Package: $PKG"
echo "    Checksum:  ${PKG}.sha256"
