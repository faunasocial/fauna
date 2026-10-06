#!/bin/bash
set -e

# fauna-nest Linux installer
# Installs, upgrades, or removes fauna-nest as a systemd system service.
#
# Usage:
#   sudo ./install.sh --local-binary /path/to/fauna-nest
#   sudo ./install.sh --upgrade --local-binary /path/to/fauna-nest
#   sudo ./install.sh --uninstall [--remove-data]

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_CONFIG="${SCRIPT_DIR}/../../config/default.toml"

BINARY_SRC=""
MODE=""
BIND="0.0.0.0:3000"
DB_PATH="/var/lib/fauna/nest.db"
BLOB_DIR=""

DO_UNINSTALL=false
DO_UPGRADE=false
REMOVE_DATA=false
NON_INTERACTIVE=false

FAUNA_USER="fauna"
FAUNA_GROUP="fauna"
BINARY_DEST="/usr/local/bin/fauna-nest"
CONFIG_DIR="/etc/fauna"
CONFIG_FILE="/etc/fauna/nest.toml"
DATA_DIR="/var/lib/fauna"
UNIT_FILE="/etc/systemd/system/fauna-nest.service"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
die() {
    echo "Error: $*" >&2
    exit 1
}

require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        die "This script must be run as root (e.g. sudo $0 $*)."
    fi
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [ $# -gt 0 ]; do
    case "$1" in
        --non-interactive)
            NON_INTERACTIVE=true
            shift
            ;;
        --config-template)
            [ $# -ge 2 ] || die "--config-template requires a path argument"
            DEFAULT_CONFIG="$2"
            shift 2
            ;;
        --local-binary)
            [ $# -ge 2 ] || die "--local-binary requires a path argument"
            BINARY_SRC="$2"
            shift 2
            ;;
        --mode)
            [ $# -ge 2 ] || die "--mode requires an argument"
            MODE="$2"
            shift 2
            ;;
        --bind)
            [ $# -ge 2 ] || die "--bind requires an argument"
            BIND="$2"
            shift 2
            ;;
        --db)
            [ $# -ge 2 ] || die "--db requires an argument"
            DB_PATH="$2"
            shift 2
            ;;
        --blob-dir)
            [ $# -ge 2 ] || die "--blob-dir requires an argument"
            BLOB_DIR="$2"
            shift 2
            ;;
        --remove-data)
            REMOVE_DATA=true
            shift
            ;;
        --uninstall)
            DO_UNINSTALL=true
            shift
            ;;
        --upgrade)
            DO_UPGRADE=true
            shift
            ;;
        --help|-h)
            cat <<'EOF'
Usage: sudo ./install.sh [OPTIONS]

Options:
  --config-template <path> Path to default.toml template (default: auto-detected from repo)
  --local-binary <path>   Path to a pre-built fauna-nest binary (required for install/upgrade)
  --mode <mode>           Node mode: public|private (default: public)
  --bind <addr:port>      Listen address (default: 0.0.0.0:3000 — all interfaces)
  --db <path>             SQLite database path (default: /var/lib/fauna/nest.db)
  --blob-dir <path>       Blob storage directory (default: none)
  --remove-data           Also remove /var/lib/fauna when uninstalling
  --uninstall             Uninstall the service
  --upgrade               Upgrade the binary and restart the service
  --non-interactive       Never prompt; use defaults or provided flags
  --help                  Show this help
EOF
            exit 0
            ;;
        *)
            die "Unknown option: $1  (run '$0 --help' for usage)"
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
do_uninstall() {
    require_root

    echo "Stopping and disabling fauna-nest..."
    systemctl stop fauna-nest 2>/dev/null || true
    systemctl disable fauna-nest 2>/dev/null || true

    echo "Removing systemd unit..."
    rm -f "$UNIT_FILE"
    systemctl daemon-reload

    echo "Removing binary..."
    rm -f "$BINARY_DEST"

    echo "Removing config directory..."
    rm -rf "$CONFIG_DIR"

    if $REMOVE_DATA; then
        echo "Removing data directory $DATA_DIR ..."
        rm -rf "$DATA_DIR"
        echo "  Data removed."

        echo "Removing system user $FAUNA_USER ..."
        if id "$FAUNA_USER" &>/dev/null; then
            userdel "$FAUNA_USER" 2>/dev/null || true
        fi
    else
        echo "  Data preserved at $DATA_DIR (use --remove-data to delete)."
        echo "  System user $FAUNA_USER preserved (use --remove-data to delete)."
    fi

    echo ""
    echo "fauna-nest uninstalled."
}

# ---------------------------------------------------------------------------
# Install config from default.toml template (mirrors macOS install_default_config)
# ---------------------------------------------------------------------------
install_default_config() {
    if [ -f "$CONFIG_FILE" ]; then
        echo "Config already exists at $CONFIG_FILE, preserving"
        return
    fi

    if [ ! -f "$DEFAULT_CONFIG" ]; then
        die "Default config template not found at $DEFAULT_CONFIG. Use --config-template to specify its location."
    fi

    # Copy template
    cp "$DEFAULT_CONFIG" "$CONFIG_FILE"

    # Rewrite Docker-convention /data/ paths to Linux data dir
    sed -i "s|/data/|${DATA_DIR}/|g" "$CONFIG_FILE"

    # Apply listen address
    sed -i "s|listen = \"0.0.0.0:3000\"|listen = \"${BIND}\"|" "$CONFIG_FILE"

    # Apply mode
    if [ -n "$MODE" ]; then
        sed -i "s|mode = \"public\"|mode = \"${MODE}\"|" "$CONFIG_FILE"
    fi

    # Nothing here writes a domain, an ACME contact or mail settings: those
    # are the admin's choices, made in the app and kept in nest state. The
    # nest learns its domain at claim and derives whether to order a
    # certificate (docs/goal/architecture/installers/linux-nest.md).

    if [ -n "$BLOB_DIR" ]; then
        # Override the blob_dir from the template
        sed -i "s|blob_dir = \"${DATA_DIR}/blobs\"|blob_dir = \"${BLOB_DIR}\"|" "$CONFIG_FILE"
    fi
}

# ---------------------------------------------------------------------------
# Generate claim code (mirrors macOS installer postinstall)
# ---------------------------------------------------------------------------
generate_claim_code() {
    local claim_path="$DATA_DIR/claim-code"
    if [ -f "$claim_path" ]; then
        echo "Claim code already exists at $claim_path, preserving"
        return
    fi
    # 40-bit claim code in the shared display format (mirrors
    # fauna_core::claim_code::generate): 8 chars x 5 bits from the
    # ambiguity-free 32-symbol alphabet (A-Z without I/O, plus 2-9), grouped in
    # one hyphenated 4-char pair. `tr -dc` keeps only alphabet bytes from
    # /dev/urandom (each of the 32 chars equally likely), `fold` chunks,
    # `paste` hyphen-joins.
    local code
    code=$(LC_ALL=C tr -dc 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789' < /dev/urandom \
        | head -c 8 | fold -w4 | paste -sd- -)
    echo "$code" > "$claim_path"
    chown "$FAUNA_USER":"$FAUNA_GROUP" "$claim_path"
    chmod 0640 "$claim_path"
    echo ""
    echo "=========================================="
    echo "  CLAIM CODE: ${code}"
    echo ""
    echo "  Enter this code in your Fauna app"
    echo "  to become admin. Single-use."
    echo "=========================================="
    echo ""
}

# ---------------------------------------------------------------------------
# Write systemd unit
# ---------------------------------------------------------------------------
write_unit() {
    cat > "$UNIT_FILE" <<'EOF'
[Unit]
Description=fauna-nest
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=fauna
Group=fauna
ExecStart=/usr/local/bin/fauna-nest --config /etc/fauna/nest.toml
Restart=on-failure
RestartSec=5
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/fauna
PrivateTmp=true
NoNewPrivileges=true
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
}

# ---------------------------------------------------------------------------
# Upgrade
# ---------------------------------------------------------------------------
do_upgrade() {
    require_root

    if [ ! -f "$CONFIG_FILE" ]; then
        die "No existing install detected ($CONFIG_FILE not found). Run without --upgrade to install."
    fi

    if [ -z "$BINARY_SRC" ]; then
        die "Specify the new binary with --local-binary <path>. (Download not yet implemented.)"
    fi

    [ -f "$BINARY_SRC" ] || die "Binary not found: $BINARY_SRC"

    echo "Upgrading fauna-nest binary..."
    install -Dm755 "$BINARY_SRC" "$BINARY_DEST"
    echo "  Installed $BINARY_DEST"

    echo "Restarting service..."
    systemctl restart fauna-nest
    echo "  fauna-nest restarted."

    echo ""
    echo "Upgrade complete."
}

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
do_install() {
    require_root

    if [ -z "$BINARY_SRC" ]; then
        die "Specify a binary with --local-binary <path>. (Download not yet implemented.)"
    fi
    if [ -z "$MODE" ]; then
        die "--mode is required (public or private)."
    fi
    if [ "$MODE" != "public" ] && [ "$MODE" != "private" ]; then
        die "--mode must be 'public' or 'private', got '$MODE'."
    fi

    [ -f "$BINARY_SRC" ] || die "Binary not found: $BINARY_SRC"

    # 1. Create system user/group
    echo "Creating system user $FAUNA_USER ..."
    if ! id "$FAUNA_USER" &>/dev/null; then
        useradd --system \
            --shell /usr/sbin/nologin \
            --home-dir "$DATA_DIR" \
            --create-home \
            "$FAUNA_USER"
        echo "  User $FAUNA_USER created."
    else
        echo "  User $FAUNA_USER already exists — skipping."
    fi

    # 2. Install binary
    echo "Installing binary to $BINARY_DEST ..."
    install -Dm755 "$BINARY_SRC" "$BINARY_DEST"
    echo "  Installed $BINARY_DEST"

    # 3. Create config directory
    echo "Creating config directory $CONFIG_DIR ..."
    mkdir -p "$CONFIG_DIR"
    chown root:"$FAUNA_GROUP" "$CONFIG_DIR"
    chmod 0750 "$CONFIG_DIR"
    echo "  $CONFIG_DIR (root:$FAUNA_GROUP, 0750)"

    # 4. Install config from default.toml template
    echo "Installing $CONFIG_FILE ..."
    install_default_config
    chown root:"$FAUNA_GROUP" "$CONFIG_FILE"
    chmod 0640 "$CONFIG_FILE"
    echo "  Config written."

    # 5. Create data subdirectories
    echo "Creating data directories under $DATA_DIR ..."
    mkdir -p "$DATA_DIR"
    if [ -n "$BLOB_DIR" ]; then
        mkdir -p "$BLOB_DIR"
        chown "$FAUNA_USER":"$FAUNA_GROUP" "$BLOB_DIR"
    fi
    chown "$FAUNA_USER":"$FAUNA_GROUP" "$DATA_DIR"
    chmod 0750 "$DATA_DIR"
    echo "  $DATA_DIR ($FAUNA_USER:$FAUNA_GROUP, 0750)"

    # 6. Generate claim code for first-time setup
    generate_claim_code

    # 7. Write systemd unit
    echo "Writing systemd unit $UNIT_FILE ..."
    write_unit
    echo "  Unit written."

    # 8. Enable service
    echo "Enabling fauna-nest service..."
    systemctl daemon-reload
    systemctl enable fauna-nest
    echo "  Service enabled."

    # 9. Summary
    echo ""
    echo "Installation complete."
    echo ""
    echo "  Binary:    $BINARY_DEST"
    echo "  Config:    $CONFIG_FILE"
    echo "  Data:      $DATA_DIR"
    echo "  Unit:      $UNIT_FILE"
    echo ""
    echo "Start the service with:"
    echo "  systemctl start fauna-nest"
    echo ""
    echo "View logs with:"
    echo "  journalctl -u fauna-nest -f"
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------
if $DO_UNINSTALL; then
    do_uninstall
elif $DO_UPGRADE; then
    do_upgrade
else
    do_install
fi
