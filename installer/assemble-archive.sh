#!/bin/bash
set -euo pipefail

# Assemble a client app's release archive — the ONE definition of what each
# archive channel ships, for the release legs and the install-then-drive
# witnesses alike, so the two can never disagree about the layout: the archive
# name, the files and their modes are decided here and nowhere else. One
# directory, one archive per target, with Fauna's two licence texts and the
# archive's own THIRD-PARTY-NOTICES at its root (shipped-notices.md § Per
# channel). Two payloads:
#
#   tui            the terminal app's channel (docs/goal/architecture/installers/
#                  tui.md § The ratified channel): fauna-tui + fauna-sync-agent +
#                  fauna-cli + apps/fauna-tui/packaging/install.sh, in
#                  fauna-tui-<suffix>. fauna-cli is the cli app's device shell
#                  (docs/goal/architecture/apps/cli.md § The device shell), the
#                  archive's third member (decision 4 of installers/tui.md
#                  § Decisions). Linux, macOS and (with --exe) Windows.
#   linux-desktop  the Linux desktop app's channel 1 (docs/goal/architecture/
#                  installers/linux-desktop.md § What the release publishes):
#                  fauna-desktop + fauna-sync-agent + the ONE
#                  apps/fauna-linux/install.sh + the desktop entry
#                  social.fauna.fauna.desktop + the icon fauna.svg, in
#                  fauna-desktop-<suffix>. Linux only; --exe is refused.
#
# Usage:
#   assemble-archive.sh --payload <tui|linux-desktop> --bin-dir <dir> --out <dir>
#                       --suffix <target-suffix> --notices <file> [--exe] [--no-tar]
#
#   --payload  which archive (above).
#   --bin-dir  where the built binaries are (a cargo target's release/ dir, or —
#              for an install-then-drive witness — the DEBUG binaries; this
#              script reads nothing there but the payload's binaries).
#   --out      where the staged directory and the archive land.
#   --suffix   the target suffix the release names artifacts by
#              (x86_64-linux, aarch64-darwin, x64-windows, …).
#   --notices  the archive's THIRD-PARTY-NOTICES, written by the notices
#              generator before this script runs (`just shipped-notices
#              terminal-archive-<os> <file>`, `just shipped-notices linux-desktop
#              <file>`). Copied, never read: it is dependency-authored text.
#              Staged at the archive root beside LICENSE-MIT and LICENSE-APACHE,
#              which are the repository's own files byte for byte. An archive
#              without them is refused.
#   --exe      the binaries carry a .exe extension (Windows; tui only). The
#              staged directory then holds no install.sh — a user unpacks the zip
#              into a directory on PATH — the three licence files carry `.txt` so
#              that they open there, and the archive is left to the caller
#              (Compress-Archive on the Windows runner; --no-tar is implied).
#   --no-tar   stage the directory only, produce no tar.gz (a witness installs
#              from the staged directory, the way a user installs from the
#              unpacked archive).

PAYLOAD="" BIN_DIR="" OUT="" SUFFIX="" EXT="" TAR=true NOTICES=""
while [ $# -gt 0 ]; do
    case "$1" in
        --payload) PAYLOAD="$2"; shift 2 ;;
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --suffix) SUFFIX="$2"; shift 2 ;;
        --notices) NOTICES="$2"; shift 2 ;;
        --exe) EXT=".exe"; TAR=false; shift ;;
        --no-tar) TAR=false; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$PAYLOAD" ] && [ -n "$BIN_DIR" ] && [ -n "$OUT" ] && [ -n "$SUFFIX" ] && [ -n "$NOTICES" ] || {
    echo "usage: $0 --payload <tui|linux-desktop> --bin-dir <dir> --out <dir> --suffix <target-suffix> --notices <file> [--exe] [--no-tar]" >&2
    exit 2
}

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The payload table: the archive's name, its binaries, its installer, and the
# further repository files staged beside them (EXTRA_SRC[i] → EXTRA_NAME[i]).
EXTRA_SRC=() EXTRA_NAME=()
case "$PAYLOAD" in
    tui)
        NAME="fauna-tui-$SUFFIX"
        BINS=(fauna-tui fauna-sync-agent fauna-cli)
        INSTALLER="$REPO_ROOT/apps/fauna-tui/packaging/install.sh"
        BUILD_HINT="build fauna-tui, fauna-sync-agent AND fauna-cli first (just tui-release)"
        ;;
    linux-desktop)
        [ -z "$EXT" ] || { echo "Error: the linux-desktop archive has no Windows form (--exe)" >&2; exit 2; }
        NAME="fauna-desktop-$SUFFIX"
        BINS=(fauna-desktop fauna-sync-agent)
        INSTALLER="$REPO_ROOT/apps/fauna-linux/install.sh"
        BUILD_HINT="build fauna-desktop AND fauna-sync-agent first (just linux-release)"
        # The desktop entry's basename IS the app id (install.sh's APP_ID).
        EXTRA_SRC=("$REPO_ROOT/apps/fauna-linux/packaging/social.fauna.fauna.desktop" "$REPO_ROOT/apps/fauna-linux/fauna.svg")
        EXTRA_NAME=(social.fauna.fauna.desktop fauna.svg)
        ;;
    *) echo "Error: unknown payload '$PAYLOAD' (tui or linux-desktop)" >&2; exit 2 ;;
esac
STAGE="$OUT/$NAME"

for bin in "${BINS[@]}"; do
    [ -f "$BIN_DIR/$bin$EXT" ] || {
        echo "Error: $BIN_DIR/$bin$EXT not found — $BUILD_HINT." >&2
        exit 1
    }
done

[ -s "$NOTICES" ] || {
    echo "Error: $NOTICES not found or empty — write it first: just shipped-notices <artifact> $NOTICES" >&2
    exit 1
}
for f in "$REPO_ROOT/LICENSE-MIT" "$REPO_ROOT/LICENSE-APACHE" "$INSTALLER" ${EXTRA_SRC[@]+"${EXTRA_SRC[@]}"}; do
    [ -f "$f" ] || { echo "Error: $f not found" >&2; exit 1; }
done

rm -rf "$STAGE"
mkdir -p "$STAGE"
for bin in "${BINS[@]}"; do
    install -m755 "$BIN_DIR/$bin$EXT" "$STAGE/$bin$EXT"
done
if [ -z "$EXT" ]; then
    install -m755 "$INSTALLER" "$STAGE/install.sh"
fi
for i in ${EXTRA_SRC[@]+"${!EXTRA_SRC[@]}"}; do
    install -m644 "${EXTRA_SRC[$i]}" "$STAGE/${EXTRA_NAME[$i]}"
done
# The licence files: `.txt` on Windows so that they open, bare elsewhere.
DOC_EXT=""
[ -n "$EXT" ] && DOC_EXT=".txt"
install -m644 "$REPO_ROOT/LICENSE-MIT" "$STAGE/LICENSE-MIT$DOC_EXT"
install -m644 "$REPO_ROOT/LICENSE-APACHE" "$STAGE/LICENSE-APACHE$DOC_EXT"
install -m644 "$NOTICES" "$STAGE/THIRD-PARTY-NOTICES$DOC_EXT"
echo "staged $STAGE"

if [ "$TAR" = true ]; then
    ARCHIVE="$OUT/$NAME.tar.gz"
    rm -f "$ARCHIVE"
    tar -C "$OUT" -czf "$ARCHIVE" "$NAME"
    echo "archive $ARCHIVE"
fi
