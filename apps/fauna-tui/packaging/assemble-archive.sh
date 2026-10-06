#!/bin/bash
set -euo pipefail

# Assemble the fauna-tui release archive — the ONE definition of what the
# terminal app's channel ships (docs/goal/architecture/installers/tui.md § The
# ratified channel): fauna-tui + fauna-sync-agent + install.sh, one directory,
# one archive per target.
#
# Usage:
#   assemble-archive.sh --bin-dir <dir> --out <dir> --suffix <target-suffix> [--exe] [--no-tar]
#
#   --bin-dir  where the two built binaries are (a cargo target's release/ dir,
#              or — for the install-then-drive witness — a staging copy of the
#              DEBUG binaries; this script reads nothing but the two files).
#   --out      where the staged directory and the archive land.
#   --suffix   the target suffix the release names artifacts by
#              (x86_64-linux, aarch64-darwin, x64-windows, …).
#   --exe      the binaries carry a .exe extension (Windows). The staged
#              directory then holds no install.sh — a user unpacks the zip into
#              a directory on PATH — and the archive is left to the caller
#              (Compress-Archive on the Windows runner; --no-tar is implied).
#   --no-tar   stage the directory only, produce no tar.gz (the witness installs
#              from the staged directory, the way a user installs from the
#              unpacked archive).
#
# Called by release.yml / release-macos.yml (the channel) and by
# tests/e2e-unified/helpers/tui_installed_product.py (the witness), so the two
# can never disagree about the layout: the archive name, the files and their
# modes are decided here and nowhere else.

BIN_DIR="" OUT="" SUFFIX="" EXT="" TAR=true
while [ $# -gt 0 ]; do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --suffix) SUFFIX="$2"; shift 2 ;;
        --exe) EXT=".exe"; TAR=false; shift ;;
        --no-tar) TAR=false; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "$BIN_DIR" ] && [ -n "$OUT" ] && [ -n "$SUFFIX" ] || {
    echo "usage: $0 --bin-dir <dir> --out <dir> --suffix <target-suffix> [--exe] [--no-tar]" >&2
    exit 2
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="fauna-tui-$SUFFIX"
STAGE="$OUT/$NAME"

for bin in fauna-tui fauna-sync-agent; do
    [ -f "$BIN_DIR/$bin$EXT" ] || {
        echo "Error: $BIN_DIR/$bin$EXT not found — build fauna-tui AND fauna-sync-agent first (just tui-release)." >&2
        exit 1
    }
done

rm -rf "$STAGE"
mkdir -p "$STAGE"
install -m755 "$BIN_DIR/fauna-tui$EXT" "$STAGE/fauna-tui$EXT"
install -m755 "$BIN_DIR/fauna-sync-agent$EXT" "$STAGE/fauna-sync-agent$EXT"
if [ -z "$EXT" ]; then
    install -m755 "$SCRIPT_DIR/install.sh" "$STAGE/install.sh"
fi
echo "staged $STAGE"

if [ "$TAR" = true ]; then
    ARCHIVE="$OUT/$NAME.tar.gz"
    rm -f "$ARCHIVE"
    tar -C "$OUT" -czf "$ARCHIVE" "$NAME"
    echo "archive $ARCHIVE"
fi
