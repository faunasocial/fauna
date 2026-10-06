#!/bin/bash
set -e

# Fauna Linux installer
# Usage:
#   ./install.sh                   — install to ~/.local (user)
#   sudo ./install.sh              — install to /usr/local (system)
#   PREFIX=/opt/fauna ./install.sh — install to custom prefix
#   ./install.sh --uninstall       — remove installed files

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    BINARY="$CARGO_TARGET_DIR/release/fauna-desktop"
    AGENT_BINARY="$CARGO_TARGET_DIR/release/fauna-sync-agent"
else
    BINARY="target/release/fauna-desktop"
    AGENT_BINARY="target/release/fauna-sync-agent"
fi

# Determine prefix: sudo → /usr/local, else honour $PREFIX or default to ~/.local
if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" -eq 0 ]; then
        PREFIX="/usr/local"
    else
        PREFIX="$HOME/.local"
    fi
fi

INSTALL_BIN="$PREFIX/bin/fauna-desktop"
INSTALL_AGENT="$PREFIX/bin/fauna-sync-agent"
# The desktop entry's basename IS the app id (`APP_ID` in src/main.rs, the
# Flatpak app-id, and the metainfo <id> — all one string since 2026-08-22;
# installers/linux-desktop.md § Desktop entry). The shell associates a window
# with its icon by matching the GApplication id against this basename, so the
# two may never drift apart.
APP_ID="social.fauna.fauna"
INSTALL_DESKTOP="$PREFIX/share/applications/$APP_ID.desktop"
INSTALL_ICON="$PREFIX/share/icons/hicolor/scalable/apps/fauna.svg"
# The client-written systemd user unit (the app installs it at first
# post-auth, not this script). User-mode uninstall runs AS that user, so it is
# the one channel that can remove the unit; root-scope uninstalls leave other
# users' units to go condition-inert instead (linux-desktop.md § Uninstall).
AGENT_UNIT="fauna-sync-agent.service"
AGENT_UNIT_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$AGENT_UNIT"

uninstall() {
    echo "Uninstalling Fauna from $PREFIX..."
    local removed=0

    if [ -f "$INSTALL_BIN" ]; then
        rm -f "$INSTALL_BIN"
        echo "  Removed $INSTALL_BIN"
        removed=$((removed + 1))
    fi

    if [ -f "$INSTALL_AGENT" ]; then
        rm -f "$INSTALL_AGENT"
        echo "  Removed $INSTALL_AGENT"
        removed=$((removed + 1))
    fi

    if [ -f "$AGENT_UNIT_FILE" ]; then
        if command -v systemctl &>/dev/null; then
            systemctl --user disable --now "$AGENT_UNIT" 2>/dev/null || true
        fi
        rm -f "$AGENT_UNIT_FILE"
        if command -v systemctl &>/dev/null; then
            systemctl --user daemon-reload 2>/dev/null || true
        fi
        echo "  Removed $AGENT_UNIT_FILE"
        removed=$((removed + 1))
    fi

    if [ -f "$INSTALL_DESKTOP" ]; then
        rm -f "$INSTALL_DESKTOP"
        echo "  Removed $INSTALL_DESKTOP"
        removed=$((removed + 1))
    fi

    if [ -f "$INSTALL_ICON" ]; then
        rm -f "$INSTALL_ICON"
        echo "  Removed $INSTALL_ICON"
        removed=$((removed + 1))
    fi

    if [ "$removed" -eq 0 ]; then
        echo "Nothing to remove — Fauna does not appear to be installed at $PREFIX."
        exit 0
    fi

    # Refresh desktop/icon caches if available
    if command -v update-desktop-database &>/dev/null; then
        update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache &>/dev/null; then
        gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    fi

    echo "Done. Fauna uninstalled from $PREFIX."
}

install_fauna() {
    if [ ! -f "$BINARY" ] || [ ! -f "$AGENT_BINARY" ]; then
        echo "Error: release binaries not found at $BINARY / $AGENT_BINARY"
        echo "Run: just linux-release"
        exit 1
    fi

    echo "Installing Fauna to $PREFIX..."

    # Binaries (the sync agent ships beside the app on every channel —
    # linux-desktop.md § Installation Files)
    install -Dm755 "$BINARY" "$INSTALL_BIN"
    echo "  Installed $INSTALL_BIN"
    install -Dm755 "$AGENT_BINARY" "$INSTALL_AGENT"
    echo "  Installed $INSTALL_AGENT"

    # Desktop file
    install -Dm644 "$SCRIPT_DIR/packaging/$APP_ID.desktop" "$INSTALL_DESKTOP"
    echo "  Installed $INSTALL_DESKTOP"

    # SVG icon
    if [ -f "$SCRIPT_DIR/fauna.svg" ]; then
        install -Dm644 "$SCRIPT_DIR/fauna.svg" "$INSTALL_ICON"
        echo "  Installed $INSTALL_ICON"
    else
        echo "  Warning: fauna.svg not found — skipping icon install"
    fi

    # Refresh desktop/icon caches if available
    if command -v update-desktop-database &>/dev/null; then
        update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache &>/dev/null; then
        gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
    fi

    echo "Done. Fauna installed to $PREFIX."
    if [ "$PREFIX" = "$HOME/.local" ]; then
        echo "Make sure $PREFIX/bin is in your PATH."
    fi

    # An on-demand folder is a FUSE mount the sync agent makes through the
    # fuse3 package's `fusermount3` (the deb declares it; this channel can only
    # check). Fauna works without it — every synced folder is then kept in
    # full, and the app shows the on-demand switch disabled with this same
    # reason — so its absence is said here, never fatal.
    if ! command -v fusermount3 &>/dev/null; then
        echo "Note: fusermount3 was not found. On-demand folders need the fuse3 package"
        echo "      (for example: sudo apt install fuse3). Until it is installed, every"
        echo "      synced folder is kept in full on this device."
    fi
}

# Parse arguments
case "${1:-}" in
    --uninstall|-u)
        uninstall
        ;;
    --help|-h)
        echo "Usage: $0 [--uninstall]"
        echo ""
        echo "  (no args)     Install to PREFIX (default: ~/.local, or /usr/local if root)"
        echo "  --uninstall   Remove installed files"
        echo ""
        echo "Override install prefix with: PREFIX=/path $0"
        ;;
    "")
        install_fauna
        ;;
    *)
        echo "Unknown option: $1"
        echo "Run '$0 --help' for usage."
        exit 1
        ;;
esac
