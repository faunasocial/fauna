#!/bin/bash
# Common functions for Fauna macOS installer postinstall scripts.
# Source this from each component's postinstall script.
#
# Two service shapes (the machine-service re-shape; goal
# `docs/goal/architecture/installers/macos.md` § launchd jobs):
#
#   - SERVER (nest, bridge) → machine `LaunchDaemon`s in /Library/LaunchDaemons/,
#     boot-started, headless, each under a dedicated hidden service user
#     (`_fauna` / `_fauna-bridge` — the macOS twin of Linux-native's
#     `useradd -r fauna` / `fauna-bridge`), system data-dir
#     /Library/Application Support/Fauna. Uses the `ensure_service_user` /
#     `install_launchdaemon` helpers below.
#   - SYNC → a per-user `LaunchAgent` in ~/Library/LaunchAgents/ running as the
#     console user (sync is *your* files). Uses `resolve_console_user` /
#     `install_launchagent`.

set -e

# ---------------------------------------------------------------------------
# Per-user (sync) — runs as the GUI-logged-in user.
# ---------------------------------------------------------------------------

# Resolve the GUI-logged-in user. Installer postinstall runs as root,
# so $USER and $(id -u) are root — not the actual user.
resolve_console_user() {
    CONSOLE_USER=$(/usr/sbin/scutil <<< "show State:/Users/ConsoleUser" \
        | awk '/Name :/ { print $3 }')
    if [ -z "$CONSOLE_USER" ] || [ "$CONSOLE_USER" = "loginwindow" ]; then
        echo "Error: could not determine console user" >&2
        exit 1
    fi
    CONSOLE_UID=$(id -u "$CONSOLE_USER")
    USER_HOME=$(dscl . -read /Users/"$CONSOLE_USER" NFSHomeDirectory \
        | awk '{print $2}')
    export CONSOLE_USER CONSOLE_UID USER_HOME
}

# Create the per-user Fauna directories (sync config, LaunchAgents) and ensure
# correct ownership. No per-user log dir: the sync agent writes its own
# size-capped log under Application Support/Fauna/sync/logs/. The server data
# dir is a SYSTEM path created by `setup_system_data_dir` instead.
setup_user_directories() {
    mkdir -p "$USER_HOME/Library/Application Support/Fauna"
    mkdir -p "$USER_HOME/Library/LaunchAgents"
    chown "$CONSOLE_USER:staff" "$USER_HOME/Library/Application Support/Fauna"
    chown "$CONSOLE_USER" "$USER_HOME/Library/LaunchAgents"
}

# Install a per-user LaunchAgent plist if it doesn't already exist.
# On upgrade (plist exists), restart the service instead.
# Args: $1 = label (e.g. "social.fauna.sync-agent"), $2 = plist content
install_launchagent() {
    local label="$1"
    local content="$2"
    local plist_path="$USER_HOME/Library/LaunchAgents/${label}.plist"

    if [ ! -f "$plist_path" ]; then
        echo "$content" > "$plist_path"
        chown "$CONSOLE_USER" "$plist_path"
        # Bootstrap may fail for disabled services in installer context;
        # the plist is still installed and the user can enable later.
        launchctl bootstrap "gui/$CONSOLE_UID" "$plist_path" 2>/dev/null || true
    elif [ "$content" != "$(cat "$plist_path")" ]; then
        # Upgrade with a CHANGED plist (an exec-path or key change shipped):
        # rewrite and reload — the old "kickstart only" shape silently kept
        # every upgraded box on the old plist forever.
        launchctl bootout "gui/$CONSOLE_UID/$label" 2>/dev/null || true
        echo "$content" > "$plist_path"
        chown "$CONSOLE_USER" "$plist_path"
        launchctl bootstrap "gui/$CONSOLE_UID" "$plist_path" 2>/dev/null || true
    else
        # Upgrade, plist unchanged: restart the service so it picks up the
        # newly-installed binary.
        launchctl kickstart -k "gui/$CONSOLE_UID/$label" 2>/dev/null || true
    fi
}

# ---------------------------------------------------------------------------
# Machine-service (nest, bridge) — boot-started LaunchDaemons under a dedicated
# hidden service user. The macOS twin of Linux-native's
# `useradd -r -s /sbin/nologin -d /var/lib/fauna fauna` (installers/linux-nest.md).
# ---------------------------------------------------------------------------

# Create a hidden service user + matching group if absent. Hidden (IsHidden),
# no login shell (/usr/bin/false), no real home (/var/empty) — it exists only to
# own the daemon process + its system data dir. Idempotent.
# Args: $1 = service user/group name (e.g. "_fauna"), $2 = RealName.
ensure_service_user() {
    local name="$1" real_name="$2"
    if dscl . -read "/Users/$name" >/dev/null 2>&1; then
        echo "Service user $name already exists"
        return
    fi
    # Find a free UID/GID in the system service range [300, 499]: Apple reserves
    # <300 for its own daemons; >=500 is the human-login range. Use the same value
    # for the user's UID and its dedicated group's GID.
    local sid=300
    local taken
    taken=$( { dscl . -list /Users UniqueID; dscl . -list /Groups PrimaryGroupID; } \
        | awk '{print $2}' )
    while echo "$taken" | grep -qx "$sid"; do
        sid=$((sid + 1))
        if [ "$sid" -ge 500 ]; then
            echo "Error: no free service UID/GID below 500 for $name" >&2
            exit 1
        fi
    done
    echo "Creating hidden service user $name (uid/gid $sid)"
    # Dedicated group first, so the user's PrimaryGroupID resolves immediately.
    dscl . -create "/Groups/$name"
    dscl . -create "/Groups/$name" PrimaryGroupID "$sid"
    dscl . -create "/Groups/$name" RealName "$real_name"
    # The user: hidden, no login, no home.
    dscl . -create "/Users/$name"
    dscl . -create "/Users/$name" RealName "$real_name"
    dscl . -create "/Users/$name" UniqueID "$sid"
    dscl . -create "/Users/$name" PrimaryGroupID "$sid"
    dscl . -create "/Users/$name" UserShell /usr/bin/false
    dscl . -create "/Users/$name" NFSHomeDirectory /var/empty
    dscl . -create "/Users/$name" IsHidden 1
}

# The canonical SYSTEM server data dir (bucket-2 IPC, wired into the daemon plist
# as FAUNA_DATA_DIR). Owned by `_fauna`; the bridge writes only its `bridge/`
# subdir (owned `_fauna-bridge`) and reads the nest's flag files from here.
FAUNA_SYSTEM_DATA_DIR="/Library/Application Support/Fauna"
# No daemon log dir: each daemon writes its own size-capped `fauna_log` file
# under the data dir it owns (the nest `logs/`, the bridge `bridge/logs/`), and
# a daemon plist carries no StandardOutPath/StandardErrorPath — launchd never
# rotates a redirect (docs/goal/architecture/apps/observability.md
# § Persistence & privacy).

# Create the system data dir owned by a service user.
# Args: $1 = owning service user (e.g. "_fauna").
setup_system_data_dir() {
    local owner="$1"
    mkdir -p "$FAUNA_SYSTEM_DATA_DIR"
    chown "$owner:$owner" "$FAUNA_SYSTEM_DATA_DIR"
    # 755 so the `_fauna-bridge` daemon can read the nest's flag files here.
    chmod 755 "$FAUNA_SYSTEM_DATA_DIR"
}

# Install a machine LaunchDaemon plist into /Library/LaunchDaemons/ if it doesn't
# already exist (root:wheel 0644 — launchd refuses a daemon plist a non-root user
# could overwrite). On upgrade with a CHANGED plist, rewrite and reload it; with
# an unchanged one, restart the running job so it picks up the new binary.
# bootstraps into the SYSTEM domain (the machine daemon), never gui/$UID.
# Args: $1 = label (e.g. "social.fauna.nest"), $2 = plist content.
install_launchdaemon() {
    local label="$1" content="$2"
    local plist_path="/Library/LaunchDaemons/${label}.plist"

    if [ ! -f "$plist_path" ]; then
        echo "$content" > "$plist_path"
        chown root:wheel "$plist_path"
        chmod 644 "$plist_path"
        # The plist ships ENABLED (this postinstall only runs for a SELECTED
        # component — the opt-in is the .pkg component selection, default-unselected;
        # decision 2026-06-26, installers/macos.md § launchd jobs → Default state).
        # bootstrap + RunAtLoad therefore load AND start the daemon right after
        # install — the macOS twin of the Windows MSI auto-starting a ticked service.
        launchctl bootstrap system "$plist_path" 2>/dev/null || true
    elif [ "$content" != "$(cat "$plist_path")" ]; then
        # Upgrade with a CHANGED plist (a key change shipped): rewrite and
        # reload — kickstart alone keeps every upgraded box on the old plist
        # forever (the same fix `install_launchagent` carries).
        launchctl bootout "system/$label" 2>/dev/null || true
        echo "$content" > "$plist_path"
        chown root:wheel "$plist_path"
        chmod 644 "$plist_path"
        launchctl bootstrap system "$plist_path" 2>/dev/null || true
    else
        # Upgrade, plist unchanged: restart the job if it is loaded + enabled.
        launchctl kickstart -k "system/$label" 2>/dev/null || true
    fi
}
