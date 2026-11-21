#!/bin/bash
set -euo pipefail

# Stage the Linux desktop app's AppDir — the ONE definition of what the
# AppImage carries (docs/goal/architecture/installers/linux-desktop.md § 5
# AppImage), for `just linux-appimage` and the tier_3 fixture alike, so the
# recipe and the test can never disagree about the layout:
#
#   AppRun                                  apps/fauna-linux/packaging/appimage/AppRun
#   social.fauna.fauna.desktop              the ONE shared desktop entry
#   fauna.svg (+ .DirIcon → fauna.svg)      the icon the entry's Icon=fauna names
#   usr/bin/fauna-desktop                   the app
#   usr/bin/fauna-sync-agent                the agent AppRun's --sync-agent reaches
#   usr/share/doc/fauna-desktop/            LICENSE-MIT, LICENSE-APACHE and the
#                                           app's THIRD-PARTY-NOTICES
#
# No library is staged: the AppImage shares the host floor of § Platform
# Support. Packing is the caller's (`just linux-appimage`); this script stages.
#
# Usage:
#   stage-appdir.sh --bin-dir <dir> --out <appdir> --notices <file>
#
#   --bin-dir  where fauna-desktop and fauna-sync-agent are (a cargo target's
#              release/ dir, or — for a test — any directory holding the two).
#   --out      the AppDir to write; replaced if it exists.
#   --notices  the THIRD-PARTY-NOTICES the notices generator wrote
#              (`just linux-desktop-notices`). Copied, never read.

BIN_DIR="" OUT="" NOTICES=""
while [ $# -gt 0 ]; do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --notices) NOTICES="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$BIN_DIR" ] && [ -n "$OUT" ] && [ -n "$NOTICES" ] || {
    echo "usage: $0 --bin-dir <dir> --out <appdir> --notices <file>" >&2
    exit 2
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="$REPO_ROOT/apps/fauna-linux/packaging"

for bin in fauna-desktop fauna-sync-agent; do
    [ -f "$BIN_DIR/$bin" ] || {
        echo "Error: $BIN_DIR/$bin not found — build fauna-desktop AND fauna-sync-agent first (just linux-release)." >&2
        exit 1
    }
done
[ -s "$NOTICES" ] || {
    echo "Error: $NOTICES not found or empty — write it first: just linux-desktop-notices" >&2
    exit 1
}
for f in "$PKG/appimage/AppRun" "$PKG/social.fauna.fauna.desktop" "$REPO_ROOT/apps/fauna-linux/fauna.svg" \
    "$REPO_ROOT/LICENSE-MIT" "$REPO_ROOT/LICENSE-APACHE"; do
    [ -f "$f" ] || { echo "Error: $f not found" >&2; exit 1; }
done

rm -rf "$OUT"
mkdir -p "$OUT/usr/bin" "$OUT/usr/share/doc/fauna-desktop"
install -m755 "$PKG/appimage/AppRun" "$OUT/AppRun"
# The desktop entry's basename IS the app id; its Icon=fauna names the icon
# file at the AppDir root, and .DirIcon is the AppImage's own thumbnail.
install -m644 "$PKG/social.fauna.fauna.desktop" "$OUT/social.fauna.fauna.desktop"
install -m644 "$REPO_ROOT/apps/fauna-linux/fauna.svg" "$OUT/fauna.svg"
ln -s fauna.svg "$OUT/.DirIcon"
for bin in fauna-desktop fauna-sync-agent; do
    install -m755 "$BIN_DIR/$bin" "$OUT/usr/bin/$bin"
done
DOC="$OUT/usr/share/doc/fauna-desktop"
install -m644 "$REPO_ROOT/LICENSE-MIT" "$DOC/LICENSE-MIT"
install -m644 "$REPO_ROOT/LICENSE-APACHE" "$DOC/LICENSE-APACHE"
install -m644 "$NOTICES" "$DOC/THIRD-PARTY-NOTICES"
echo "staged $OUT"
