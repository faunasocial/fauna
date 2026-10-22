#!/bin/bash
set -e

# Fauna terminal app — installer for the release archive.
#
# This script ships INSIDE the fauna-tui archive, beside the three binaries it
# installs (docs/goal/architecture/installers/tui.md § The ratified channel).
# Unpack the archive anywhere and run it from there.
#
# Usage:
#   ./install.sh                   — install to ~/.local (user)
#   sudo ./install.sh              — install to /usr/local (system)
#   PREFIX=/opt/fauna ./install.sh — install to custom prefix
#   ./install.sh --uninstall       — remove installed files
#
# Three binaries, never one: fauna-tui's Folders and Devices pages provision the
# per-user sync agent it expects BESIDE itself (agent_spawner::agent_binary_absolute
# resolves the sibling first, then PATH), so the archive carries fauna-sync-agent
# and this script installs both to the same bin dir; and fauna-cli — the cli
# app's device shell, Fauna operated by an AI assistant over MCP
# (docs/goal/architecture/apps/cli.md § The device shell) — rides the same
# archive as its third member (installers/tui.md § Decisions, decision 4), so it
# is installed and removed beside them. That is also why the script installs
# from its OWN directory and branches on nothing else: the install-then-drive
# witness (tests/e2e-unified/tests/artifact/test_tui_installed_product.py)
# stages debug-configuration binaries into a copy of the archive layout and runs
# this exact script over it, so any step that read a build configuration would
# make that substitution dishonest.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BINARY="$SCRIPT_DIR/fauna-tui"
AGENT_BINARY="$SCRIPT_DIR/fauna-sync-agent"
CLI_BINARY="$SCRIPT_DIR/fauna-cli"

# Determine prefix: sudo → /usr/local, else honour $PREFIX or default to ~/.local
if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" -eq 0 ]; then
        PREFIX="/usr/local"
    else
        PREFIX="$HOME/.local"
    fi
fi

INSTALL_BIN="$PREFIX/bin/fauna-tui"
INSTALL_AGENT="$PREFIX/bin/fauna-sync-agent"
INSTALL_CLI="$PREFIX/bin/fauna-cli"
# The Linux desktop app's install.sh puts ITS fauna-desktop + fauna-sync-agent
# in the same prefix. The agent is one binary shared by both apps (one product
# version, one agent), so installing over it is right — and uninstalling the
# terminal app must leave it in place while the desktop app is still there.
DESKTOP_BIN="$PREFIX/bin/fauna-desktop"
# The systemd user unit the app writes for the agent at first post-auth (the
# shared agent_spawner; linux-desktop.md § Uninstall). A user-mode uninstall
# runs AS that user, so it is the one channel that can remove the unit.
AGENT_UNIT="fauna-sync-agent.service"
AGENT_UNIT_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$AGENT_UNIT"

uninstall() {
    echo "Uninstalling the Fauna terminal app from $PREFIX..."
    local removed=0

    if [ -f "$INSTALL_BIN" ]; then
        rm -f "$INSTALL_BIN"
        echo "  Removed $INSTALL_BIN"
        removed=$((removed + 1))
    fi

    # The cli app is the archive's own (no desktop package installs it into a
    # user prefix), so it goes whenever the terminal app goes.
    if [ -f "$INSTALL_CLI" ]; then
        rm -f "$INSTALL_CLI"
        echo "  Removed $INSTALL_CLI"
        removed=$((removed + 1))
    fi

    if [ -f "$DESKTOP_BIN" ]; then
        echo "  Kept $INSTALL_AGENT — the Fauna desktop app at $DESKTOP_BIN still uses it"
    else
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
    fi

    if [ "$removed" -eq 0 ]; then
        echo "Nothing to remove — the Fauna terminal app does not appear to be installed at $PREFIX."
        exit 0
    fi

    echo "Done. Fauna terminal app uninstalled from $PREFIX."
}

# `install -D` is GNU-only: macOS's BSD `install` (this script serves both OSes)
# has no such flag and dies writing into a bin dir that does not exist yet, so
# the directory is made first and `install` only ever copies a file.
install_binary() {
    mkdir -p "$(dirname "$2")"
    install -m755 "$1" "$2"
}

install_fauna() {
    if [ ! -f "$BINARY" ] || [ ! -f "$AGENT_BINARY" ] || [ ! -f "$CLI_BINARY" ]; then
        echo "Error: fauna-tui, fauna-sync-agent and fauna-cli must sit beside this script (unpack the whole archive)."
        echo "Looked for: $BINARY, $AGENT_BINARY and $CLI_BINARY"
        exit 1
    fi

    echo "Installing the Fauna terminal app to $PREFIX..."

    install_binary "$BINARY" "$INSTALL_BIN"
    echo "  Installed $INSTALL_BIN"
    install_binary "$AGENT_BINARY" "$INSTALL_AGENT"
    echo "  Installed $INSTALL_AGENT"
    install_binary "$CLI_BINARY" "$INSTALL_CLI"
    echo "  Installed $INSTALL_CLI (Fauna for AI assistants: fauna-cli mcp)"

    echo "Done. Run: fauna-tui"
    if [ "$PREFIX" = "$HOME/.local" ]; then
        echo "Make sure $PREFIX/bin is in your PATH."
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
