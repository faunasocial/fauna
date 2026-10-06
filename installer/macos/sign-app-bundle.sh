#!/bin/bash
# Sign Fauna.app INSIDE-OUT — the bundled agent, then every app extension, then
# third-party frameworks, each with ITS OWN entitlements, and only then the app;
# never `--deep` on the app.
#
# Usage:
#   ./installer/macos/sign-app-bundle.sh <path/to/Fauna.app> <identity>
#     <identity>  a Developer ID Application cert name (hardened runtime +
#                 timestamp are added), or "-" for an ad-hoc dev signature
#                 (no runtime, no timestamp — the `mac-app` shape).
#
# Why this script exists (measured 2026-08-25, installers/macos.md § Identifier
# domain, record item 6): `codesign --deep --entitlements X App.app` re-signs
# every nested Mach-O with the OUTER bundle's entitlements. The bundled
# `Contents/MacOS/fauna-sync-agent` — the copy the .pkg's LaunchAgent prefers —
# therefore shipped carrying the APP's `com.apple.security.application-groups`
# claims, including the account keychain group only the app may hold, while the
# `/usr/local/bin` copy signed with `fauna-sync-agent.entitlements` carried none.
# Same binary, two entitlement sets, decided by which codesign line ran last.
# Signing inside-out is Apple's own guidance and the only shape under which an
# entitlements file means what it says.
#
# Callers: `just mac-app` (ad-hoc), `installer/macos/build.sh --sign|--sign-only`
# and `just mac-dmg` (Developer ID). `tests/e2e-unified/tests/platform/macos/
# test_installer.py::TestDryRun::test_bundled_agent_carries_its_own_entitlements`
# pins the result on the built artifact.
set -euo pipefail

APP="${1:?usage: sign-app-bundle.sh <Fauna.app> <identity|->}"
IDENTITY="${2:?usage: sign-app-bundle.sh <Fauna.app> <identity|->}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"          # installer/macos
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
AGENT_ENTITLEMENTS="$HERE/fauna-sync-agent.entitlements"
APP_ENTITLEMENTS="$REPO_ROOT/apps/fauna-apple/Fauna-macOS/Fauna-macOS.entitlements"

FLAGS=(--force --sign "$IDENTITY")
if [ "$IDENTITY" != "-" ]; then
    FLAGS+=(--options runtime --timestamp)
fi

# 1. Nested executables first, each with its OWN entitlements. The agent's file
#    names no app group on purpose (it never opens the container); this is the
#    line that keeps that true for the bundled copy.
AGENT="$APP/Contents/MacOS/fauna-sync-agent"
if [ -f "$AGENT" ]; then
    codesign "${FLAGS[@]}" --entitlements "$AGENT_ENTITLEMENTS" "$AGENT"
fi

# 2. App extensions, each with ITS OWN entitlements — the same law as the agent,
#    and the reason it matters more here: an .appex's entitlements are what the
#    OS reads to decide what the SANDBOXED extension may touch. The File Provider
#    appex names one app group (its replica container) and the FileProviderUI
#    appex names none at all, while the app names two; `--deep` on the app would
#    hand both of them the app's account-keychain group, which exists precisely so
#    a sandboxed extension can never read the identity seed
#    (installers/macos.md § Identifier domain).
#
#    The entitlements file is derived from the bundle name — the project keeps
#    each appex's sources, Info.plist and entitlements in one directory named
#    after it (`apps/fauna-apple/<Name>/<Name>.entitlements`), so a new appex is
#    signed correctly by existing.  An appex WITHOUT one is a build-system bug,
#    not something to sign entitlement-less: it would silently ship unsandboxed
#    claims, so fail loudly instead.
for APPEX in "$APP"/Contents/PlugIns/*.appex; do
    [ -d "$APPEX" ] || continue          # the glob itself when PlugIns is empty
    NAME="$(basename "$APPEX" .appex)"
    APPEX_ENTITLEMENTS="$REPO_ROOT/apps/fauna-apple/$NAME/$NAME.entitlements"
    if [ ! -f "$APPEX_ENTITLEMENTS" ]; then
        echo "no entitlements file for $NAME at $APPEX_ENTITLEMENTS" >&2
        echo "  (each .appex must carry its own; see the comment above)" >&2
        exit 1
    fi
    codesign "${FLAGS[@]}" --entitlements "$APPEX_ENTITLEMENTS" "$APPEX"
done

# 3. Third-party frameworks. Nothing inside Sparkle carries a Fauna claim, so
#    `--deep` is correct HERE (it signs the Updater.app / XPC services / Autoupdate
#    helpers Sparkle nests) — the footgun is `--deep` on a bundle whose nested
#    code has entitlements of its own.
SPARKLE="$APP/Contents/Frameworks/Sparkle.framework"
if [ -d "$SPARKLE" ]; then
    codesign "${FLAGS[@]}" --deep "$SPARKLE"
fi

# 4. The app itself — no --deep. Nested code is already sealed with the
#    signature it should carry; the app's signature seals THOSE signatures.
codesign "${FLAGS[@]}" --entitlements "$APP_ENTITLEMENTS" "$APP"
codesign --verify --deep --strict "$APP"
