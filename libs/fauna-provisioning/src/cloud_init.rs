use serde::{Deserialize, Serialize};

/// Which builds a provisioned box's automatic updater follows — the
/// `vps-config-update-channel-row` choice on `vps_config`
/// (`docs/goal/behavior/onboarding-provisioning.md` § 5). [`Self::image_tag`]
/// is the ONE place a channel becomes an image tag; which pipeline moves each
/// tag is `docs/goal/architecture/build-system.md` § Image tags & channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum UpdateChannel {
    /// Released builds. The default.
    #[default]
    Stable,
    /// Release candidates under verification.
    Test,
    /// The newest development builds, not yet verified.
    Dev,
}

impl UpdateChannel {
    /// Every channel, in the order the page lists them.
    pub const ALL: [UpdateChannel; 3] = [Self::Stable, Self::Test, Self::Dev];

    /// The stable id the UI keys its rows by
    /// (`vps-config-update-channel-row[<id>]`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Test => "test",
            Self::Dev => "dev",
        }
    }

    /// The channel with this [`Self::id`], if any.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }

    /// The image tag the box pulls and its updater polls.
    pub fn image_tag(self) -> &'static str {
        match self {
            Self::Stable => "latest",
            Self::Test => "test",
            Self::Dev => "dev",
        }
    }
}

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
    pub watchtower_poll: u32,
    pub claim_code: String,
    /// Whether this box provisions the **mail subsystem**. Decided at
    /// onboarding (`vps_config`), seeded from the handle's real-domain default
    /// and overridable by the `vps-config-mail-mode-toggle` (mail vs. social).
    ///
    /// - `true` → the compose carries the mail ports `{25,465,587,993}`, the
    ///   content-scan env `FAUNA_CLAMD_ADDR`/`FAUNA_RSPAMD_URL`, the `clamd` +
    ///   `rspamd` sidecars, and the matching `ufw` rules.
    /// - `false` → a **lean social-only box** (nest + watchtower only): no mail
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
// Watchtower's job) with no human ever logging in, so the whole policy is
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

# When sourced for unit testing (FAUNA_COORDINATOR_TEST=1) expose only the pure
# helper functions (fauna_reboot_decision, fauna_read_connection_count) — do not
# gather host state or reboot.
if [ "${FAUNA_COORDINATOR_TEST:-}" != 1 ]; then
    fauna_coordinator_main
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

/// The host-maintenance `runcmd` lines, injected after `ufw --force enable` and
/// before `docker compose up` so both bind-mount sources exist before the nest
/// comes up. Two dirs, split by trust direction (installers/vps.md § Host OS
/// Maintenance): `/opt/fauna/maintenance` is `chown`ed to the container's `fauna`
/// uid 1000 (the nest writes nest-readiness / restart-requested there);
/// `/opt/fauna/maintenance-host` is left **root-owned `0755`** (only root writes
/// host-status there; exported `:ro` to the container) so a compromised nest can
/// neither symlink-plant nor forge the host status. The coordinator timer is
/// enabled last.
const MAINTENANCE_RUNCMD: &str = "\n  - mkdir -p /opt/fauna/maintenance\n  - chown 1000:1000 /opt/fauna/maintenance\n  - mkdir -p /opt/fauna/maintenance-host\n  - chmod 0755 /opt/fauna/maintenance-host\n  - systemctl daemon-reload\n  - systemctl enable --now fauna-reboot-coordinator.timer";

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
    s.push_str(&write_file_entry(
        "/usr/local/sbin/fauna-reboot-coordinator",
        REBOOT_COORDINATOR_SCRIPT,
        "0755",
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
    s
}

/// Generate a cloud-init YAML payload for a fauna-nest VPS.
///
/// The **mail subsystem** is provisioned only when `params.enable_mail` — the
/// mail ports, the content-scan env, the `clamd`/`rspamd` sidecars, their `configs:` block, and the matching `ufw`
/// rules are all gated on it. A social-only box (`enable_mail == false`) gets a
/// lean nest+watchtower compose with no ~1.5 GB `clamd`, keeping the 1 GB VPS
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
    format!(
        r#"#cloud-config
package_update: true
packages:
  - docker.io
  # docker.io is the engine only; the `docker compose` v2 plugin ships separately.
  # List it explicitly so runcmd's `docker compose up` never depends on an implicit
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
            FAUNA_MODE: public{mail_env}{seed_env}
          labels:
            - "com.centurylinklabs.watchtower.enable=true"
          logging: *default-logging
          # 15s > the nest's WS 1001 drain (GRACEFUL_SHUTDOWN_TIMEOUT 7s) + DB
          # flush, so a Watchtower redeploy never SIGKILLs the nest mid-shutdown
          # (mirrors docker-compose.yml).
          stop_grace_period: 15s
          restart: unless-stopped
        watchtower:
          # Maintained fork of containrrr/watchtower (unmaintained; its bundled
          # Docker client speaks API v1.25, which Docker 24+ rejects -> silent
          # auto-update failure — mirrors docker-compose.yml).
          image: nickfedor/watchtower:latest@sha256:3b8d2e3f0f6ff9295a5d634e4fdb7062e5e6602f71d89baa15cd4d39d22b5743
          volumes:
            - /var/run/docker.sock:/var/run/docker.sock
          environment:
            WATCHTOWER_CLEANUP: "true"
            WATCHTOWER_POLL_INTERVAL: "{watchtower_poll}"
            WATCHTOWER_LABEL_ENABLE: "true"
            WATCHTOWER_ROLLING_RESTART: "true"
          logging: *default-logging
          restart: unless-stopped{mail_services}
{mail_configs}      volumes:
        fauna-data:
    permissions: '0600'
  # No /opt/fauna/.env: the compose above carries literal values (domain, image
  # tag, poll), so an .env would be inert (the compose has no ${{...}} refs).
  # Switch the image tag by editing the compose `image:` line.
  - path: /opt/fauna/claim-code
    content: "{claim_code}"
    permissions: '0600'
{maintenance_write_files}
runcmd:
  - systemctl enable docker
  - systemctl start docker
  - ufw allow 22/tcp
  - ufw allow 80/tcp
  - ufw allow 443/tcp
  - ufw allow 7842/udp{mail_ufw}
  - ufw --force enable{maintenance_runcmd}
  - docker compose -f /opt/fauna/docker-compose.yml up -d
"#,
        image_tag = params.image_tag,
        watchtower_poll = params.watchtower_poll,
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The channel → tag mapping is the contract between the wizard's choice
    /// and the pipelines that move the tags; `stable` must stay `latest`, the
    /// tag every box provisioned before the choice existed already follows.
    #[test]
    fn update_channel_ids_and_tags() {
        let table: Vec<_> = UpdateChannel::ALL
            .into_iter()
            .map(|c| (c.id(), c.image_tag()))
            .collect();
        assert_eq!(
            table,
            [("stable", "latest"), ("test", "test"), ("dev", "dev")]
        );
        assert_eq!(UpdateChannel::default(), UpdateChannel::Stable);
        for c in UpdateChannel::ALL {
            assert_eq!(UpdateChannel::from_id(c.id()), Some(c));
        }
        assert_eq!(UpdateChannel::from_id("latest"), None);
        assert_eq!(
            serde_json::to_string(&UpdateChannel::Dev).unwrap(),
            "\"dev\""
        );
    }

    /// The box boots DOMAINLESS: the rendered compose must NOT inject
    /// `FAUNA_DOMAIN`. The nest learns its domain from the admin's claim handle
    /// (the primary `mail_domains` row IS the deployment identity;
    /// `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Env contract).
    #[test]
    fn test_cloud_init_is_domainless() {
        let params = CloudInitParams {
            domain: "test.example.com".into(),
            image_tag: "latest".into(),
            watchtower_poll: 300,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
        };
        let output = build_cloud_init(&params);
        assert!(output.starts_with("#cloud-config"));
        assert!(
            !output.contains("FAUNA_DOMAIN"),
            "cloud-init must boot domainless (no FAUNA_DOMAIN env): {output}"
        );
        assert!(output.contains("ghcr.io/faunasocial/nest:latest"));
        assert!(output.contains("WATCHTOWER_POLL_INTERVAL: \"300\""));
    }

    #[test]
    fn test_cloud_init_has_required_ports() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
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
            "22/tcp", "80/tcp", "443/tcp", "7842/udp", "25/tcp", "465/tcp", "587/tcp", "993/tcp",
        ] {
            assert!(
                output.contains(&format!("ufw allow {port}")),
                "missing ufw rule {port}"
            );
        }
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
                watchtower_poll: 300,
                claim_code: "AABBCC".into(),
                enable_mail,
                deployment_seed: None,
            };
            let output = build_cloud_init(&params);
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
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
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
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
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
            watchtower_poll: 300,
            claim_code: "A1B2C3".into(),
            enable_mail: true,
            deployment_seed: None,
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
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: Some(seed.clone()),
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

    /// DKIM keys are nest-minted (when the mail domain is added, and at boot)
    /// and published post-boot from the nest's DKIM selector list; cloud-init
    /// must NOT embed a client-generated key or it would publish a key the
    /// nest never signs with.
    #[test]
    fn test_cloud_init_embeds_no_dkim_key() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
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
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
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
    /// nest+watchtower compose: NO mail ports, NO content-scan env, NO ~1.5 GB
    /// clamd/rspamd sidecars, and NO mail `ufw` rules — keeping the 1 GB VPS tier viable
    /// (`installers/vps.md` § Minimum VPS Requirements). The base nest surface
    /// (HTTP/HTTPS ports, volume, watchtower) is unchanged.
    #[test]
    fn test_cloud_init_social_only_omits_mail() {
        let params = CloudInitParams {
            domain: "social.example.com".into(),
            image_tag: "dev".into(),
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
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
            "nickfedor/watchtower:latest",
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
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
        });
        assert!(
            output.contains("        fauna-data:\n    permissions: '0600'"),
            "docker-compose.yml must be written 0600 (it can hold the deployment seed)"
        );
    }

    #[test]
    fn test_cloud_init_has_unattended_upgrades() {
        let params = CloudInitParams {
            domain: "x.com".into(),
            image_tag: "dev".into(),
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: true,
            deployment_seed: None,
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
            watchtower_poll: 60,
            claim_code: "AABBCC".into(),
            enable_mail: false,
            deployment_seed: None,
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
            watchtower_poll: 300,
            claim_code: "CLAIMCODE".into(),
            enable_mail: true,
            deployment_seed: None,
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
}
