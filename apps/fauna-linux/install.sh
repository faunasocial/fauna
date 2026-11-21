#!/bin/bash
set -e

# Fauna Linux installer
# Usage:
#   ./install.sh                   — install to ~/.local (user)
#   sudo ./install.sh              — install to /usr/local (system)
#   PREFIX=/opt/fauna ./install.sh — install to custom prefix
#   ./install.sh --uninstall       — remove installed files

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Two layouts, one script (installers/linux-desktop.md § What the release
# publishes): the unpacked release archive `fauna-desktop-<suffix>`, which
# holds every file this script installs BESIDE it (staged by
# installer/assemble-archive.sh --payload linux-desktop), or a built source
# tree, where the binaries come from the cargo target and the rest from the
# repository.
if [ -f "$SCRIPT_DIR/fauna-desktop" ]; then
    BINARY="$SCRIPT_DIR/fauna-desktop"
    AGENT_BINARY="$SCRIPT_DIR/fauna-sync-agent"
    LICENCE_DIR="$SCRIPT_DIR"
    NOTICES="$SCRIPT_DIR/THIRD-PARTY-NOTICES"
    DESKTOP_SRC="$SCRIPT_DIR/social.fauna.fauna.desktop"
    ICON_SRC="$SCRIPT_DIR/fauna.svg"
    NOT_FOUND_HINT="The archive is incomplete — download it again."
else
    REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
    LICENCE_DIR="$REPO_ROOT"
    # The app's notices file, written by `just linux-release` (its dependency
    # `linux-desktop-notices`) at the one path every Linux packaging copies
    # from. SHIPPED_NOTICES names another copy of that same file (the tier_1
    # test's).
    NOTICES="${SHIPPED_NOTICES:-$REPO_ROOT/build/shipped-notices/linux-desktop/THIRD-PARTY-NOTICES}"
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        BINARY="$CARGO_TARGET_DIR/release/fauna-desktop"
        AGENT_BINARY="$CARGO_TARGET_DIR/release/fauna-sync-agent"
    else
        BINARY="target/release/fauna-desktop"
        AGENT_BINARY="target/release/fauna-sync-agent"
    fi
    DESKTOP_SRC="$SCRIPT_DIR/packaging/social.fauna.fauna.desktop"
    ICON_SRC="$SCRIPT_DIR/fauna.svg"
    NOT_FOUND_HINT="Run: just linux-release"
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
# Fauna's two licence texts and the app's THIRD-PARTY-NOTICES, in the prefix's
# documentation directory, named after the Debian package (shipped-notices.md
# § Per channel, the Linux desktop row).
INSTALL_DOC="$PREFIX/share/doc/fauna-desktop"
DOC_FILES="LICENSE-MIT LICENSE-APACHE THIRD-PARTY-NOTICES"
# The client-written systemd user unit (the app installs it at first
# post-auth, not this script). User-mode uninstall runs AS that user, so it is
# the one channel that can remove the unit; root-scope uninstalls leave other
# users' units to go condition-inert instead (linux-desktop.md § Uninstall).
AGENT_UNIT="fauna-sync-agent.service"
AGENT_UNIT_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$AGENT_UNIT"
# The GNOME Files Share module the app writes at sign-in (apps/linux.md § The
# file manager's Share entry (GNOME Files)) — per-user like the unit, so only a
# user-mode uninstall reaches it; elsewhere the module finds no agent and
# offers nothing.
SHARE_MODULE_FILE="${XDG_DATA_HOME:-$HOME/.local/share}/nautilus-python/extensions/fauna_share.py"

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

    if [ -f "$SHARE_MODULE_FILE" ]; then
        rm -f "$SHARE_MODULE_FILE"
        echo "  Removed $SHARE_MODULE_FILE"
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

    for doc in $DOC_FILES; do
        if [ -f "$INSTALL_DOC/$doc" ]; then
            rm -f "$INSTALL_DOC/$doc"
            echo "  Removed $INSTALL_DOC/$doc"
            removed=$((removed + 1))
        fi
    done
    rmdir "$INSTALL_DOC" 2>/dev/null || true

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

# The installed menu entry starts THIS prefix's binary by its absolute path
# (installers/linux-desktop.md § 1. Shell Script): a per-user install into
# ~/.local must start from the menu whether or not ~/.local/bin was on the
# graphical session's PATH at login. The shared source entry keeps the bare
# name for the deb and the Flatpak, which install onto every PATH; only its
# `Exec=fauna-desktop` line is rewritten here, for every prefix alike. The path
# is double-quoted and `%` doubled (Desktop Entry Specification § The Exec
# key); a prefix holding a character that would need backslash escaping is
# refused before anything is installed, never rewritten.
check_menu_entry_prefix() {
    case "$INSTALL_BIN" in
        *[\"\`\$\\]*|*$'\n'*)
            echo "Error: the prefix $PREFIX cannot be named in a menu entry"
            echo "(it holds a quote, a backtick, a dollar sign, a backslash or a newline)."
            echo "Choose another prefix: PREFIX=/path $0"
            exit 1
            ;;
    esac
}

menu_entry_for_prefix() {
    local exec_path="${INSTALL_BIN//%/%%}" line rewritten=0
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            "Exec=fauna-desktop"|"Exec=fauna-desktop "*)
                printf 'Exec="%s"%s\n' "$exec_path" "${line#Exec=fauna-desktop}"
                rewritten=$((rewritten + 1))
                ;;
            *)
                printf '%s\n' "$line"
                ;;
        esac
    done < "$DESKTOP_SRC"
    if [ "$rewritten" -ne 1 ]; then
        echo "Error: $DESKTOP_SRC has no single 'Exec=fauna-desktop' line to point at $INSTALL_BIN" >&2
        return 1
    fi
}

install_fauna() {
    if [ ! -f "$BINARY" ] || [ ! -f "$AGENT_BINARY" ]; then
        echo "Error: the binaries were not found at $BINARY / $AGENT_BINARY"
        echo "$NOT_FOUND_HINT"
        exit 1
    fi
    if [ ! -s "$NOTICES" ]; then
        echo "Error: the app's notices file was not found at $NOTICES"
        echo "$NOT_FOUND_HINT"
        exit 1
    fi

    check_menu_entry_prefix
    # Global, not local: the EXIT trap runs after install_fauna returns.
    MENU_ENTRY_TMP="$(mktemp)"
    trap 'rm -f "$MENU_ENTRY_TMP"' EXIT
    menu_entry_for_prefix > "$MENU_ENTRY_TMP"

    echo "Installing Fauna to $PREFIX..."

    # Binaries (the sync agent ships beside the app on every channel —
    # linux-desktop.md § Installation Files)
    install -Dm755 "$BINARY" "$INSTALL_BIN"
    echo "  Installed $INSTALL_BIN"
    install -Dm755 "$AGENT_BINARY" "$INSTALL_AGENT"
    echo "  Installed $INSTALL_AGENT"

    # The licence texts and the notices file (copied, never read: the notices
    # file is dependency-authored text)
    install -Dm644 "$LICENCE_DIR/LICENSE-MIT" "$INSTALL_DOC/LICENSE-MIT"
    install -Dm644 "$LICENCE_DIR/LICENSE-APACHE" "$INSTALL_DOC/LICENSE-APACHE"
    install -Dm644 "$NOTICES" "$INSTALL_DOC/THIRD-PARTY-NOTICES"
    echo "  Installed $INSTALL_DOC/ ($DOC_FILES)"

    # Desktop file, its Exec naming this prefix's binary
    install -Dm644 "$MENU_ENTRY_TMP" "$INSTALL_DESKTOP"
    echo "  Installed $INSTALL_DESKTOP"

    # SVG icon
    if [ -f "$ICON_SRC" ]; then
        install -Dm644 "$ICON_SRC" "$INSTALL_ICON"
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
    # The menu entry needs no PATH (above); only starting Fauna by name from a
    # terminal does.
    if [ "$PREFIX" = "$HOME/.local" ]; then
        echo "To start Fauna from a terminal by name, make sure $PREFIX/bin is in your PATH."
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
