use serde::{Deserialize, Serialize};

/// Moved to [`crate::update_channel`], its one home; re-exported here for the
/// callers that name it by its provisioning path.
pub use crate::update_channel::UpdateChannel;

/// Parameters for generating a cloud-init payload.
///
/// DKIM keys are **not** carried here: the nest mints each mail domain's DKIM
/// signing key itself, when the domain is added and at boot, and holds it sealed
/// under its own key-encryption key
/// (`docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning). The
/// DKIM TXT record is published post-boot from the nest's DKIM selector list
/// (`fauna.bridges.list_dkim_selectors` / `fauna.setup.status` `dkim_records`,
/// the `admin-dns` page). A keypair generated client-side and embedded at
/// `/data/dkim.pem` is one the nest never signs with (`dkim=fail` from day
/// one).
#[derive(Debug, Clone)]
pub struct CloudInitParams {
    pub domain: String,
    pub image_tag: String,
    pub claim_code: String,
    /// Whether this box provisions the **mail subsystem**. Decided at
    /// onboarding (`vps_config`), seeded from the handle's real-domain default
    /// and overridable by the `vps-config-mail-mode-toggle` (mail vs. social).
    ///
    /// - `true` → the compose carries the mail ports `{25,465,587,993}`, the
    ///   content-scan env `FAUNA_CLAMD_ADDR`/`FAUNA_RSPAMD_URL`, the `clamd` +
    ///   `rspamd` sidecars, and the matching `ufw` rules.
    /// - `false` → a **lean social-only box** (nest + updater only): no mail
    ///   ports and **no ~1.5 GB `clamd`**, so the 1 GB VPS tier stays viable
    ///   (`installers/vps.md` § Minimum VPS Requirements). A social-only box
    ///   cannot later enable mail without a resize — a documented, accepted
    ///   constraint.
    ///
    /// This is deployment **topology**, distinct from the runtime
    /// `/data/imap-enabled` mail-enable gate the bridge run-scripts wait on —
    /// so a mail box still only *serves* mail once the admin enables it
    /// post-claim; this field only decides whether the box is *provisioned*
    /// for mail at all.
    pub enable_mail: bool,
    /// The deployment Ed25519 signing seed (64-char hex) to **install at
    /// provision** so the box re-presents this exact `nest_actor_id`, instead
    /// of minting its own on first boot. This is the
    /// *client-provisioned-cloud* box-recovery origin
    /// (`docs/goal/architecture/nest/box-recovery.md` § Mechanism — Capture):
    /// the admin's client generates the seed (the seed's origin), injects it
    /// here, and custodies it off-box in `fauna.state.deployment-seeds` so a rebuilt box can
    /// re-present the same identity after total box loss. The *same* field
    /// carries the **saved** seed at recovery re-provision — the unifying
    /// "provision a box with a caller-supplied deployment seed" primitive.
    ///
    /// Rendered into the compose `environment:` block as
    /// `FAUNA_DEPLOYMENT_SEED`, matching the nest's read path
    /// (`deployment_key::deployment_seed_from_env`). It is bucket-2 IPC the
    /// provisioning artifact sets — never a human-edited knob; `docker inspect`
    /// exposure is root-only on the VPS, and the rendered compose is mode 0600
    /// (so the seed is not world-readable on disk) — the same trust as the
    /// on-disk seed (§ Trust & audience). Unlike the `claim_code` (a *mounted file*), the
    /// seed is env-injected because the nest reads it from env.
    ///
    /// `None` → the env line is omitted and the box mints its own random seed
    /// (today's behavior, and the *claim-an-existing-box* origin, where the
    /// seed is captured at **claim** rather than injected at provision).
    pub deployment_seed: Option<String>,
    /// The admin's **SSH door** choice (`installers/vps.md` § The SSH door,
    /// default off). `None` → the keyless, sshd-less box of
    /// `deployment-threat-model.md` § Concern 1a, rendered byte-for-byte as
    /// before the door existed. `Some` → a key-only sshd for `root` with the
    /// door's public key installed; 22/tcp stays denied until the root
    /// coordinator admits the one address the admin claims from.
    pub ssh_door: Option<SshDoor>,
}

/// The SSH door's box-side input: the public half of the Ed25519 keypair the
/// admin's app mints for this box alone (`installers/vps.md` § The SSH door).
/// The private half never leaves the admin's device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshDoor {
    /// One OpenSSH `authorized_keys` line (`ssh-ed25519 AAAA… [comment]`),
    /// written as `root`'s only authorized key.
    pub authorized_key: String,
}

/// Like `Serialize`/`Deserialize` derives — kept manual to keep
/// `CloudInitParams` deliberately non-serializable: it carries the
/// `claim_code` (the box-claim secret) and must never accidentally land in a
/// JSON log or be reconstructed from untrusted input.
impl Serialize for CloudInitParams {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom(
            "CloudInitParams carries the claim-code secret — serialization is intentionally disabled",
        ))
    }
}

impl<'de> Deserialize<'de> for CloudInitParams {
    fn deserialize<D: serde::Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom(
            "CloudInitParams must be constructed in-process",
        ))
    }
}

// ── Host OS Maintenance (installers/vps.md § Host OS Maintenance) ───────────
//
// An onboarded box keeps **Ubuntu itself** patched (the nest *image* is
// the Fauna updater's job) with no human ever logging in, so the whole policy is
// artifact-set IPC + constants — never a human-edited knob. These static files
// are baked into cloud-init below: `unattended-upgrades` for the live-apply
// majority, and a `fauna-reboot-coordinator` systemd timer that reboots the box
// for a pending kernel/glibc/systemd update only when the nest is idle (or a
// 24 h ceiling elapses), reusing the nest's existing graceful-shutdown path.
//
// NOTE: each const is a literal VALUE injected as a `format!` argument (like the
// mail fragments), never part of the format *string* — so the `${distro_id}`,
// `$nrconf{...}`, `$(...)`, and `$(( ... ))` braces/sigils need no escaping.

/// `/etc/apt/apt.conf.d/20auto-upgrades` — enable the periodic package-list
/// update + unattended-upgrade timers (off by default on a minimal install).
const APT_20AUTO_UPGRADES: &str = r#"APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";"#;

/// `/etc/apt/apt.conf.d/50unattended-upgrades` — apply security + `-updates`
/// origins automatically, but **never auto-reboot**: the nest-coordinated idle
/// reboot below owns reboots, not the apt timer (which is timezone-naive and
/// blind to live sessions).
const APT_50UNATTENDED_UPGRADES: &str = r#"Unattended-Upgrade::Allowed-Origins {
        "${distro_id}:${distro_codename}-security";
        "${distro_id}ESMApps:${distro_codename}-apps-security";
        "${distro_id}ESM:${distro_codename}-infra-security";
        "${distro_id}:${distro_codename}-updates";
};
// The fauna-reboot-coordinator owns reboots; the apt timer must never reboot.
Unattended-Upgrade::Automatic-Reboot "false";"#;

/// `/etc/needrestart/conf.d/fauna.conf` — list-only so a library update never
/// auto-bounces `docker.service` (which would drop the nest container
/// uncoordinated); anything needing a restart waits for the next idle reboot.
const NEEDRESTART_CONF: &str = r#"# Library updates must never auto-bounce docker.service (that would drop the
# nest container uncoordinated). List-only: defer every service restart to the
# next nest-coordinated reboot.
# Authority: docs/goal/architecture/installers/vps.md § Host OS Maintenance.
$nrconf{restart} = 'l';"#;

/// `/usr/local/sbin/fauna-reboot-coordinator` — the host-side reboot policy.
/// The pure `fauna_reboot_decision` function is argument-only so the cloud-init
/// tests can exercise it via `sh` without touching the filesystem.
const REBOOT_COORDINATOR_SCRIPT: &str = r#"#!/bin/sh
# fauna-reboot-coordinator — host-OS reboot policy for an onboarded fauna VPS.
# Baked into cloud-init by libs/fauna-provisioning; no human ever edits or runs
# it by hand (there is no operator). Driven by fauna-reboot-coordinator.timer
# every ~15 min. Authority: docs/goal/architecture/installers/vps.md
# § Host OS Maintenance.
#
# It (1) publishes host apt state to the nest via host-status, and (2) reboots
# the box for a pending kernel/glibc/systemd update ONLY when the nest is idle
# (no live client connection) or a 24 h hard ceiling has elapsed — so a security
# reboot never interrupts an active user yet can never be deferred forever.
#
# Trust model: this runs as ROOT, and the adversary host-patching defends against
# is a COMPROMISED nest container (host uid 1000). So the channel is split by
# trust direction. The nest->host files (nest-readiness, restart-requested) live
# in the container-owned rw $MAINT_DIR and are read DEFENSIVELY (symlink-rejecting
# + value-validated; root never writes there). This coordinator's own trusted
# state (host-status) lives in the root-owned $HOST_DIR the container cannot write
# to or symlink-plant in (exported :ro to the container so the nest can still read
# it). The ceiling clock is the mtime of the host-owned /run/reboot-required,
# which the container cannot forge.
set -eu

# nest->host (container-owned, rw): read defensively; root never writes here.
MAINT_DIR=/opt/fauna/maintenance
READINESS="$MAINT_DIR/nest-readiness"
RESTART_REQUESTED="$MAINT_DIR/restart-requested"
# host->nest (root-owned, exported :ro to the container): only root writes here.
HOST_DIR=/opt/fauna/maintenance-host
HOST_STATUS="$HOST_DIR/host-status"
# The host's own global-unicast addresses: the IP bridge cert's second source
# (nest/tls-certificates.md § B-IP *When*). The nest cannot see them from inside
# its bridge network.
HOST_ADDRESSES="$HOST_DIR/host-addresses"
CEILING_SECS=86400
# Min host uptime before an admin "restart now" is honoured (INFO-1 anti-loop): a
# compromised container that re-plants restart-requested after each boot would
# otherwise drive a tight reboot loop at the OnBootSec cadence. 600 s > OnBootSec
# (300 s), so a re-plant is refused at the first post-boot run and only honoured a
# cycle later — slowing any loop and widening an admin's window to SSH in, while a
# genuine admin restart-now (issued on a box up > 10 min) stays immediate.
MIN_REBOOT_UPTIME_SECS=600

# Pure reboot decision (argument-only so it can be unit-tested without touching
# the filesystem). Args:
#   reboot_pending(true|false) connection_count reboot_deferred_since now ceiling_secs [restart_now(true|false)]
# Prints "reboot" or "hold". restart_now (the admin "restart now" request,
# default false) overrides idle + ceiling — the admin explicitly chose to reboot.
fauna_reboot_decision() {
    _rp=$1
    _cc=$2
    _rds=$3
    _now=$4
    _ceil=$5
    _restart_now=${6:-false}
    # Admin "restart now" (fauna.admin.request_host_restart) overrides everything.
    [ "$_restart_now" = true ] && { echo reboot; return 0; }
    [ "$_rp" = true ] || { echo hold; return 0; }
    # Idle ⇒ reboot now (a request can only be in flight over a connection, so
    # zero connections means nothing is in flight).
    if [ "$_cc" -eq 0 ] 2>/dev/null; then echo reboot; return 0; fi
    # Otherwise reboot once the pending update has waited past the hard ceiling.
    case "$_rds" in
        ''|*[!0-9]*) : ;;
        *) [ "$(( _now - _rds ))" -gt "$_ceil" ] && { echo reboot; return 0; } ;;
    esac
    echo hold
}

# Read the nest's live connection count from the readiness file at $1, DEFENSIVELY
# and HANG-PROOF. Argument-only (no global state) so the cloud-init tests can plant
# a FIFO/symlink/regular file and assert it never blocks. Echoes the count, or 0.
#
# Defensive: reject a symlink (a compromised nest could point it at a host file)
# and digit-validate. A missing/symlinked/stale file reads as idle (0) — rebooting
# a dead nest is desirable, and forcing "idle" only ACCELERATES a benign, crash-safe
# reboot, never suppresses one (N-3).
#
# Hang-proof (HM-3): the container OWNS this dir, so it can win a TOCTOU race —
# present a regular file at the [ ! -L ] guard, then rename(2) a FIFO over it before
# grep opens it. grep on a writer-less FIFO blocks forever, which would wedge this
# Type=oneshot unit in `activating` so the timer can never re-trigger it -> every
# future security reboot suppressed. `timeout` bounds the open+read so a planted
# FIFO/slow-file can never block the coordinator; a killed read yields 0 (the benign
# idle path). TimeoutStartSec= on the .service is the second half of the fix (any
# residual wedge self-clears); together they close both the per-run window and the
# permanence.
fauna_read_connection_count() {
    _rf=$1
    _cc=0
    if [ -f "$_rf" ] && [ ! -L "$_rf" ]; then
        _line=$(timeout 5 grep '^connection_count=' "$_rf" 2>/dev/null | head -n1 || true)
        _line=${_line#connection_count=}
        case "$_line" in ''|*[!0-9]*) : ;; *) _cc=$_line ;; esac
    fi
    echo "$_cc"
}

# Pure filter: `ip -o addr show` output on stdin -> one `address=<ip>` line per
# global-unicast address the host holds (IPv4 and IPv6). Coarse on purpose:
# loopback, link-local, ULA, RFC 1918 and CGNAT are dropped here, and the nest
# re-classifies every address itself (is_global_ip), so nothing here is a trust
# decision. A field carrying anything but hex digits, `.` and `:` is dropped,
# never echoed into the root-owned file.
fauna_host_address_lines() {
    while read -r _idx _ifname _family _cidr _rest; do
        case "$_family" in inet|inet6) : ;; *) continue ;; esac
        case " $_rest " in *" scope global "*) : ;; *) continue ;; esac
        _addr=${_cidr%%/*}
        case "$_addr" in ''|*[!0-9A-Fa-f:.]*) continue ;; esac
        case "$_family:$_addr" in
            inet:10.*|inet:127.*|inet:169.254.*|inet:192.168.*) continue ;;
            inet:172.1[6-9].*|inet:172.2[0-9].*|inet:172.3[01].*) continue ;;
            inet:100.6[4-9].*|inet:100.[7-9][0-9].*|inet:100.1[01][0-9].*|inet:100.12[0-7].*) continue ;;
            inet6:[Ff][CcDd]*|inet6:[Ff][Ee][89AaBb]*|inet6:::1) continue ;;
        esac
        printf 'address=%s\n' "$_addr"
    done
}

# Write the filtered listing on stdin to $1 atomically (tmp + rename), like
# host-status. Argument-only so the tests can aim it at a scratch path.
fauna_write_host_addresses() {
    _dest=$1
    fauna_host_address_lines > "$_dest.tmp"
    mv "$_dest.tmp" "$_dest"
}

# Publish the host's addresses into the ROOT-OWNED $HOST_DIR. An unreadable
# interface table leaves the last published set in place rather than blanking
# it.
fauna_publish_host_addresses() {
    _listing=$(ip -o addr show 2>/dev/null) || return 0
    printf '%s\n' "$_listing" | fauna_write_host_addresses "$HOST_ADDRESSES"
}

fauna_coordinator_main() {
    # Both dirs are created by cloud-init (runcmd) before this timer's first run;
    # we never mkdir them here (the root-owned $HOST_DIR must stay root-owned, and
    # under ProtectSystem=strict their parents are read-only anyway).
    now=$(date +%s)

    # Pending security updates: apt-check prints "<all>;<security>" on stderr.
    security_updates_pending=0
    if [ -x /usr/lib/update-notifier/apt-check ]; then
        _counts=$(/usr/lib/update-notifier/apt-check 2>&1 || echo "0;0")
        security_updates_pending=${_counts#*;}
        case "$security_updates_pending" in ''|*[!0-9]*) security_updates_pending=0 ;; esac
    fi

    reboot_pending=false
    [ -f /run/reboot-required ] && reboot_pending=true

    last_patched_at=
    if [ -f /var/lib/apt/periodic/unattended-upgrades-stamp ]; then
        last_patched_at=$(stat -c %Y /var/lib/apt/periodic/unattended-upgrades-stamp 2>/dev/null || echo "")
    fi

    # First moment the reboot became pending (drives the 24 h ceiling). Derived
    # from the mtime of the host-owned /run/reboot-required — an UN-FORGEABLE
    # clock: a compromised nest container cannot touch /run, so it can never push
    # the ceiling into the future to suppress a security reboot (HM-2). Empty when
    # no reboot is pending.
    reboot_deferred_since=
    if [ "$reboot_pending" = true ]; then
        reboot_deferred_since=$(stat -c %Y /run/reboot-required 2>/dev/null || echo "")
        case "$reboot_deferred_since" in ''|*[!0-9]*) reboot_deferred_since=$now ;; esac
    fi

    # Publish host status for the nest into the ROOT-OWNED $HOST_DIR (atomic: write
    # tmp + rename). The container cannot symlink-plant here, so the plain >/mv is
    # safe (closes HM-1 for this write sink).
    _tmp="$HOST_STATUS.tmp"
    {
        printf 'security_updates_pending=%s\n' "$security_updates_pending"
        printf 'reboot_pending=%s\n' "$reboot_pending"
        printf 'reboot_deferred_since=%s\n' "$reboot_deferred_since"
        printf 'last_patched_at=%s\n' "$last_patched_at"
    } > "$_tmp"
    mv "$_tmp" "$HOST_STATUS"

    # Refresh the host's published addresses every run (an address added or
    # moved since first boot reaches the nest within one timer interval). Never
    # allowed to stop the reboot decision below.
    fauna_publish_host_addresses || true

    # Read the nest's live connection count defensively + hang-proof (HM-3) —
    # extracted as a pure, argument-only fn so the cloud-init tests can plant a
    # FIFO/symlink/regular file at the path and assert it never blocks.
    connection_count=$(fauna_read_connection_count "$READINESS")

    # Host uptime in whole seconds from /proc/uptime — host-owned state a
    # compromised container cannot forge (it has no /proc write, and the coordinator
    # runs on the host, not in the container). Drives the INFO-1 anti-loop guard
    # below. An unreadable/odd value defaults to 0 (= "just booted" → restart-now
    # refused — the conservative direction).
    uptime_secs=0
    if [ -r /proc/uptime ]; then
        read -r _up _ < /proc/uptime || _up=0
        uptime_secs=${_up%%.*}
        case "$uptime_secs" in ''|*[!0-9]*) uptime_secs=0 ;; esac
    fi

    # Admin "restart now" request (fauna.admin.request_host_restart): the nest
    # writes this flag, the admin client triggers it. Read DEFENSIVELY from the
    # container-owned dir — reject a symlink (the nest writes a regular file). This
    # is a STAT-ONLY test ([ -f ]/[ ! -L ] never open() the file, and [ -f ] is
    # false for a FIFO), so unlike the readiness grep above it has no FIFO-hang
    # vector; the TimeoutStartSec= backstop covers any residual. Honoured regardless
    # of idle/ceiling (below); consumed (rm) on the reboot path so it fires exactly
    # once. The `rm -f` removes the flag's own dir entry and never follows a
    # final-component symlink, so the consume is O_NOFOLLOW-safe; a stray/forged
    # flag at worst causes one extra benign, crash-safe reboot.
    restart_requested=false
    if [ -f "$RESTART_REQUESTED" ] && [ ! -L "$RESTART_REQUESTED" ]; then
        restart_requested=true
    fi

    # INFO-1 anti-loop: ignore a restart-now within the first MIN_REBOOT_UPTIME_SECS
    # of boot so a compromised container re-planting the flag after each boot cannot
    # drive a tight reboot loop. A genuine pending update still reboots below; a
    # legit admin restart-now is merely deferred to the next run (the flag is left
    # unconsumed while held). The clock is the un-forgeable host uptime.
    if [ "$restart_requested" = true ] && [ "$uptime_secs" -lt "$MIN_REBOOT_UPTIME_SECS" ]; then
        restart_requested=false
    fi

    if [ "$(fauna_reboot_decision "$reboot_pending" "$connection_count" "$reboot_deferred_since" "$now" "$CEILING_SECS" "$restart_requested")" = reboot ]; then
        rm -f "$RESTART_REQUESTED"
        logger -t fauna-reboot-coordinator "rebooting for host update: connection_count=$connection_count reboot_deferred_since=$reboot_deferred_since now=$now restart_requested=$restart_requested"
        systemctl reboot
    fi
}

# Entry point. `publish-addresses` is the first-boot call cloud-init makes
# before `docker compose up`: it publishes host-addresses and exits WITHOUT the
# reboot decision, because a freshly patched box may already carry
# /run/reboot-required and would reboot before the compose is up. A bare run
# (the timer) is the full coordinator.
fauna_coordinator_entry() {
    case "${1:-}" in
        publish-addresses) fauna_publish_host_addresses ;;
        *) fauna_coordinator_main ;;
    esac
}

# When sourced for unit testing (FAUNA_COORDINATOR_TEST=1) expose only the
# helper functions (fauna_reboot_decision, fauna_read_connection_count,
# fauna_host_address_lines, fauna_write_host_addresses, fauna_coordinator_entry)
# — do not gather host state or reboot.
if [ "${FAUNA_COORDINATOR_TEST:-}" != 1 ]; then
    fauna_coordinator_entry "$@"
fi"#;

/// `/etc/systemd/system/fauna-reboot-coordinator.service` — oneshot, driven by
/// the timer.
const REBOOT_COORDINATOR_SERVICE: &str = r#"[Unit]
Description=Fauna host-OS reboot coordinator (oneshot; driven by the timer)

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/fauna-reboot-coordinator
# HM-3 backstop: Type=oneshot DISABLES the start timeout by default, so a wedged
# ExecStart (e.g. a read that blocks on a container-planted FIFO) would hang the
# unit in `activating` forever and the timer could never re-trigger it -> reboots
# suppressed permanently. An explicit TimeoutStartSec= re-enables the timeout so
# any wedge self-clears (the unit fails, the timer re-arms next interval). Paired
# with the `timeout`-bounded readiness read so the per-run window is closed too.
TimeoutStartSec=120
# Defense-in-depth (installers/vps.md § Host OS Maintenance): the coordinator runs
# as root, so mount-namespace-confine its blast radius. Even if a write somehow
# followed a symlink, ProtectSystem=strict makes the whole FS read-only except the
# two maintenance dirs, so it cannot reach /etc, /usr, or /boot. The script only
# ever writes inside those two dirs.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/opt/fauna/maintenance /opt/fauna/maintenance-host"#;

/// `/etc/systemd/system/fauna-reboot-coordinator.timer` — runs the coordinator
/// ~5 min after boot (`OnBootSec`, deliberately < `MIN_REBOOT_UPTIME_SECS` 600 s so
/// the INFO-1 anti-loop guard refuses a re-planted restart-now on the *first*
/// post-boot run) and every ~15 min thereafter (`OnUnitActiveSec`).
const REBOOT_COORDINATOR_TIMER: &str = r#"[Unit]
Description=Run the Fauna host-OS reboot coordinator every 15 min

[Timer]
OnBootSec=5min
OnUnitActiveSec=15min

[Install]
WantedBy=timers.target"#;

// ── The SSH door (installers/vps.md § The SSH door) ─────────────────────────
//
// Rendered only when the admin chose the door at create
// ([`CloudInitParams::ssh_door`]); a door-less box carries none of it.

/// `/etc/ssh/sshd_config.d/0-fauna-door.conf` — key-only sshd. Named to sort
/// before the provider's `50-cloud-init.conf`: sshd keeps the first value it
/// reads per keyword, and the provider's cloud-init writes
/// `PasswordAuthentication yes` for a keyless create.
const SSH_DOOR_DROPIN_PATH: &str = "/etc/ssh/sshd_config.d/0-fauna-door.conf";
const SSH_DOOR_DROPIN: &str = r#"# The SSH door (installers/vps.md § The SSH door): key-only, for root.
# Written by the Fauna installer; sorts before 50-cloud-init.conf so these win.
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
AuthenticationMethods publickey"#;

/// `/usr/local/sbin/fauna-ssh-door` — the root coordinator's door pass: admit
/// the claimant's address once. Its own script and unit, written only on a door
/// box, so a door-less box carries no door code at all and `ufw` can write
/// `/etc/ufw` and `/run`, which the reboot coordinator's `ProtectSystem=strict`
/// keeps read-only. The validation and admission functions are argument-only so
/// the tests run the shipped shell against scratch paths with a stub `ufw`.
const SSH_DOOR_SCRIPT: &str = r#"#!/bin/sh
# fauna-ssh-door — the SSH door's one-shot admission on an onboarded fauna VPS.
# Baked into cloud-init by libs/fauna-provisioning, only on a box whose admin
# chose the door; no human ever edits or runs it by hand. Driven by
# fauna-ssh-door.timer every minute until the admission is recorded.
# Authority: docs/goal/architecture/installers/vps.md § The SSH door.
#
# Trust model: the same as fauna-reboot-coordinator's. This runs as ROOT and
# reads one nest-written file, ssh-door-source, from the container-owned rw
# $MAINT_DIR, DEFENSIVELY; its record, ssh-door-admitted, goes to the root-owned
# $HOST_DIR the container cannot write to. A compromised nest in the claim
# window can at most choose the one admitted address; once the admission is
# recorded nothing the nest writes is read again.
set -eu

# nest->host (container-owned, rw): read defensively; root never writes here.
MAINT_DIR=/opt/fauna/maintenance
SSH_DOOR_SOURCE="$MAINT_DIR/ssh-door-source"
# host->nest (root-owned, exported :ro to the container): only root writes here.
HOST_DIR=/opt/fauna/maintenance-host
SSH_DOOR_ADMITTED="$HOST_DIR/ssh-door-admitted"
SSH_DOOR_DROPIN=/etc/ssh/sshd_config.d/0-fauna-door.conf

# Pure validation: succeed iff $1 is one IPv4 dotted quad — four decimal octets,
# none over 255, no leading zero, and not the unspecified 0.0.0.0.
fauna_ssh_door_ipv4() {
    _v4=$1
    case "$_v4" in ''|*[!0-9.]*|.*|*.|*..*) return 1 ;; esac
    _n=0
    _rest=$_v4
    while :; do
        _o=${_rest%%.*}
        case "$_o" in ''|0[0-9]*|????*) return 1 ;; esac
        [ "$_o" -le 255 ] || return 1
        _n=$((_n + 1))
        [ "$_rest" = "$_o" ] && break
        _rest=${_rest#*.}
    done
    [ "$_n" -eq 4 ] && [ "$_v4" != 0.0.0.0 ]
}

# Pure validation: succeed iff $1 is one IPv6 literal — colon-separated groups of
# at most four hex digits, at most one `::`, an optional dotted-quad tail, and
# not the unspecified `::`. No prefix length, no zone, nothing ufw would read as
# a range.
fauna_ssh_door_ipv6() {
    _v6=$1
    case "$_v6" in ''|*[!0-9A-Fa-f:.]*|*:::*|*::*::*|::) return 1 ;; esac
    case "$_v6" in *:*) : ;; *) return 1 ;; esac
    _n=0
    _rest=$_v6
    while :; do
        _f=${_rest%%:*}
        if [ "$_rest" = "$_f" ]; then
            case "$_f" in
                *.*) fauna_ssh_door_ipv4 "$_f" || return 1; _n=$((_n + 2)) ;;
                ?????*|*[!0-9A-Fa-f]*) return 1 ;;
                *) _n=$((_n + 1)) ;;
            esac
            break
        fi
        case "$_f" in ?????*|*[!0-9A-Fa-f]*) return 1 ;; esac
        _n=$((_n + 1))
        _rest=${_rest#*:}
    done
    case "$_v6" in
        *::*) [ "$_n" -le 9 ] ;;
        *) [ "$_n" -eq 8 ] ;;
    esac
}

# One IPv4 or IPv6 literal, and nothing else.
fauna_ssh_door_valid_address() {
    case "$1" in
        *:*) fauna_ssh_door_ipv6 "$1" ;;
        *) fauna_ssh_door_ipv4 "$1" ;;
    esac
}

# The SSH door's one-shot admission. Argument-only so the tests can aim it at
# scratch paths with a stub `ufw` on PATH:
#   fauna_ssh_door_admit <source> <dropin> <admitted> <now>
# Acts only on a box carrying the door's drop-in, and only while <admitted> does
# not exist — once it does, whatever the nest writes later is ignored. The
# container-owned <source> is read DEFENSIVELY, like nest-readiness: a symlink is
# rejected, the read is timeout-bounded (a planted FIFO cannot wedge the unit),
# and the value must be a single IP literal, handed to ufw as one argument and
# never evaluated. `ufw prepend` puts the allow above the cloud-init deny in the
# rule's own family's chain, IPv4 or IPv6 alike (ufw stops at the first matching
# rule; `insert N` numbers the combined list, where IPv6 rules follow every
# IPv4 one). A malformed
# value is recorded as refused — the door stays shut for good, as the nest
# writes the file only once — while a failing ufw exits non-zero under `set -e`
# and leaves <admitted> unwritten, so the next pass retries.
fauna_ssh_door_admit() {
    _src=$1
    _dropin=$2
    _adm=$3
    _at=$4
    [ -e "$_adm" ] && return 0
    [ -f "$_dropin" ] || return 0
    if [ ! -f "$_src" ] || [ -L "$_src" ]; then
        return 0
    fi
    _line=$(timeout 5 grep '^address=' "$_src" 2>/dev/null | head -n1 || true)
    _ip=${_line#address=}
    if fauna_ssh_door_valid_address "$_ip"; then
        ufw prepend allow proto tcp from "$_ip" to any port 22
        _outcome="address=$_ip"
    else
        logger -t fauna-ssh-door "refused a malformed ssh-door-source; the door stays shut" 2>/dev/null || true
        _outcome="refused=malformed"
    fi
    {
        printf '%s\n' "$_outcome"
        printf 'at=%s\n' "$_at"
    } > "$_adm.tmp"
    mv "$_adm.tmp" "$_adm"
}

# The door pass the timer drives. Once the admission is recorded the timer has
# nothing left to do and switches itself off.
fauna_ssh_door_pass() {
    fauna_ssh_door_admit "$SSH_DOOR_SOURCE" "$SSH_DOOR_DROPIN" "$SSH_DOOR_ADMITTED" "$(date +%s)"
    if [ -e "$SSH_DOOR_ADMITTED" ]; then
        systemctl disable --now fauna-ssh-door.timer 2>/dev/null || true
    fi
}

# When sourced for unit testing (FAUNA_SSH_DOOR_TEST=1) expose only the
# functions — do not read the box's files or touch the firewall.
if [ "${FAUNA_SSH_DOOR_TEST:-}" != 1 ]; then
    fauna_ssh_door_pass
fi"#;

/// `/etc/systemd/system/fauna-ssh-door.service` — runs [`SSH_DOOR_SCRIPT`].
/// `ProtectSystem=true` (not the reboot coordinator's `strict`) because `ufw`
/// writes `/etc/ufw` and `/run`; `/usr` and `/boot` stay read-only, and the
/// script itself writes only `ssh-door-admitted` in the root-owned host dir.
const SSH_DOOR_SERVICE: &str = r#"[Unit]
Description=Fauna SSH door: admit the claimant's address once (oneshot; driven by the timer)

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/fauna-ssh-door
TimeoutStartSec=120
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=true
ProtectHome=read-only"#;

/// `/etc/systemd/system/fauna-ssh-door.timer` — every minute, so the door
/// opens within about a minute of the claim; the pass disables the timer once
/// the admission is recorded.
const SSH_DOOR_TIMER: &str = r#"[Unit]
Description=Run the Fauna SSH door pass every minute until the claimant is admitted

[Timer]
OnBootSec=1min
OnUnitActiveSec=1min

[Install]
WantedBy=timers.target"#;

/// The door's `write_files` entries. `root`'s `authorized_keys` is written
/// `defer: true` — in cloud-init's final stage, after its `ssh` module has set
/// up root's keys — so nothing rewrites it afterwards. Only the key's first
/// line is used, trimmed: the value lands inside a YAML block scalar, and a
/// newline must never reach the document.
fn ssh_door_write_files(door: &SshDoor) -> String {
    let key = door.authorized_key.lines().next().unwrap_or("").trim();
    let mut s = String::new();
    s.push_str(&write_file_entry(
        SSH_DOOR_DROPIN_PATH,
        SSH_DOOR_DROPIN,
        "0644",
    ));
    s.push_str(&format!(
        "  - path: /root/.ssh/authorized_keys\n    content: |\n{body}\n    permissions: '0600'\n    defer: true\n",
        body = indent_block(key, 6),
    ));
    s.push_str(&write_script_entry(
        "/usr/local/sbin/fauna-ssh-door",
        SSH_DOOR_SCRIPT,
    ));
    s.push_str(&write_file_entry(
        "/etc/systemd/system/fauna-ssh-door.service",
        SSH_DOOR_SERVICE,
        "0644",
    ));
    s.push_str(&write_file_entry(
        "/etc/systemd/system/fauna-ssh-door.timer",
        SSH_DOOR_TIMER,
        "0644",
    ));
    s
}

/// The door's `runcmd` lines, after the coordinator timer (so after the
/// `daemon-reload` that sees the door units): `/root/.ssh` to 0700, root's
/// provider-expired password un-expired (`SSH_DOOR_UNEXPIRE_ROOT`), then the
/// door timer.
const SSH_DOOR_RUNCMD: &str = "\n  - chmod 0700 /root/.ssh\n  - chage -d \"$(date +%Y-%m-%d)\" root\n  - systemctl enable --now fauna-ssh-door.timer";

/// The provider creates a keyless server with root's generated password
/// already expired, and PAM's account check then ends every key login that has
/// no TTY ("You are required to change your password immediately"). The door
/// resets the last-change date to the boot's day; the password stays unknown
/// and sshd never accepts it (`PasswordAuthentication no`).
#[cfg(test)]
const SSH_DOOR_UNEXPIRE_ROOT: &str = "  - chage -d \"$(date +%Y-%m-%d)\" root";

/// Seconds between `docker compose up` attempts while the nest image will not
/// pull (installers/vps.md § What Happens Behind the Scenes, step 3). Bucket 1 —
/// no human chooses it; it matches the pre-release channels' registry interval.
pub const NEST_UP_RETRY_INTERVAL_SECS: u64 = 60;

/// `/usr/local/sbin/fauna-nest-up` — bring the nest compose up, retrying until
/// it succeeds. A failed pull (tag not yet published, registry down, transient
/// network) creates no container, so Docker's `restart:` policy never applies
/// and nothing else retries — and sshd is already off, so nobody could by hand.
/// A shell loop rather than `Restart=on-failure`: independent of the systemd
/// version's oneshot-restart rules, and every attempt is logged.
/// `FAUNA_NEST_UP_INTERVAL_S` exists only so the containerised test can shorten
/// the wait; the unit never sets it.
fn nest_up_script() -> String {
    format!(
        r#"#!/bin/sh
# Bring up the Fauna nest compose; retry until the image pulls and it starts.
INTERVAL="${{FAUNA_NEST_UP_INTERVAL_S:-{NEST_UP_RETRY_INTERVAL_SECS}}}"
until docker compose -f /opt/fauna/docker-compose.yml up -d; do
    logger -t fauna-nest-up "compose up failed; retrying in $INTERVAL s"
    sleep "$INTERVAL"
done"#
    )
}

/// `/etc/systemd/system/fauna-nest.service` — runs `fauna-nest-up` on first
/// boot (cloud-init starts it `--no-block`) and on every boot after (an
/// idempotent `up -d`). `Type=exec` + `RemainAfterExit=yes`, NOT `oneshot`: a
/// target gains an implicit `After=` on every unit it `Wants`, so a oneshot still
/// retrying would hold `multi-user.target` — and everything ordered after it —
/// for as long as the registry stays out of reach.
const NEST_UP_SERVICE: &str = r#"[Unit]
Description=Bring up the Fauna nest compose (retries until the image pulls)
After=docker.service network-online.target
Wants=network-online.target
Requires=docker.service

[Service]
Type=exec
RemainAfterExit=yes
ExecStart=/usr/local/sbin/fauna-nest-up

[Install]
WantedBy=multi-user.target"#;

/// The final `runcmd` lines: enable the first-boot unit and start it WITHOUT
/// blocking cloud-init, which must not wait on a pull that may take weeks. The
/// `systemctl daemon-reload` in [`MAINTENANCE_RUNCMD`] precedes them.
const NEST_UP_RUNCMD: &str =
    "  - systemctl enable fauna-nest.service\n  - systemctl start --no-block fauna-nest.service";

/// The host-maintenance `runcmd` lines, injected after `ufw --force enable` and
/// before the `fauna-nest.service` start ([`NEST_UP_RUNCMD`]) so both bind-mount
/// sources exist before the nest comes up. Two dirs, split by trust direction (installers/vps.md § Host OS
/// Maintenance): `/opt/fauna/maintenance` is `chown`ed to the container's `fauna`
/// uid 1000 (the nest writes nest-readiness / restart-requested there);
/// `/opt/fauna/maintenance-host` is left **root-owned `0755`** (only root writes
/// host-status there; exported `:ro` to the container) so a compromised nest can
/// neither symlink-plant nor forge the host status. Once the host dir exists the
/// coordinator's `publish-addresses` entry point writes `host-addresses` (and
/// only that — never the reboot decision), so the nest's first lifecycle tick
/// already sees the host's addresses (`installers/vps.md` § Host OS Maintenance
/// → 3). The coordinator timer is enabled last.
const MAINTENANCE_RUNCMD: &str = "\n  - mkdir -p /opt/fauna/maintenance\n  - chown 1000:1000 /opt/fauna/maintenance\n  - mkdir -p /opt/fauna/maintenance-host\n  - chmod 0755 /opt/fauna/maintenance-host\n  - /usr/local/sbin/fauna-reboot-coordinator publish-addresses || true\n  - systemctl daemon-reload\n  - systemctl enable --now fauna-reboot-coordinator.timer";

/// Prefix every non-empty line of `text` with `spaces` spaces, for embedding a
/// file's content under a YAML `content: |` block scalar. Blank lines stay
/// empty (valid in a block scalar). The block indent is the first non-empty
/// line's indent (here `spaces`), and cloud-init strips it when writing the file
/// — so a script line that is itself indented N spaces lands back at N spaces.
fn indent_block(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one cloud-init `write_files` entry (2-space list indent, content under
/// a 6-space `content: |` block scalar).
fn write_file_entry(path: &str, content: &str, permissions: &str) -> String {
    format!(
        "  - path: {path}\n    content: |\n{body}\n    permissions: '{permissions}'\n",
        body = indent_block(content, 6),
    )
}

/// A box-side `sh` script's `write_files` entry (mode 0755), rendered without
/// its full-line comments: they document the script here, in the source, and
/// on the box would only spend [`USER_DATA_MAX_BYTES`]. The shebang stays. Safe
/// because none of these scripts carries a heredoc or a multi-line quoted
/// string, where a `#` line would be data (pinned by
/// `test_box_scripts_lose_only_their_comment_lines`).
fn write_script_entry(path: &str, script: &str) -> String {
    write_file_entry(path, &script_without_comments(script), "0755")
}

fn script_without_comments(script: &str) -> String {
    script
        .lines()
        .enumerate()
        .filter(|(i, line)| {
            (*i == 0 && line.starts_with("#!")) || !line.trim_start().starts_with('#')
        })
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// All host-maintenance `write_files` entries (apt config, needrestart defer,
/// the reboot-coordinator script + its systemd service/timer), as a block to
/// inject after the claim-code entry.
fn maintenance_write_files() -> String {
    let mut s = String::new();
    s.push_str(&write_file_entry(
        "/etc/apt/apt.conf.d/20auto-upgrades",
        APT_20AUTO_UPGRADES,
        "0644",
    ));
    s.push_str(&write_file_entry(
        "/etc/apt/apt.conf.d/50unattended-upgrades",
        APT_50UNATTENDED_UPGRADES,
        "0644",
    ));
    s.push_str(&write_file_entry(
        "/etc/needrestart/conf.d/fauna.conf",
        NEEDRESTART_CONF,
        "0644",
    ));
    s.push_str(&write_script_entry(
        "/usr/local/sbin/fauna-reboot-coordinator",
        REBOOT_COORDINATOR_SCRIPT,
    ));
    s.push_str(&write_file_entry(
        "/etc/systemd/system/fauna-reboot-coordinator.service",
        REBOOT_COORDINATOR_SERVICE,
        "0644",
    ));
    s.push_str(&write_file_entry(
        "/etc/systemd/system/fauna-reboot-coordinator.timer",
        REBOOT_COORDINATOR_TIMER,
        "0644",
    ));
    s.push_str(&write_script_entry(
        "/usr/local/sbin/fauna-nest-up",
        &nest_up_script(),
    ));
    s.push_str(&write_file_entry(
        "/etc/systemd/system/fauna-nest.service",
        NEST_UP_SERVICE,
        "0644",
    ));
    s
}

/// Generate a cloud-init YAML payload for a fauna-nest VPS.
///
/// The **mail subsystem** is provisioned only when `params.enable_mail` — the
/// mail ports, the content-scan env, the `clamd`/`rspamd` sidecars, their `configs:` block, and the matching `ufw`
/// rules are all gated on it. A social-only box (`enable_mail == false`) gets a
/// lean nest+updater compose with no ~1.5 GB `clamd`, keeping the 1 GB VPS
/// tier viable (`installers/vps.md` § Minimum VPS Requirements). See
/// [`CloudInitParams::enable_mail`].
///
/// ⚠ **The compose this renders is also published, by hand, in
/// `docs/guides/nest-internet-setup.md` § Step 5** — that guide is how a user who
/// declines the app-driven provisioning brings the same box up themselves, so
/// the two MUST stay functionally identical. Change one, change the other in the
/// same commit. Enforced by
/// [`tests::test_internet_nest_guide_matches_rendered_compose`], which
/// compares the guide's YAML block against this output modulo comments and blank
/// lines — a drift fails the merge rather than silently shipping two different
/// deployments.
/// The most `user_data` any VPS adapter may send: Hetzner Cloud refuses a
/// create whose `user_data` is longer than 32,768 bytes (`422 invalid_input`),
/// the tightest limit among the adapters. Every render [`build_cloud_init`]
/// can produce stays at or under it (unit-tested on the largest one).
pub const USER_DATA_MAX_BYTES: usize = 32_768;

pub fn build_cloud_init(params: &CloudInitParams) -> String {
    // Mail-only compose fragments — empty strings on a social-only box, so the
    // rendered YAML omits the ports / scan env / sidecars / ufw rules entirely.
    let mail_ports = if params.enable_mail {
        r#"
            # Mail: 25 inbound MX, 465 implicit-TLS + 587 STARTTLS submission, 993 IMAPS
            - "25:25"
            - "465:465"
            - "587:587"
            - "993:993""#
    } else {
        ""
    };
    let mail_env = if params.enable_mail {
        r#"
            # Co-located content-scan daemons (sidecars below). The mail bridge's
            # scan gate is default-on + FAIL-CLOSED: an unreachable clamd/rspamd
            # 451s every inbound message once mail is enabled
            # (installers/docker.md § content-scan sidecars).
            FAUNA_CLAMD_ADDR: clamd:3310
            FAUNA_RSPAMD_URL: http://rspamd:11333"#
    } else {
        ""
    };
    let mail_services = if params.enable_mail {
        r#"
        # ── Content-scan sidecars ───────────────────────────────────────────
        # The mail bridge dials these from inside the fauna-nest container over
        # the compose network. Both are required whenever mail is enabled: the
        # scan gate defaults on and fail-closes (451) when a scanner is
        # unreachable. clamd holds the signature DB in RAM (~1.5 GB), so the
        # practical RAM floor for a mail-enabled box is well above the 1 GB
        # minimum (installers/docker.md + installers/vps.md).
        clamd:
          image: clamav/clamav:latest-debian@sha256:967334b92d1782e4d1314ddf903ae537d26792d21c9a39adecb8ac9757980514
          logging: *default-logging
          restart: unless-stopped
        rspamd:
          image: rspamd/rspamd:latest@sha256:86bc544548bc881276e19dcff4cf36bc4fb5c8a3050717f3c2315cb823938a80
          # Bind the normal worker (the /checkv2 endpoint the bridge POSTs to) on
          # all interfaces so the nest container can reach it; the stock image
          # binds localhost only.
          configs:
            - source: rspamd-worker-normal
              target: /etc/rspamd/local.d/worker-normal.inc
          logging: *default-logging
          restart: unless-stopped"#
    } else {
        ""
    };
    let mail_configs = if params.enable_mail {
        "      configs:\n        rspamd-worker-normal:\n          content: |\n            bind_socket = \"*:11333\";\n"
    } else {
        ""
    };
    let mail_ufw = if params.enable_mail {
        r#"
  - ufw allow 25/tcp
  - ufw allow 465/tcp
  - ufw allow 587/tcp
  - ufw allow 993/tcp"#
    } else {
        ""
    };
    // Box-recovery (box-recovery.md § Mechanism — Capture): a
    // client-provisioned box boots with the caller-supplied deployment seed so
    // it re-presents the client-custodied `nest_actor_id` instead of minting
    // its own. Injected as compose env to match the nest's read path
    // (`deployment_key::deployment_seed_from_env` reads `FAUNA_DEPLOYMENT_SEED`).
    // `None` → omitted → the box mints its own random seed (today's behavior +
    // the claim-an-existing-box origin). 12-space indent to sit beside
    // FAUNA_MODE inside the `environment:` block.
    let seed_env = match &params.deployment_seed {
        Some(seed) => format!("\n            FAUNA_DEPLOYMENT_SEED: {seed}"),
        None => String::new(),
    };
    // Host-OS maintenance write_files (apt unattended-upgrades, needrestart
    // defer, the reboot-coordinator script + systemd units) — always present;
    // installers/vps.md § Host OS Maintenance.
    let maintenance_files = maintenance_write_files();
    // The SSH door (installers/vps.md § The SSH door): every fragment is empty
    // on a door-less box, so its render is unchanged. With the door, sshd stays
    // enabled (key-only) instead of being switched off; 22/tcp stays denied
    // until the door pass admits the claimant's address.
    let (door_pwauth, door_files, sshd_off, door_runcmd) = match &params.ssh_door {
        Some(door) => (
            "ssh_pwauth: false\n",
            ssh_door_write_files(door),
            "",
            SSH_DOOR_RUNCMD,
        ),
        None => (
            "",
            String::new(),
            "\n  - systemctl disable --now ssh.service ssh.socket",
            "",
        ),
    };
    format!(
        r#"#cloud-config
{door_pwauth}package_update: true
packages:
  - docker.io
  # docker.io is the engine only; the `docker compose` v2 plugin ships separately.
  # List it explicitly so the first-boot unit's `docker compose up` never depends on an implicit
  # Recommends. docker.io stays the base — it's in the Ubuntu archive, so the
  # unattended-upgrades below patch it (docker-ce from Docker's own repo would not
  # be covered by the Ubuntu-origin allowlist).
  - docker-compose-v2
  - ufw
  - unattended-upgrades
  - needrestart

write_files:
  - path: /opt/fauna/docker-compose.yml
    content: |
      # Cap every container's json logs so a chatty or runaway service (clamd's
      # freshclam, a refresh loop) can never fill the host disk and wedge the
      # node. Mirrors docker-compose.yml's x-logging anchor — without it a single
      # json log once grew to 69 GB on a 75 GB box (installers/vps.md § rotation).
      x-logging: &default-logging
        driver: json-file
        options:
          max-size: "50m"
          max-file: "5"
      services:
        fauna-nest:
          image: ghcr.io/faunasocial/nest:{image_tag}
          ports:
            - "80:8080"
            - "443:443"
            # The P2P relay's address discovery (UDP): tells a device its own
            # public address so two devices behind NATs can connect directly.
            # Mirrors docker-compose.yml (behavior/p2p.md § The relay).
            - "7842:7842/udp"{mail_ports}
          volumes:
            - fauna-data:/data
            # Claim-code seed, staged OUTSIDE /data (the entrypoint copies it
            # into the writable volume on first boot). It MUST NOT mount onto
            # /data/claim-code directly: that path is read-only here, but the
            # uid-1000 nest must both *read* the code (at claim) and *delete* it
            # (single-use), and the entrypoint's first-run `chown -R /data` +
            # sensitive-file `chmod 0600` would EROFS-crash the boot on a
            # read-only bind under `set -e`. Mounting the seed off /data keeps
            # the mount-secrecy intent (the code never lands in `environment:`)
            # while leaving the live file writable + deletable.
            - /opt/fauna/claim-code:/run/fauna/claim-code-seed:ro
            # Host-OS maintenance channel (installers/vps.md § Host OS
            # Maintenance), split by trust direction so a compromised nest
            # (uid 1000) cannot forge the host->nest status or the ceiling clock:
            #  - maintenance (rw, uid 1000): the nest writes nest-readiness +
            #    restart-requested; the root coordinator reads them defensively.
            #  - maintenance-host (ro, root-owned): the root coordinator writes
            #    host-status; the nest only reads it (it cannot symlink-plant in a
            #    root-owned dir -> closes the HM-1 root-clobber).
            - /opt/fauna/maintenance:/data/maintenance
            - /opt/fauna/maintenance-host:/data/maintenance-host:ro
          environment:
            FAUNA_MODE: public
            # The update-channel seed (nest/update-channel.md § The channel as
            # nest state): the same tag as `image:`, read only while no admin
            # has chosen a channel from the app.
            FAUNA_IMAGE_TAG: {image_tag}{mail_env}{seed_env}
          labels:
            - "social.fauna.update=true"
          logging: *default-logging
          # 15s > the nest's WS 1001 drain (GRACEFUL_SHUTDOWN_TIMEOUT 7s) + DB
          # flush, so an updater re-create never SIGKILLs the nest mid-shutdown
          # (mirrors docker-compose.yml).
          stop_grace_period: 15s
          restart: unless-stopped
        fauna-updater:
          # The Fauna updater (nest/update-channel.md § The updater): the nest
          # image's second binary at the same tag, holding the Docker socket
          # and nothing else; it follows the channel the nest reports and
          # re-creates the labelled nest on a newer build, never an older one
          # (mirrors docker-compose.yml).
          image: ghcr.io/faunasocial/nest:{image_tag}
          entrypoint: ["/usr/local/bin/fauna-updater"]
          volumes:
            - /var/run/docker.sock:/var/run/docker.sock
          logging: *default-logging
          restart: unless-stopped{mail_services}
{mail_configs}      volumes:
        fauna-data:
    permissions: '0600'
  # No /opt/fauna/.env: the compose above carries literal values (domain, image
  # tag), so an .env would be inert (the compose has no ${{...}} refs).
  # Switch the image tag by editing the compose `image:` line.
  - path: /opt/fauna/claim-code
    content: "{claim_code}"
    permissions: '0600'
{maintenance_write_files}{door_files}
runcmd:
  - systemctl enable docker
  - systemctl start docker{sshd_off}
  - ufw deny 22/tcp
  - ufw allow 80/tcp
  - ufw allow 443/tcp
  - ufw allow 7842/udp{mail_ufw}
  - ufw --force enable{maintenance_runcmd}{door_runcmd}
{nest_up_runcmd}
"#,
        door_pwauth = door_pwauth,
        door_files = door_files,
        sshd_off = sshd_off,
        door_runcmd = door_runcmd,
        image_tag = params.image_tag,
        claim_code = params.claim_code,
        // NB: params.domain is no longer injected — the box boots DOMAINLESS and
        // learns its domain from the admin's claim handle (the primary
        // `mail_domains` row IS the deployment identity;
        // docs/goal/architecture/nest/domains-and-tls-bootstrap.md § Env
        // contract). The field is retained on CloudInitParams for the callers
        // that still pass it, but the rendered compose sets no FAUNA_DOMAIN.
        mail_ports = mail_ports,
        mail_env = mail_env,
        seed_env = seed_env,
        mail_services = mail_services,
        mail_configs = mail_configs,
        mail_ufw = mail_ufw,
        maintenance_write_files = maintenance_files,
        maintenance_runcmd = MAINTENANCE_RUNCMD,
        nest_up_runcmd = NEST_UP_RUNCMD,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The box boots DOMAINLESS: the rendered compose must NOT inject
    /// `FAUNA_DOMAIN`. The nest learns its domain from the admin's claim handle
    /// (the primary `mail_domains` row IS the deployment identity;
    /// `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Env contract).
    #[test]
    fn test_cloud_init_is_domainless() {
        let params = CloudInitParams {
            domain: "test.example.com".into(),
            image_tag: "latest".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        assert!(output.starts_with("#cloud-config"));
        assert!(
            !output.contains("FAUNA_DOMAIN"),
            "cloud-init must boot domainless (no FAUNA_DOMAIN env): {output}"
        );
        assert!(output.contains("ghcr.io/faunasocial/nest:latest"));
        assert!(
            !output.contains("WATCHTOWER"),
            "the updater has no interval to set"
        );
    }

    #[test]
    fn test_cloud_init_has_required_ports() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        for port in [
            "80:8080",
            "443:443",
            "7842:7842/udp",
            "25:25",
            "465:465",
            "587:587",
            "993:993",
        ] {
            assert!(output.contains(port), "missing port mapping {port}");
        }
        for port in [
            "80/tcp", "443/tcp", "7842/udp", "25/tcp", "465/tcp", "587/tcp", "993/tcp",
        ] {
            assert!(
                output.contains(&format!("ufw allow {port}")),
                "missing ufw rule {port}"
            );
        }
        // A deployed box runs no sshd and allows no 22/tcp (deployment-threat-model.md
        // § Concern 1a): both units off (Ubuntu socket-activates sshd), the port denied.
        assert!(
            !output.contains("ufw allow 22/tcp"),
            "22/tcp must never be allowed"
        );
        assert!(output.contains("systemctl disable --now ssh.service ssh.socket"));
        assert!(output.contains("ufw deny 22/tcp"));
    }

    /// Bridge-isolation hardening is an IMAGE property (UID split + Landlock +
    /// the entrypoint's blessed-key mint — security.md § Co-resident process
    /// trust boundary, verified in-image by tier_4 `test_uid_isolation.py` /
    /// `test_bridge_enrollment_pop.py`) — but a compose override could defeat
    /// every slice from outside the image: `privileged:`/`cap_add` breaks the
    /// UID/DAC wall, a `user:` override breaks the s6 root entrypoint (no key
    /// mint, no blessed registry → silently-lenient enrollment), an
    /// `entrypoint:`/`command:` override skips the mint outright, a
    /// `security_opt` seccomp/apparmor line can block the landlock syscalls
    /// (→ the warn-and-continue NotEnforced fallback), and a `FAUNA_BLESSED`/
    /// `FAUNA_ROUTER_PROXY_SECRET`/`FAUNA_BLESSED_KEYS_DIR` env override could
    /// repoint trust anchors. The installer must render NONE of these — this is
    /// the installer half of the deployment-carries-the-hardening verification
    /// (the live half is `_assert_enrollment_strict` in
    /// `tests/live/test_private_relay_hetzner.py`; no SSH exists on a
    /// provisioned box to check with, `testing.md` § Gap 3).
    #[test]
    fn test_cloud_init_does_not_defeat_image_hardening() {
        for enable_mail in [false, true] {
            let params = CloudInitParams {
                domain: "x.com".into(),
                image_tag: "latest".into(),
                claim_code: "AABBCC".into(),
                enable_mail,
                deployment_seed: None,
                ssh_door: None,
            };
            let rendered = build_cloud_init(&params);
            // The one sanctioned override is the updater's own entrypoint — a
            // second container of the image that runs no nest and mints nothing
            // (`nest/update-channel.md` § The updater); it appears exactly once.
            let updater_entrypoint = r#"entrypoint: ["/usr/local/bin/fauna-updater"]"#;
            assert_eq!(rendered.matches(updater_entrypoint).count(), 1);
            let output = rendered.replace(updater_entrypoint, "");
            for banned in [
                "privileged",
                "cap_add",
                "security_opt",
                "entrypoint:",
                "command:",
                "user:",
                "userns_mode",
                "FAUNA_BLESSED",
                "FAUNA_ROUTER_PROXY_SECRET",
            ] {
                assert!(
                    !output.contains(banned),
                    "cloud-init (enable_mail={enable_mail}) must not render \
                     `{banned}` — it can defeat the in-image bridge-isolation \
                     hardening: {output}"
                );
            }
            // The nest data volume is the compose-managed named volume — a
            // host-path bind onto /data/keys or /data/keys/blessed could
            // substitute the blessed registry from outside the container.
            assert!(
                output.contains("- fauna-data:/data"),
                "nest /data must be the named volume: {output}"
            );
            assert!(
                !output.contains(":/data/keys"),
                "nothing may bind-mount over the key/registry tree: {output}"
            );
        }
    }

    /// The mail bridge's scan gate is default-on + fail-closed, so an
    /// orchestrator-provisioned box must ship the clamd/rspamd sidecars the
    /// canonical docker-compose.yml carries — otherwise the bridge dials an
    /// unreachable scanner and 451s every inbound message once mail is enabled
    /// (`installers/docker.md` § content-scan sidecars; flow-traced in
    /// `bins/fauna-bridges/internal/mta/scan_gate.go`).
    #[test]
    fn test_cloud_init_has_content_scan_sidecars() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        // The two sidecar services and their images.
        assert!(output.contains("clamd:"), "missing clamd service: {output}");
        assert!(
            output.contains("image: clamav/clamav:latest-debian"),
            "missing clamav image"
        );
        assert!(output.contains("rspamd:"), "missing rspamd service");
        assert!(
            output.contains("image: rspamd/rspamd:latest"),
            "missing rspamd image"
        );
        // The bridge's scan-gate addresses (entrypoint.sh writes these into the
        // operator-hatch → hatch.ClamdAddr / RspamdURL).
        assert!(
            output.contains("FAUNA_CLAMD_ADDR: clamd:3310"),
            "missing FAUNA_CLAMD_ADDR env"
        );
        assert!(
            output.contains("FAUNA_RSPAMD_URL: http://rspamd:11333"),
            "missing FAUNA_RSPAMD_URL env"
        );
        // rspamd's stock image binds localhost only — the worker must bind all
        // interfaces so the nest container can POST /checkv2 over the network.
        assert!(
            output.contains(r#"bind_socket = "*:11333";"#),
            "missing rspamd all-interfaces worker bind"
        );
    }

    /// The cloud-init compose must carry the same json-log rotation backstop as
    /// the canonical docker-compose.yml — clamd's freshclam is log-chatty, and
    /// an unbounded log once wedged a node at 69 GB (`installers/vps.md`).
    #[test]
    fn test_cloud_init_has_log_rotation() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        assert!(
            output.contains("x-logging: &default-logging"),
            "missing x-logging anchor"
        );
        assert!(
            output.contains(r#"max-size: "50m""#),
            "missing max-size cap"
        );
        assert!(
            output.contains("logging: *default-logging"),
            "services must reference the logging anchor"
        );
    }

    #[test]
    fn test_cloud_init_contains_claim_code() {
        let params = CloudInitParams {
            domain: "test.example.com".into(),
            image_tag: "latest".into(),
            claim_code: "A1B2C3".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        assert!(
            output.contains("/opt/fauna/claim-code"),
            "cloud-init must write the claim code file on the host"
        );
        assert!(
            output.contains("A1B2C3"),
            "cloud-init must contain the actual claim code value"
        );
        // The seed is staged OUTSIDE /data so the entrypoint can copy it into
        // the writable volume (the uid-1000 nest must read + delete it). A bind
        // straight onto /data/claim-code is the read-only-mount boot crash bug.
        assert!(
            output.contains("/opt/fauna/claim-code:/run/fauna/claim-code-seed:ro"),
            "claim code must be staged off /data (not mounted onto /data/claim-code)"
        );
        assert!(
            !output.contains("/data/claim-code:ro"),
            "claim code must NOT be mounted read-only directly onto /data/claim-code"
        );
    }

    /// Box-recovery (`box-recovery.md` § Mechanism — Capture, client-provisioned
    /// cloud VPS): when the client generates + injects a deployment seed, the
    /// compose `environment:` block must carry `FAUNA_DEPLOYMENT_SEED: <seed>`
    /// so the box adopts that identity at boot (the nest reads it via
    /// `deployment_key::deployment_seed_from_env`) instead of minting its own —
    /// the off-box-custodied seed that makes total-box-loss recovery work. When
    /// `None` the env line is **absent** and the box mints its own random seed
    /// (today's behavior + the claim-an-existing-box origin), so a pre-recovery
    /// / claim-path provision is byte-for-byte unchanged.
    #[test]
    fn test_cloud_init_injects_deployment_seed() {
        let seed = "a".repeat(64);
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: Some(seed.clone()),
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        // Injected into the compose env (12-space indent, alongside FAUNA_MODE).
        assert!(
            output.contains(&format!("\n            FAUNA_DEPLOYMENT_SEED: {seed}")),
            "deployment seed must be injected into the compose env block: {output}"
        );

        // Absent when None — the box mints its own (unchanged default).
        let params_none = CloudInitParams {
            deployment_seed: None,
            ..params
        };
        let output_none = build_cloud_init(&params_none);
        assert!(
            !output_none.contains("FAUNA_DEPLOYMENT_SEED"),
            "no seed env when deployment_seed is None: {output_none}"
        );
    }

    /// The update-channel seed (`nest/update-channel.md` § The channel as nest
    /// state → *The seed*): the nest's environment repeats the literal tag the
    /// compose wrote into `image:`, so the box follows the channel the wizard
    /// chose until an admin picks another from the app.
    #[test]
    fn test_cloud_init_seeds_the_update_channel_from_the_image_tag() {
        for tag in ["dev", "test", "latest"] {
            let output = build_cloud_init(&CloudInitParams {
                domain: "x.com".into(),
                image_tag: tag.into(),
                claim_code: "AABBCC".into(),
                enable_mail: false,
                deployment_seed: None,
                ssh_door: None,
            });
            assert!(
                output.contains(&format!("image: ghcr.io/faunasocial/nest:{tag}")),
                "{output}"
            );
            assert!(
                output.contains(&format!("\n            FAUNA_IMAGE_TAG: {tag}\n")),
                "the nest's env must carry the seed beside FAUNA_MODE: {output}"
            );
        }
    }

    /// The updater replaces Watchtower (`nest/update-channel.md` § The
    /// bundles): a second container of the nest image at the same tag, its
    /// entrypoint overridden; the nest — and only the nest — carries the label
    /// that puts it under the updater's management.
    #[test]
    fn test_cloud_init_runs_the_updater_from_the_nest_image() {
        let output = build_cloud_init(&CloudInitParams {
            domain: "x.com".into(),
            image_tag: "test".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        });
        let updater = output
            .split("\n        fauna-updater:\n")
            .nth(1)
            .expect("a fauna-updater service");
        let updater = &updater[..updater.find("\n        clamd:").unwrap_or(updater.len())];
        assert!(
            updater.contains("image: ghcr.io/faunasocial/nest:test\n"),
            "{updater}"
        );
        assert!(updater.contains(r#"entrypoint: ["/usr/local/bin/fauna-updater"]"#));
        assert!(updater.contains("restart: unless-stopped"));
        assert_eq!(output.matches("social.fauna.update=true").count(), 1);
        assert!(!output.to_lowercase().contains("watchtower"), "{output}");
    }

    /// DKIM keys are nest-minted (when the mail domain is added, and at boot)
    /// and published post-boot from the nest's DKIM selector list; cloud-init
    /// must NOT embed a client-generated key or it would publish a key the
    /// nest never signs with.
    #[test]
    fn test_cloud_init_embeds_no_dkim_key() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        assert!(
            !output.contains("dkim"),
            "cloud-init must not embed DKIM material: {output}"
        );
        assert!(
            !output.contains("FAUNA_DKIM"),
            "cloud-init must not set DKIM env vars: {output}"
        );
    }

    /// A mail box's compose names no `FAUNA_IMAP`: nothing in the image reads it
    /// (`installers/docker.md` § Environment Variables), so emitting it would
    /// publish a knob that configures nothing. What provisions a box for mail
    /// is the ports, the scan env and the sidecars; what serves mail is the
    /// admin's in-app toggle.
    #[test]
    fn test_cloud_init_mail_enabled_emits_no_imap_env() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        assert!(
            !output.contains("FAUNA_IMAP"),
            "a mail box must not emit the dead FAUNA_IMAP env: {output}"
        );
        assert!(
            output.contains("FAUNA_CLAMD_ADDR: clamd:3310"),
            "the mail env block itself must survive: {output}"
        );
    }

    /// A **social-only** box (`enable_mail == false`) provisions a lean
    /// nest+updater compose: NO mail ports, NO content-scan env, NO ~1.5 GB
    /// clamd/rspamd sidecars, and NO mail `ufw` rules — keeping the 1 GB VPS tier viable
    /// (`installers/vps.md` § Minimum VPS Requirements). The base nest surface
    /// (HTTP/HTTPS ports, volume, updater) is unchanged.
    #[test]
    fn test_cloud_init_social_only_omits_mail() {
        let params = CloudInitParams {
            domain: "social.example.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);

        // Still valid cloud-config with the base nest surface.
        assert!(output.starts_with("#cloud-config"));
        assert!(
            !output.contains("FAUNA_DOMAIN"),
            "box boots domainless — no FAUNA_DOMAIN env even on a social-only box"
        );
        assert!(output.contains("FAUNA_MODE: public"), "mode must remain");
        for keep in [
            "80:8080",
            "443:443",
            "fauna-data:",
            "fauna-updater:",
            "stop_grace_period: 15s",
            "ufw allow 443/tcp",
            "7842:7842/udp",
            "ufw allow 7842/udp",
            "x-logging: &default-logging",
        ] {
            assert!(
                output.contains(keep),
                "social-only box dropped {keep}: {output}"
            );
        }

        // Mail subsystem must be entirely absent.
        for mail in [
            "25:25",
            "465:465",
            "587:587",
            "993:993",
            "FAUNA_IMAP",
            "FAUNA_CLAMD_ADDR",
            "FAUNA_RSPAMD_URL",
            "clamav/clamav",
            "rspamd/rspamd",
            "configs:",
            "bind_socket",
            "ufw allow 25/tcp",
            "ufw allow 465/tcp",
            "ufw allow 587/tcp",
            "ufw allow 993/tcp",
        ] {
            assert!(
                !output.contains(mail),
                "social-only box must omit mail artifact {mail}: {output}"
            );
        }

        // Host-OS maintenance is unconditional — it must be present on a
        // social-only box too (it has nothing to do with mail).
        assert!(
            output.contains("fauna-reboot-coordinator.timer"),
            "social-only box must still carry host-OS maintenance"
        );
    }

    /// Host-OS auto-patching (installers/vps.md § Host OS Maintenance, layer 1):
    /// cloud-init must install + configure `unattended-upgrades` with the apt
    /// timers enabled, auto-reboot OFF (the coordinator owns reboots), and
    /// `needrestart` set to list-only so a lib update never bounces docker.
    #[test]
    fn test_cloud_init_compose_is_root_only() {
        // The compose env can carry FAUNA_DEPLOYMENT_SEED, so the file must not
        // be world-readable (the cloud-init write_files default is 0644).
        let output = build_cloud_init(&CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
            ssh_door: None,
        });
        assert!(
            output.contains("        fauna-data:\n    permissions: '0600'"),
            "docker-compose.yml must be written 0600 (it can hold the deployment seed)"
        );
    }

    /// A sidecar holds no secret and no socket
    /// (`deployment-threat-model.md` § Concern 1a mitigation 1). Audited on the
    /// RENDERED compose with mail on — what a provisioned box actually runs. The
    /// repo-file and guide copies are audited by the private fleet's compose-sidecar audit.
    #[test]
    fn test_cloud_init_sidecars_hold_no_secret_or_socket() {
        let output = build_cloud_init(&CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: Some("SEEDSEED".into()),
            ssh_door: None,
        });
        /// Non-comment lines of the `<service>:` block (6-space-indented compose).
        fn block(out: &str, service: &str) -> Vec<String> {
            let lines: Vec<&str> = out.lines().collect();
            let header = format!("{service}:");
            let start = lines
                .iter()
                .position(|l| l.trim() == header)
                .unwrap_or_else(|| panic!("rendered compose has no `{service}:` service"));
            let indent = lines[start].len() - lines[start].trim_start().len();
            lines[start + 1..]
                .iter()
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .take_while(|l| l.len() - l.trim_start().len() > indent)
                .map(|l| l.trim().to_string())
                .collect()
        }
        for service in ["clamd", "rspamd"] {
            let b = block(&output, service);
            assert!(!b.is_empty(), "{service} block empty");
            for forbidden in ["fauna-data", "docker.sock", "/opt/fauna"] {
                assert!(
                    !b.iter().any(|l| l.contains(forbidden)),
                    "{service} must not mention {forbidden}: {b:?}"
                );
            }
            for key in [
                "environment:",
                "env_file:",
                "secrets:",
                "privileged:",
                "network_mode:",
            ] {
                assert!(
                    !b.iter().any(|l| l.starts_with(key)),
                    "{service} must not carry `{key}`: {b:?}"
                );
            }
        }
        // The updater holds the one socket and nothing else: no data volume, no
        // host state, no environment, no port (nest/update-channel.md § The
        // updater → *What it holds*).
        let w = block(&output, "fauna-updater");
        assert!(w.iter().any(|l| l.contains("/var/run/docker.sock")));
        for forbidden in [
            "fauna-data",
            "/opt/fauna",
            "environment:",
            "ports:",
            "env_file:",
        ] {
            assert!(
                !w.iter().any(|l| l.contains(forbidden)),
                "fauna-updater must not carry {forbidden}: {w:?}"
            );
        }
        // The socket is named nowhere else in the rendered compose.
        assert_eq!(
            output
                .lines()
                .filter(|l| l.contains("/var/run/docker.sock") && !l.trim_start().starts_with('#'))
                .count(),
            1,
            "docker.sock must appear exactly once (fauna-updater)"
        );
    }

    #[test]
    fn test_cloud_init_has_unattended_upgrades() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        assert!(
            output.contains("- unattended-upgrades"),
            "unattended-upgrades must be in packages"
        );
        assert!(
            output.contains("- needrestart"),
            "needrestart must be in packages"
        );
        assert!(
            output.contains("- docker-compose-v2"),
            "the docker compose v2 plugin must be an explicit package (runcmd uses `docker compose`)"
        );
        assert!(
            output.contains(r#"APT::Periodic::Unattended-Upgrade "1";"#),
            "20auto-upgrades must enable unattended upgrade"
        );
        assert!(
            output.contains(r#"Unattended-Upgrade::Automatic-Reboot "false";"#),
            "auto-reboot must be OFF — the coordinator owns reboots"
        );
        assert!(
            output.contains(r#"${distro_id}:${distro_codename}-security"#),
            "security origin must be allowed"
        );
        assert!(
            output.contains("$nrconf{restart} = 'l';"),
            "needrestart must be list-only (defer service restarts)"
        );
    }

    /// Host-OS auto-patching (layer 2): the reboot-coordinator script, its
    /// systemd timer/service, the two trust-split bind mounts, the runcmd that
    /// creates them (the rw one owned by uid 1000, the host one left root-owned),
    /// the systemd sandbox, and the un-forgeable ceiling clock must all be present.
    #[test]
    fn test_cloud_init_has_reboot_coordinator() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
            ssh_door: None,
        };
        let output = build_cloud_init(&params);
        for needle in [
            "/usr/local/sbin/fauna-reboot-coordinator",
            "fauna-reboot-coordinator.service",
            "fauna-reboot-coordinator.timer",
            "OnUnitActiveSec=15min",
            // INFO-1 relies on OnBootSec (300 s) < MIN_REBOOT_UPTIME_SECS (600 s):
            // the anti-loop guard refuses a re-planted restart-now on the first run.
            "OnBootSec=5min",
            // Trust-split channel: rw uid-1000 dir + ro root-owned host dir.
            "- /opt/fauna/maintenance:/data/maintenance",
            "- /opt/fauna/maintenance-host:/data/maintenance-host:ro",
            "mkdir -p /opt/fauna/maintenance",
            "chown 1000:1000 /opt/fauna/maintenance",
            "mkdir -p /opt/fauna/maintenance-host",
            "chmod 0755 /opt/fauna/maintenance-host",
            "systemctl enable --now fauna-reboot-coordinator.timer",
            // systemd sandbox (HM-1 defense-in-depth).
            "ProtectSystem=strict",
            "ReadWritePaths=/opt/fauna/maintenance /opt/fauna/maintenance-host",
            "NoNewPrivileges=true",
            "PrivateTmp=true",
            // HM-3: the readiness read is `timeout`-bounded (FIFO-hang-proof) and
            // the oneshot has a start-timeout backstop so a wedge self-clears.
            "timeout 5 grep '^connection_count='",
            "TimeoutStartSec=120",
            // INFO-1: the min-reboot-uptime anti-loop guard + its un-forgeable
            // host-uptime clock.
            "MIN_REBOOT_UPTIME_SECS=600",
            "/proc/uptime",
            // Un-forgeable ceiling clock (HM-2): host-owned /run/reboot-required.
            "stat -c %Y /run/reboot-required",
            // The pure decision fn must ship in the generated script.
            "fauna_reboot_decision",
            // The admin "restart now" flag the nest writes / coordinator consumes.
            "restart-requested",
        ] {
            assert!(
                output.contains(needle),
                "reboot-coordinator artifact missing: {needle}"
            );
        }
        // The forgeable container-writable deferred-stamp FILE is gone (HM-2): the
        // ceiling now derives from the host-owned /run/reboot-required mtime. (The
        // `reboot_deferred_since` host-status KEY, underscored, still ships.)
        assert!(
            !output.contains("reboot-deferred-since"),
            "the container-writable reboot-deferred-since stamp file must be gone"
        );
    }

    /// The reboot-coordinator's pure decision function is the testable core
    /// (installers/vps.md § Host OS Maintenance step 2): reboot iff a reboot is
    /// pending AND (the nest is idle OR the 24 h ceiling has elapsed). We source
    /// the exact shell that ships and feed it fixture arguments via `sh`.
    #[test]
    fn test_reboot_decision_logic() {
        // (reboot_pending, connection_count, reboot_deferred_since, now, ceiling, expected)
        let cases = [
            ("false", "0", "", "100", "86400", "hold"), // no reboot pending → never
            ("true", "0", "", "100", "86400", "reboot"), // idle → reboot now
            ("true", "3", "", "100", "86400", "hold"),  // busy, never deferred → hold
            ("true", "3", "50", "100", "86400", "hold"), // busy, under ceiling → hold
            ("true", "3", "10", "100000", "86400", "reboot"), // busy, past ceiling → reboot
            // Ceiling boundary is strict (`-gt`): exactly at the ceiling still holds…
            ("true", "3", "0", "86400", "86400", "hold"), // now-rds == ceiling → hold
            ("true", "3", "0", "86401", "86400", "reboot"), // now-rds == ceiling+1 → reboot
            // A non-numeric reboot_deferred_since ⇒ "no ceiling known" ⇒ hold (busy).
            ("true", "3", "notanumber", "100000", "86400", "hold"),
        ];
        for (rp, cc, rds, now, ceil, expected) in cases {
            let script = format!(
                "{REBOOT_COORDINATOR_SCRIPT}\nfauna_reboot_decision {rp} {cc} '{rds}' {now} {ceil}\n"
            );
            let out = std::process::Command::new("sh")
                .env("FAUNA_COORDINATOR_TEST", "1")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("run sh");
            let got = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                got.trim(),
                expected,
                "decision({rp},{cc},{rds:?},{now},{ceil}) stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The admin "restart now" request (the 6th arg) overrides idle + ceiling:
    /// the admin explicitly chose to reboot immediately
    /// (installers/vps.md § Host OS Maintenance step 4). A `false` (or absent)
    /// restart_now leaves the idle/ceiling logic intact.
    #[test]
    fn test_restart_now_overrides_decision() {
        // (reboot_pending, connection_count, reboot_deferred_since, now, ceiling, restart_now, expected)
        let cases = [
            // restart_now=true reboots even with NO pending reboot and a busy nest.
            ("false", "5", "", "100", "86400", "true", "reboot"),
            // …and even when a reboot is pending but under the ceiling on a busy box.
            ("true", "5", "50", "100", "86400", "true", "reboot"),
            // restart_now=false leaves the normal "busy, under ceiling → hold" path.
            ("true", "5", "50", "100", "86400", "false", "hold"),
        ];
        for (rp, cc, rds, now, ceil, rn, expected) in cases {
            let script = format!(
                "{REBOOT_COORDINATOR_SCRIPT}\nfauna_reboot_decision {rp} {cc} '{rds}' {now} {ceil} {rn}\n"
            );
            let out = std::process::Command::new("sh")
                .env("FAUNA_COORDINATOR_TEST", "1")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("run sh");
            let got = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                got.trim(),
                expected,
                "decision({rp},{cc},{rds:?},{now},{ceil},restart_now={rn}) stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// HM-3 regression (2026-06-28 second-pass review): a compromised uid-1000
    /// container can TOCTOU-swap a writer-less FIFO over the readiness file. The
    /// hang-proof read (`timeout`-bounded grep) must still return —
    /// `connection_count=0`, the benign idle path — instead of blocking this
    /// `Type=oneshot` coordinator forever and suppressing every future security
    /// reboot. We source the shipped script and feed `fauna_read_connection_count`
    /// a real FIFO; pre-fix this call hangs indefinitely
    /// (`installers/vps.md` § Host OS Maintenance step 3).
    #[test]
    fn test_readiness_read_hang_proof_on_fifo() {
        let dir = std::env::temp_dir().join(format!("fauna-hm3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("nest-readiness");
        let _ = std::fs::remove_file(&fifo);
        let mk = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo available (coreutils)");
        assert!(mk.success(), "mkfifo failed");

        // Source the shipped script (functions only — FAUNA_COORDINATOR_TEST skips
        // main) and call the hang-proof reader against the writer-less FIFO.
        let script = format!(
            "{REBOOT_COORDINATOR_SCRIPT}\nfauna_read_connection_count '{}'\n",
            fifo.display()
        );
        let start = std::time::Instant::now();
        let out = std::process::Command::new("sh")
            .env("FAUNA_COORDINATOR_TEST", "1")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run sh");
        let elapsed = start.elapsed();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "0",
            "a writer-less FIFO must read as idle 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The `timeout 5` cap means the bounded read returns in ~5 s; without it the
        // grep would block forever (the test would hang, not just exceed 30 s).
        assert!(
            elapsed.as_secs() < 30,
            "readiness read hung on a FIFO ({elapsed:?}) — HM-3 not closed"
        );
    }

    /// `fauna_read_connection_count` is the DEFENSIVE, argument-only readiness
    /// reader (installers/vps.md § Host OS Maintenance step 3). Beyond the
    /// FIFO-hang property (covered above) it must: read a real count from a
    /// regular file, REJECT a symlink (an HM-1-class defense — a compromised
    /// uid-1000 nest could point the readiness path at a host file), digit-validate
    /// the value, and read a missing/keyless/non-numeric file as the benign idle 0
    /// (forcing "idle" only ACCELERATES a crash-safe reboot, never suppresses one).
    /// We source the shipped script and call the fn against planted files.
    /// unix-only: plants a real symlink (`std::os::unix::fs::symlink`) and
    /// shells `sh` — the script under test only ever runs on a Linux box.
    ///
    /// Gated to **linux**, not `unix`: the script's `timeout 5 grep …` is GNU
    /// coreutils, which macOS does not ship, so on a mac `timeout` exits 127,
    /// the grep never runs, and the fn returns the 0 fallback — failing the
    /// happy-path assertion for a reason that says nothing about the code. The
    /// original `#[cfg(unix)]` was aimed at excluding Windows and
    /// swept macOS in by accident.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_read_connection_count_defensive() {
        let dir = std::env::temp_dir().join(format!("fauna-hm-rcc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let read_count = |path: &std::path::Path| -> String {
            let script = format!(
                "{REBOOT_COORDINATOR_SCRIPT}\nfauna_read_connection_count '{}'\n",
                path.display()
            );
            let out = std::process::Command::new("sh")
                .env("FAUNA_COORDINATOR_TEST", "1")
                .arg("-c")
                .arg(&script)
                .output()
                .expect("run sh");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // Happy path: a real regular file → the parsed count.
        let ok = dir.join("readiness-ok");
        std::fs::write(&ok, "connection_count=3\n").unwrap();
        assert_eq!(
            read_count(&ok),
            "3",
            "a regular readiness file must yield its count"
        );

        // The grep extracts the connection_count line out of a multi-key file.
        let multi = dir.join("readiness-multi");
        std::fs::write(&multi, "generated_at=1\nconnection_count=7\nother=x\n").unwrap();
        assert_eq!(
            read_count(&multi),
            "7",
            "must extract connection_count from a multi-key file"
        );

        // Symlink rejection (HM-1-class): a compromised nest could symlink the
        // readiness path at a host file; the `[ ! -L ]` guard must reject it → idle 0
        // even though the target is a valid regular file with a real count.
        let secret = dir.join("host-secret");
        std::fs::write(&secret, "connection_count=99\n").unwrap();
        let link = dir.join("readiness-symlink");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert_eq!(
            read_count(&link),
            "0",
            "a symlinked readiness file must be rejected (read as idle 0)"
        );

        // Non-numeric value → idle 0 (digit-validation).
        let nan = dir.join("readiness-nan");
        std::fs::write(&nan, "connection_count=notanumber\n").unwrap();
        assert_eq!(
            read_count(&nan),
            "0",
            "a non-numeric count must read as idle 0"
        );

        // A file with no connection_count key → idle 0.
        let keyless = dir.join("readiness-keyless");
        std::fs::write(&keyless, "other=1\n").unwrap();
        assert_eq!(
            read_count(&keyless),
            "0",
            "a keyless file must read as idle 0"
        );

        // A missing file → idle 0.
        assert_eq!(
            read_count(&dir.join("does-not-exist")),
            "0",
            "a missing readiness file must read as idle 0"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hand-typed compose in `docs/guides/nest-internet-setup.md` § Step 5 is
    /// the SAME deployment as the one `build_cloud_init` hands a cloud provider
    /// — that guide is simply the do-it-yourself path to the same box. Two
    /// copies of a deployment artifact drift silently (the guide family already
    /// drifted four ways: `get.docker.com` vs the archive packages, a retired
    /// GHCR token, `/srv/fauna` vs `/opt/fauna`, a `.env` that does not exist),
    /// and a drifted guide ships a subtly different box than the app does. So
    /// pin them to each other here: this is the enforcement behind the
    /// MAINTAINERS note at the foot of that guide.
    ///
    /// Compared modulo comments and blank lines — the guide is written for a
    /// beginner and carries none of the rationale comments the rendered artifact
    /// does, but every functional line (images, ports, volumes, env, labels,
    /// restart/stop policy, the rspamd config block) must match exactly.
    #[test]
    fn test_internet_nest_guide_matches_rendered_compose() {
        /// Drop comment-only and blank lines; keep everything else verbatim
        /// (indentation included — YAML structure is significant).
        fn functional_lines(yaml: &str) -> Vec<&str> {
            yaml.lines()
                .map(|l| l.trim_end())
                .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .collect()
        }

        // The compose the app ships, lifted out of the cloud-init `write_files`
        // entry (6-space `content: |` block scalar).
        let rendered = build_cloud_init(&CloudInitParams {
            domain: "example.com".into(),
            image_tag: "latest".into(),
            claim_code: "CLAIMCODE".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door: None,
        });
        let body = rendered
            .split_once("  - path: /opt/fauna/docker-compose.yml\n    content: |\n")
            .expect("cloud-init must write /opt/fauna/docker-compose.yml")
            .1
            .split_once("    permissions: '0600'")
            .expect("the compose write_files entry must carry permissions")
            .0;
        let shipped: String = body
            .lines()
            .map(|l| if l.len() >= 6 { &l[6..] } else { l })
            .collect::<Vec<_>>()
            .join("\n");

        // The compose the guide tells a user to type.
        let guide_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/guides/nest-internet-setup.md");
        let guide = std::fs::read_to_string(&guide_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", guide_path.display()));
        let guide_yaml = guide
            .split_once("```yaml\n")
            .expect("nest-internet-setup.md must carry a ```yaml compose block")
            .1
            .split_once("\n```")
            .expect("the guide's yaml block must be closed")
            .0;

        let want = functional_lines(&shipped);
        let got = functional_lines(guide_yaml);
        if want != got {
            let first_diff = want
                .iter()
                .zip(got.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(want.len().min(got.len()));
            panic!(
                "docs/guides/nest-internet-setup.md § Step 5 has drifted from \
                 build_cloud_init.\n\nFirst difference at functional line {first_diff}:\n  \
                 cloud_init.rs: {:?}\n  the guide:     {:?}\n\n\
                 ({} functional lines shipped, {} in the guide.)\n\n\
                 Update BOTH in the same commit — the guide is the do-it-yourself \
                 path to the very same box.",
                want.get(first_diff),
                got.get(first_diff),
                want.len(),
                got.len(),
            );
        }
    }

    // ── host-addresses publication (installers/vps.md § Host OS Maintenance → 3;
    //    nest/tls-certificates.md § B-IP *When*, source (2)) ────────────────────

    /// Run `body` against the sourced coordinator script (functions only —
    /// `FAUNA_COORDINATOR_TEST=1` skips the entry point), feeding `stdin`, and
    /// return stdout.
    fn run_coordinator_fns(body: &str, stdin: &str) -> String {
        use std::io::Write;
        let script = format!("{REBOOT_COORDINATOR_SCRIPT}\n{body}\n");
        let mut child = std::process::Command::new("sh")
            .env("FAUNA_COORDINATOR_TEST", "1")
            .arg("-c")
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run sh");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        let out = child.wait_with_output().expect("sh output");
        assert!(
            out.status.success(),
            "coordinator fns failed: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Canned `ip -o addr show` output from a provisioned VPS: loopback, a
    /// public v4 + v6 on the uplink, the v6 link-local, Docker's RFC 1918
    /// bridge, a ULA, a CGNAT address and two more private ranges. Only the
    /// two global addresses survive the coarse host-side filter (the nest
    /// re-applies `is_global_ip`).
    const CANNED_IP_O_ADDR: &str = "\
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever
1: lo    inet6 ::1/128 scope host noprefixroute \\       valid_lft forever preferred_lft forever
2: eth0    inet 93.184.216.34/32 metric 100 scope global dynamic eth0\\       valid_lft 85000sec preferred_lft 85000sec
2: eth0    inet6 2a01:4f8:c012:1234::1/64 scope global \\       valid_lft forever preferred_lft forever
2: eth0    inet6 fe80::9400:3ff:fe12:3456/64 scope link \\       valid_lft forever preferred_lft forever
3: docker0    inet 172.17.0.1/16 brd 172.17.255.255 scope global docker0\\       valid_lft forever preferred_lft forever
4: wg0    inet6 fd00:1234::1/64 scope global \\       valid_lft forever preferred_lft forever
5: tun0    inet 100.64.0.7/10 scope global tun0\\       valid_lft forever preferred_lft forever
6: eth1    inet 10.0.0.2/32 brd 10.0.0.2 scope global dynamic eth1\\       valid_lft forever preferred_lft forever
7: eth2    inet 192.168.1.5/24 scope global eth2\\       valid_lft forever preferred_lft forever
";

    /// The pure filter turns the host's interface listing into one
    /// `address=<ip>` line per global-unicast address, v4 and v6, dropping
    /// loopback, link-local, ULA, RFC 1918 and CGNAT.
    #[test]
    fn test_host_address_lines_filters_the_interface_listing() {
        let got = run_coordinator_fns("fauna_host_address_lines", CANNED_IP_O_ADDR);
        assert_eq!(
            got, "address=93.184.216.34\naddress=2a01:4f8:c012:1234::1\n",
            "only the uplink's global v4 + v6 may be published"
        );
    }

    /// A line whose address field carries anything but hex digits, `.` and `:`
    /// is dropped, never echoed into the root-owned file.
    #[test]
    fn test_host_address_lines_drops_malformed_addresses() {
        let got = run_coordinator_fns(
            "fauna_host_address_lines",
            "2: eth0    inet 93.184.216.34;rm/32 scope global eth0\n\
             2: eth0    inet6 2a01:4f8::$(id)/64 scope global \n",
        );
        assert_eq!(got, "", "malformed addresses must not be published");
    }

    /// The writer lands the file atomically (tmp + rename) at the path it is
    /// given, leaving no `.tmp` behind.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_write_host_addresses_is_atomic_at_the_given_path() {
        let dir = std::env::temp_dir().join(format!("fauna-hostaddr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("host-addresses");
        run_coordinator_fns(
            &format!("fauna_write_host_addresses '{}'", dest.display()),
            CANNED_IP_O_ADDR,
        );
        let body = std::fs::read_to_string(&dest).expect("host-addresses written");
        let tmp_left = dir.join("host-addresses.tmp").exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            body,
            "address=93.184.216.34\naddress=2a01:4f8:c012:1234::1\n"
        );
        assert!(
            !tmp_left,
            "the tmp file must be renamed over, never left behind"
        );
    }

    /// `publish-addresses` publishes and exits WITHOUT the reboot decision (a
    /// freshly patched box may already carry /run/reboot-required at first
    /// boot); a bare run is the full coordinator. Stubs stand in for the two
    /// effectful functions so the dispatch itself is what is pinned.
    #[test]
    fn test_publish_addresses_entry_point_skips_the_reboot_decision() {
        let stubs = "fauna_publish_host_addresses() { echo publish; }\n\
                     fauna_coordinator_main() { echo main; }\n";
        assert_eq!(
            run_coordinator_fns(
                &format!("{stubs}fauna_coordinator_entry publish-addresses"),
                ""
            ),
            "publish\n"
        );
        assert_eq!(
            run_coordinator_fns(&format!("{stubs}fauna_coordinator_entry"), ""),
            "main\n"
        );
    }

    /// The publication lands in the root-owned host→nest dir, never the
    /// container-owned rw one; the coordinator publishes on every run; and the
    /// first-boot publish runs after the host dir exists and BEFORE
    /// `docker compose up`, so the nest's first lifecycle tick sees it.
    #[test]
    fn test_cloud_init_publishes_host_addresses_before_compose_up() {
        assert!(REBOOT_COORDINATOR_SCRIPT.contains("HOST_ADDRESSES=\"$HOST_DIR/host-addresses\""));
        assert!(!REBOOT_COORDINATOR_SCRIPT.contains("$MAINT_DIR/host-addresses"));
        let main_body = REBOOT_COORDINATOR_SCRIPT
            .split_once("fauna_coordinator_main() {")
            .expect("main fn")
            .1
            .split_once("\n}\n")
            .expect("main fn end")
            .0;
        assert!(
            main_body.contains("fauna_publish_host_addresses"),
            "every coordinator run must refresh host-addresses"
        );

        let output = build_cloud_init(&CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
            ssh_door: None,
        });
        let runcmd = output.split_once("\nruncmd:\n").expect("runcmd").1;
        let publish = runcmd
            .find("- /usr/local/sbin/fauna-reboot-coordinator publish-addresses")
            .expect("first-boot publish in runcmd");
        let host_dir = runcmd
            .find("chmod 0755 /opt/fauna/maintenance-host")
            .expect("host dir created in runcmd");
        let compose_up = runcmd
            .find("- systemctl start --no-block fauna-nest.service")
            .expect("first-boot unit started in runcmd");
        assert!(
            host_dir < publish,
            "publish only once the root-owned dir exists"
        );
        assert!(publish < compose_up, "publish before the nest comes up");
    }

    // ── the first-boot unit (installers/vps.md § What Happens Behind the
    //    Scenes, step 3) ───────────────────────────────────────────────────────

    /// The compose is never brought up by a one-shot runcmd line (a failed pull
    /// would strand a box whose sshd is already off): cloud-init writes the
    /// retry script + `fauna-nest.service`, and runcmd ENDS by enabling the unit
    /// and starting it without blocking, after the daemon-reload and the
    /// maintenance dirs.
    #[test]
    fn test_cloud_init_brings_the_nest_up_through_the_retry_unit() {
        for enable_mail in [false, true] {
            let output = build_cloud_init(&CloudInitParams {
                domain: "x.com".into(),
                image_tag: "dev".into(),
                claim_code: "AABBCC".into(),
                enable_mail,
                deployment_seed: None,
                ssh_door: None,
            });
            let (head, runcmd) = output.split_once("\nruncmd:\n").expect("runcmd");
            assert!(head.contains("  - path: /usr/local/sbin/fauna-nest-up\n"));
            assert!(head.contains(&indent_block(
                &script_without_comments(&nest_up_script()),
                6
            )));
            assert!(head.contains("  - path: /etc/systemd/system/fauna-nest.service\n"));
            assert!(head.contains(&indent_block(NEST_UP_SERVICE, 6)));
            assert!(
                !runcmd.contains("docker compose"),
                "runcmd must not run the compose itself: {runcmd}"
            );
            assert!(
                runcmd.trim_end().ends_with(NEST_UP_RUNCMD),
                "runcmd must end by enabling + no-block-starting the unit: {runcmd}"
            );
            let reload = runcmd.find("- systemctl daemon-reload").expect("reload");
            let maint = runcmd
                .find("- mkdir -p /opt/fauna/maintenance\n")
                .expect("maint dir");
            let start = runcmd
                .find("- systemctl start --no-block fauna-nest.service")
                .expect("start");
            assert!(reload < start && maint < start);
        }
        let script = nest_up_script();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains(&format!(
            "${{FAUNA_NEST_UP_INTERVAL_S:-{NEST_UP_RETRY_INTERVAL_SECS}}}"
        )));
        assert!(script.contains("until docker compose -f /opt/fauna/docker-compose.yml up -d;"));
        for needle in [
            "Requires=docker.service",
            "After=docker.service network-online.target",
            "Wants=network-online.target",
            "ExecStart=/usr/local/sbin/fauna-nest-up",
            "RemainAfterExit=yes",
            "WantedBy=multi-user.target",
        ] {
            assert!(NEST_UP_SERVICE.contains(needle), "unit missing {needle}");
        }
        // A oneshot still retrying would hold multi-user.target for the whole
        // registry outage (a target is ordered After= what it Wants).
        assert!(!NEST_UP_SERVICE.contains("Type=oneshot"));
    }

    /// The loop itself, against a stub `docker` that fails twice then succeeds:
    /// three attempts, a logged retry per failure, exit 0. (The containerised
    /// twin, `test_first_boot_nest_up.py`, runs it at its real path on Debian.)
    /// unix-only: the stubs are `chmod`ed executables on `PATH`, run by `sh`.
    #[cfg(unix)]
    #[test]
    fn test_nest_up_script_retries_until_compose_succeeds() {
        let dir = std::env::temp_dir().join(format!("fauna-nest-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let stub = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let p = dir.join(name);
            std::fs::write(&p, body).expect("write stub");
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        let calls = dir.join("docker.calls");
        stub(
            "docker",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{c}'\n[ \"$(wc -l < '{c}')\" -ge 3 ]\n",
                c = calls.display()
            ),
        );
        stub(
            "logger",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                dir.join("logger.calls").display()
            ),
        );
        stub("sleep", "#!/bin/sh\nexit 0\n");
        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = std::process::Command::new("sh")
            .env("PATH", path)
            .env("FAUNA_NEST_UP_INTERVAL_S", "0")
            .arg("-c")
            .arg(nest_up_script())
            .output()
            .expect("run sh");
        assert!(
            out.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let attempts = std::fs::read_to_string(&calls).expect("calls");
        assert_eq!(
            attempts.lines().collect::<Vec<_>>(),
            vec!["compose -f /opt/fauna/docker-compose.yml up -d"; 3]
        );
        let logged = std::fs::read_to_string(dir.join("logger.calls")).expect("logged");
        assert_eq!(logged.lines().count(), 2, "{logged}");
        assert!(logged.contains("-t fauna-nest-up compose up failed; retrying in 0 s"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── The SSH door (installers/vps.md § The SSH door) ─────────────────────

    const DOOR_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFaunaDoorTestKeyOnly fauna-door";

    fn door_params(ssh_door: Option<SshDoor>) -> CloudInitParams {
        CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
            ssh_door,
        }
    }

    fn door() -> Option<SshDoor> {
        Some(SshDoor {
            authorized_key: DOOR_KEY.into(),
        })
    }

    /// The largest render — mail, a deployment seed and the door — fits the
    /// tightest `user_data` limit of the adapters, Hetzner's 32,768 bytes.
    /// Measured 2026-10-20: the first live door create (dev.fauna.build) was
    /// refused `422 invalid input in field 'user_data'` — "Length must be
    /// between 0 and 32768" — while the fake cloud of the tier_3 door journey
    /// takes any length.
    #[test]
    fn test_cloud_init_largest_render_fits_the_user_data_limit() {
        let params = CloudInitParams {
            deployment_seed: Some("ab".repeat(32)),
            ..door_params(door())
        };
        let len = build_cloud_init(&params).len();
        assert!(
            len <= USER_DATA_MAX_BYTES,
            "the largest cloud-init is {len} bytes, over the {USER_DATA_MAX_BYTES}-byte limit \
             (door files {}, maintenance files {})",
            ssh_door_write_files(door().as_ref().unwrap()).len(),
            maintenance_write_files().len(),
        );
    }

    /// The box scripts reach the box without their full-line comments and
    /// nothing else: no heredoc (where a `#` line would be data), the shebang
    /// kept, every other line in order, and the result still parses as `sh`.
    #[test]
    fn test_box_scripts_lose_only_their_comment_lines() {
        for script in [
            REBOOT_COORDINATOR_SCRIPT.to_string(),
            SSH_DOOR_SCRIPT.to_string(),
            nest_up_script(),
        ] {
            assert!(
                !script.contains("<<"),
                "a heredoc makes comment-stripping unsafe"
            );
            let stripped = script_without_comments(&script);
            assert!(stripped.starts_with("#!/bin/sh\n"), "{stripped}");
            let kept: Vec<&str> = script
                .lines()
                .skip(1)
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect();
            assert_eq!(stripped.lines().skip(1).collect::<Vec<_>>(), kept);
            let out = std::process::Command::new("sh")
                .args(["-n", "-c", &stripped])
                .output()
                .expect("sh");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The door is purely additive: a door render, with the door's own
    /// fragments taken out and the sshd-off line put back, is the door-less
    /// render byte for byte — so `ssh_door: None` changes nothing on a box
    /// whose admin left the door shut (`deployment-threat-model.md` § Concern
    /// 1a), and nothing of the door leaks into one.
    #[test]
    fn test_cloud_init_door_less_render_is_unchanged() {
        let without = build_cloud_init(&door_params(None));
        for fragment in [
            "ssh_pwauth",
            "0-fauna-door",
            "authorized_keys",
            "fauna-ssh-door",
            "ufw allow 22",
        ] {
            assert!(
                !without.contains(fragment),
                "door-less render carries {fragment}"
            );
        }
        let with = build_cloud_init(&door_params(door()));
        let stripped = with
            .replacen("ssh_pwauth: false\n", "", 1)
            .replacen(&ssh_door_write_files(&door().unwrap()), "", 1)
            .replacen(SSH_DOOR_RUNCMD, "", 1)
            .replacen(
                "  - systemctl start docker\n",
                "  - systemctl start docker\n  - systemctl disable --now ssh.service ssh.socket\n",
                1,
            );
        assert_eq!(stripped, without);
    }

    /// With the door: key-only sshd for root (the four drop-in keywords, the
    /// cloud-config `ssh_pwauth: false`), the key installed in the final stage,
    /// sshd left enabled — and 22/tcp still denied and never allowed by the
    /// render: only the door pass opens it, to one address.
    #[test]
    fn test_cloud_init_door_renders_key_only_sshd() {
        let out = build_cloud_init(&door_params(door()));
        assert!(out.starts_with("#cloud-config\nssh_pwauth: false\n"));
        for line in [
            "  - path: /etc/ssh/sshd_config.d/0-fauna-door.conf",
            "      PasswordAuthentication no",
            "      KbdInteractiveAuthentication no",
            "      PermitRootLogin prohibit-password",
            "      AuthenticationMethods publickey",
            "  - path: /usr/local/sbin/fauna-ssh-door",
            "  - path: /etc/systemd/system/fauna-ssh-door.service",
            "  - path: /etc/systemd/system/fauna-ssh-door.timer",
            "  - chmod 0700 /root/.ssh",
            "  - systemctl enable --now fauna-ssh-door.timer",
            "  - ufw deny 22/tcp",
        ] {
            assert!(out.contains(line), "door render lacks {line:?}");
        }
        assert!(out.contains(&format!(
            "  - path: /root/.ssh/authorized_keys\n    content: |\n      {DOOR_KEY}\n    permissions: '0600'\n    defer: true\n"
        )));
        assert!(!out.contains("ufw allow 22"), "the render never opens 22");
        assert!(
            !out.contains("systemctl disable --now ssh.service ssh.socket"),
            "the door keeps sshd enabled"
        );
        // root's provider-expired password would refuse every key login at the
        // account check; the door clears the expiry (its own test below).
        assert!(
            out.contains(SSH_DOOR_UNEXPIRE_ROOT),
            "door render lacks the unexpire line"
        );
        // The door timer starts after the reboot coordinator's, i.e. after the
        // daemon-reload that sees the door units.
        let reload = out.find("  - systemctl daemon-reload").unwrap();
        let door_timer = out
            .find("  - systemctl enable --now fauna-ssh-door.timer")
            .unwrap();
        assert!(reload < door_timer);
    }

    /// Hetzner creates a keyless server with root's generated password already
    /// expired, so PAM's account check answers a key login with "You are
    /// required to change your password immediately" and ends any session
    /// without a TTY — measured 2026-10-20 on the first live door box
    /// (dev.fauna.build), whose key authenticated and whose session was then
    /// refused. The door resets root's last-change date to the day of the
    /// boot, in `runcmd` (after every cloud-config module that sets or expires
    /// a password); the password itself stays unknown and unusable over ssh
    /// (`PasswordAuthentication no`). A door-less box runs no sshd and keeps
    /// the provider's expiry untouched.
    #[test]
    fn test_door_clears_roots_provider_password_expiry() {
        assert_eq!(
            SSH_DOOR_UNEXPIRE_ROOT,
            "  - chage -d \"$(date +%Y-%m-%d)\" root"
        );
        let out = build_cloud_init(&door_params(door()));
        let runcmd = out.find("\nruncmd:\n").expect("runcmd");
        let unexpire = out
            .find(SSH_DOOR_UNEXPIRE_ROOT)
            .expect("the door clears the expiry");
        assert!(
            unexpire > runcmd,
            "the expiry is cleared in runcmd, after the config stage"
        );
        assert!(!build_cloud_init(&door_params(None)).contains("chage"));
    }

    /// sshd keeps the first value it reads per keyword, and the provider's
    /// cloud-init writes `PasswordAuthentication yes` into
    /// `50-cloud-init.conf`: the drop-in's name must sort before it.
    #[test]
    fn test_door_dropin_sorts_before_cloud_init_dropin() {
        let name = SSH_DOOR_DROPIN_PATH.rsplit('/').next().unwrap();
        assert_eq!(name, "0-fauna-door.conf");
        assert!(name < "50-cloud-init.conf");
    }

    /// Only the key's first line reaches the YAML: a value carrying a newline
    /// cannot inject a document key.
    #[test]
    fn test_door_key_newline_cannot_reach_the_document() {
        let out = build_cloud_init(&door_params(Some(SshDoor {
            authorized_key: format!("{DOOR_KEY}\nruncmd:\n  - id"),
        })));
        assert!(!out.contains("\nruncmd:\n  - id"));
        assert_eq!(out.matches("\nruncmd:\n").count(), 1);
    }

    /// Run `body` against the sourced door script (functions only —
    /// `FAUNA_SSH_DOOR_TEST=1` skips the pass) and return (status, stdout).
    fn run_door_fns(body: &str) -> (bool, String) {
        let script = format!("{SSH_DOOR_SCRIPT}\n{body}\n");
        let out = std::process::Command::new("sh")
            .env("FAUNA_SSH_DOOR_TEST", "1")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("run sh");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// The validator admits exactly one IPv4 or IPv6 literal and refuses
    /// everything ufw could read as more — a range, `any`, the unspecified
    /// address, a shell fragment.
    #[test]
    fn test_door_address_validation() {
        for good in [
            "203.0.113.7",
            "1.2.3.4",
            "255.255.255.255",
            "2001:db8::7",
            "2a01:4f8:c012:1234::1",
            "::1",
            "2001:db8:0:0:0:0:0:1",
            "::ffff:203.0.113.7",
        ] {
            let (ok, _) = run_door_fns(&format!("fauna_ssh_door_valid_address '{good}'"));
            assert!(ok, "{good} must be accepted");
        }
        for bad in [
            "",
            "any",
            "0.0.0.0",
            "::",
            "1.2.3",
            "1.2.3.4.5",
            "256.1.1.1",
            "01.2.3.4",
            "1.2.3.4/0",
            "1.2.3.4; rm -rf /",
            "$(id)",
            "1..2.3",
            "2001:db8:::1",
            "2001::db8::1",
            "2001:db8:0:0:0:0:0:0:1",
            "2001:db8:0:0:0:0:1",
            "12345::1",
            "fe80::1%eth0",
            "2001:db8::/32",
        ] {
            let (ok, _) = run_door_fns(&format!("fauna_ssh_door_valid_address '{bad}'"));
            assert!(!ok, "{bad:?} must be refused");
        }
    }

    /// A scratch box for the door pass: the maintenance + host dirs, the
    /// drop-in (when `door`), and a stub `ufw` logging its arguments.
    #[cfg(unix)]
    struct DoorBox {
        dir: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl DoorBox {
        fn new(tag: &str, door: bool) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("fauna-door-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for sub in ["maint", "host", "bin"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            if door {
                std::fs::write(dir.join("0-fauna-door.conf"), SSH_DOOR_DROPIN).unwrap();
            }
            let ufw = dir.join("bin/ufw");
            std::fs::write(
                &ufw,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                    dir.join("ufw.calls").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&ufw, std::fs::Permissions::from_mode(0o755)).unwrap();
            DoorBox { dir }
        }
        fn source(&self) -> std::path::PathBuf {
            self.dir.join("maint/ssh-door-source")
        }
        fn admitted(&self) -> std::path::PathBuf {
            self.dir.join("host/ssh-door-admitted")
        }
        fn admit(&self, now: &str) {
            let (ok, _) = run_door_fns(&format!(
                "PATH='{bin}':$PATH\nfauna_ssh_door_admit '{src}' '{dropin}' '{adm}' {now}",
                bin = self.dir.join("bin").display(),
                src = self.source().display(),
                dropin = self.dir.join("0-fauna-door.conf").display(),
                adm = self.admitted().display(),
            ));
            assert!(ok, "the door pass must not fail");
        }
        fn ufw_calls(&self) -> String {
            std::fs::read_to_string(self.dir.join("ufw.calls")).unwrap_or_default()
        }
    }

    #[cfg(unix)]
    impl Drop for DoorBox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The pass admits the claimant's address once, above the deny, and
    /// records it; a later write by the nest is ignored for good.
    #[cfg(unix)]
    #[test]
    fn test_door_pass_admits_once() {
        let b = DoorBox::new("once", true);
        b.admit("100");
        assert_eq!(b.ufw_calls(), "", "no source yet: nothing to admit");
        assert!(!b.admitted().exists());

        std::fs::write(b.source(), "address=203.0.113.7\n").unwrap();
        b.admit("200");
        assert_eq!(
            b.ufw_calls(),
            "prepend allow proto tcp from 203.0.113.7 to any port 22\n"
        );
        assert_eq!(
            std::fs::read_to_string(b.admitted()).unwrap(),
            "address=203.0.113.7\nat=200\n"
        );

        std::fs::write(b.source(), "address=198.51.100.9\n").unwrap();
        b.admit("300");
        assert_eq!(
            b.ufw_calls(),
            "prepend allow proto tcp from 203.0.113.7 to any port 22\n",
            "a second write is ignored"
        );
        assert_eq!(
            std::fs::read_to_string(b.admitted()).unwrap(),
            "address=203.0.113.7\nat=200\n"
        );
    }

    /// IPv6 is admitted the same way (`prepend` lands in the v6 chain).
    #[cfg(unix)]
    #[test]
    fn test_door_pass_admits_ipv6() {
        let b = DoorBox::new("v6", true);
        std::fs::write(b.source(), "address=2001:db8::7\n").unwrap();
        b.admit("1");
        assert_eq!(
            b.ufw_calls(),
            "prepend allow proto tcp from 2001:db8::7 to any port 22\n"
        );
    }

    /// A door-less box (no drop-in) ignores the file the nest writes at every
    /// claim.
    #[cfg(unix)]
    #[test]
    fn test_door_pass_ignored_on_door_less_box() {
        let b = DoorBox::new("doorless", false);
        std::fs::write(b.source(), "address=203.0.113.7\n").unwrap();
        b.admit("1");
        assert_eq!(b.ufw_calls(), "");
        assert!(!b.admitted().exists());
    }

    /// A symlinked source is never followed; a malformed value never reaches
    /// ufw and shuts the door for good.
    #[cfg(unix)]
    #[test]
    fn test_door_pass_rejects_symlink_and_malformed_source() {
        let b = DoorBox::new("symlink", true);
        let target = b.dir.join("planted");
        std::fs::write(&target, "address=203.0.113.7\n").unwrap();
        std::os::unix::fs::symlink(&target, b.source()).unwrap();
        b.admit("1");
        assert_eq!(b.ufw_calls(), "", "a symlink is never followed");
        assert!(!b.admitted().exists());

        let b = DoorBox::new("malformed", true);
        std::fs::write(b.source(), "address=1.2.3.4; rm -rf /\n").unwrap();
        b.admit("5");
        assert_eq!(b.ufw_calls(), "", "a malformed value never reaches ufw");
        assert_eq!(
            std::fs::read_to_string(b.admitted()).unwrap(),
            "refused=malformed\nat=5\n"
        );
        std::fs::write(b.source(), "address=203.0.113.7\n").unwrap();
        b.admit("6");
        assert_eq!(b.ufw_calls(), "", "the refusal is final");
    }

    /// A failing ufw leaves the admission unrecorded, so the next pass retries.
    #[cfg(unix)]
    #[test]
    fn test_door_pass_retries_after_ufw_failure() {
        use std::os::unix::fs::PermissionsExt;
        let b = DoorBox::new("ufwfail", true);
        let ufw = b.dir.join("bin/ufw");
        std::fs::write(&ufw, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&ufw, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(b.source(), "address=203.0.113.7\n").unwrap();
        let (ok, _) = run_door_fns(&format!(
            "PATH='{bin}':$PATH\nfauna_ssh_door_admit '{src}' '{dropin}' '{adm}' 1",
            bin = b.dir.join("bin").display(),
            src = b.source().display(),
            dropin = b.dir.join("0-fauna-door.conf").display(),
            adm = b.admitted().display(),
        ));
        assert!(!ok, "a failing ufw fails the pass");
        assert!(!b.admitted().exists(), "nothing recorded, so it retries");
    }
}
