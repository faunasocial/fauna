//! Host-OS maintenance: the nest → host **readiness** channel.
//!
//! On an onboarded VPS the host runs a `fauna-reboot-coordinator` systemd timer
//! (baked into cloud-init by `libs/fauna-provisioning`) that reboots the box for
//! kernel / `glibc` / `systemd` security updates **only when the nest is idle**
//! (or a 24 h hard ceiling elapses). The coordinator runs on the host and cannot
//! read `connection_count()` from inside the container, so the nest writes a
//! small flat `key=value` readiness file into the `/data/maintenance` bind mount
//! (`/opt/fauna/maintenance` on the host) that the dependency-free coordinator
//! shell `grep`s.
//!
//! **Channel split by trust direction (HM-1/HM-2 fix).** The host coordinator
//! runs as **root**; the realistic adversary host-patching defends against is a
//! **compromised nest container** (uid 1000). So a root process must never read
//! or write *trusted* state in a container-owned dir. The nest→host direction
//! (the container-writable `nest-readiness` + `restart-requested`) lives in the
//! rw uid-1000 `/data/maintenance` mount, which the coordinator reads
//! defensively; the host→nest direction (`host-status`, plus the coordinator's
//! un-forgeable ceiling clock) lives in a **root-owned** dir bind-mounted
//! **`:ro`** at `/data/maintenance-host`, which the nest only reads.
//!
//! Authority: `docs/goal/architecture/installers/vps.md` § Host OS Maintenance.
//!
//! **None-gated:** a dev / desktop / bare-metal nest has no `/data/maintenance`
//! mount, so the writer no-ops with zero behavior change (the same shape as the
//! internal-loopback listener: present only when its artifact-set wiring is).
//!
//! Idle is `connection_count() == 0`: a request can only be in flight over a
//! live connection, so no connections ⇒ nothing in flight (there is no separate
//! in-flight counter — `ws.rs`). The readiness file therefore carries just the
//! connection count and the idle-since timestamp; the coordinator keys its
//! reboot decision off `connection_count == 0`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_protocol::admin::RequestHostRestartReply;
use fauna_protocol::{RpcError, encode_canonical};

use crate::bridge_routing_handlers::require_class;
use crate::rpc_router::{RpcHandler, RpcKindMeta, RpcRouterBuilder};
use crate::ws::WsState;

/// How often the readiness file is rewritten. Far finer than the host
/// coordinator's ~15 min cadence and the 24 h reboot ceiling, so 30 s is
/// plenty fresh while keeping the writes negligible.
const READINESS_TICK: Duration = Duration::from_secs(30);

/// The **read-write** bind-mount subdirectory under `<data_dir>`
/// (`/opt/fauna/maintenance` ↔ `/data/maintenance`, owned by the container's
/// uid 1000). The nest **writes** the nest→host [`READINESS_FILE`] and the admin
/// [`RESTART_REQUESTED_FILE`] here; the root host coordinator only reads them, and
/// reads them *defensively* (symlink-rejecting + value-validating). Split from
/// [`HOST_STATE_SUBDIR`] (the host→nest direction) so a compromised nest can
/// never forge the coordinator's own trusted state — a root process must never
/// read or write trusted state in a container-owned dir (`installers/vps.md`
/// § Host OS Maintenance, the HM-1/HM-2 trust split).
const MAINTENANCE_SUBDIR: &str = "maintenance";

/// The **read-only** bind-mount subdirectory under `<data_dir>`
/// (`/opt/fauna/maintenance-host` ↔ `/data/maintenance-host:ro`, owned by host
/// `root`). The host coordinator writes [`HOST_STATUS_FILE`] here and the nest
/// only **reads** it (for the `os_*` fields on `fauna.setup.status`). Because the
/// dir is root-owned and mounted `:ro`, a compromised nest (uid 1000) can neither
/// plant a symlink the root coordinator would follow (closes HM-1) nor forge the
/// host status / ceiling clock (closes HM-2).
const HOST_STATE_SUBDIR: &str = "maintenance-host";

/// Filename written into the read-write [`MAINTENANCE_SUBDIR`]. The host
/// coordinator reads it as `/opt/fauna/maintenance/nest-readiness`.
const READINESS_FILE: &str = "nest-readiness";

/// Filename the host coordinator writes into the root-owned [`HOST_STATE_SUBDIR`];
/// the nest reads it (read-only) for the `os_*` fields on `fauna.setup.status`.
/// Host side: `/opt/fauna/maintenance-host/host-status`.
const HOST_STATUS_FILE: &str = "host-status";

/// Flag the nest writes into the read-write [`MAINTENANCE_SUBDIR`] on the admin
/// `fauna.admin.request_host_restart` kind. The host `fauna-reboot-coordinator`
/// reads it as `/opt/fauna/maintenance/restart-requested` (symlink-safe), reboots
/// regardless of idle/ceiling, and consumes it so the request fires exactly once.
const RESTART_REQUESTED_FILE: &str = "restart-requested";

/// Tracks the wall-clock instant the nest last became idle (`connection_count`
/// reached 0). Cleared while any connection is live. Pure + unit-testable.
#[derive(Debug, Default)]
pub struct IdleTracker {
    idle_since: Option<i64>,
}

impl IdleTracker {
    /// Fold in the latest `connection_count` observed at wall-clock `now` (unix
    /// seconds). The first observation that finds the nest idle stamps
    /// `idle_since`; subsequent idle observations leave it unchanged (so it
    /// reflects when idleness *began*, not the last tick). Any live connection
    /// clears it. Returns the current `idle_since`.
    pub fn observe(&mut self, connection_count: usize, now: i64) -> Option<i64> {
        if connection_count == 0 {
            if self.idle_since.is_none() {
                self.idle_since = Some(now);
            }
        } else {
            self.idle_since = None;
        }
        self.idle_since
    }

    pub fn idle_since(&self) -> Option<i64> {
        self.idle_since
    }
}

/// Render the nest→host readiness file body — flat `key=value`, jq-free so the
/// base-Ubuntu coordinator shell can `grep` it. `idle_since` is emitted only
/// while idle (omitted when a connection is live).
pub fn render_readiness(connection_count: usize, idle_since: Option<i64>) -> String {
    let mut out = format!("connection_count={connection_count}\n");
    if let Some(ts) = idle_since {
        out.push_str(&format!("idle_since={ts}\n"));
    }
    out
}

fn now_unix() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

/// Atomically write the readiness `body` into `dir` (write `<file>.tmp` + rename)
/// so the host coordinator never reads a torn file.
fn write_readiness(dir: &Path, body: &str) -> std::io::Result<()> {
    let final_path = dir.join(READINESS_FILE);
    let tmp_path = dir.join(format!("{READINESS_FILE}.tmp"));
    std::fs::write(&tmp_path, body)?;
    std::fs::rename(&tmp_path, &final_path)
}

/// Background task: rewrite `<data_dir>/maintenance/nest-readiness` every
/// [`READINESS_TICK`] with the live connection count + idle timestamp, so the
/// host reboot-coordinator can tell whether rebooting would interrupt a user.
///
/// **None-gated:** if `<data_dir>/maintenance` is absent (no bind mount — dev /
/// desktop / bare-metal) the task logs once and returns, doing nothing. On a
/// provisioned VPS cloud-init creates the directory before the container starts,
/// so it is always present at boot there.
pub async fn readiness_writer_task(ws_state: Arc<WsState>, data_dir: PathBuf) {
    let maintenance_dir = data_dir.join(MAINTENANCE_SUBDIR);
    if !maintenance_dir.is_dir() {
        tracing::debug!(
            dir = %maintenance_dir.display(),
            "host-maintenance: no /data/maintenance mount; readiness writer disabled"
        );
        return;
    }
    tracing::info!(
        dir = %maintenance_dir.display(),
        "host-maintenance: writing nest readiness for the reboot coordinator"
    );
    let mut tracker = IdleTracker::default();
    let mut interval = tokio::time::interval(READINESS_TICK);
    loop {
        interval.tick().await;
        let count = ws_state.connection_count();
        let idle_since = tracker.observe(count, now_unix());
        let body = render_readiness(count, idle_since);
        if let Err(e) = write_readiness(&maintenance_dir, &body) {
            tracing::warn!("host-maintenance: failed to write nest-readiness: {e}");
        }
    }
}

// ── Host → nest status channel (read side) ───────────────────────────────────

/// Host-OS apt/reboot state, written by the host `fauna-reboot-coordinator` into
/// `host-status` and read by the nest for the `os_*` fields on
/// `fauna.setup.status`. Every field defaults to the "nothing pending" state, so
/// a missing mount/file or an older host that hasn't written it yet reports no
/// false alarm (the version-skew-safe story the wire `#[serde(default)]` gives
/// the client). Authority: `installers/vps.md` § Host OS Maintenance.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostStatus {
    /// Pending **security** updates on the host (the coordinator's `apt-check`
    /// security count).
    pub security_updates_pending: u32,
    /// Whether the host has a pending reboot (`/run/reboot-required`).
    pub reboot_pending: bool,
    /// Unix seconds the pending reboot was first observed (drives the 24 h
    /// ceiling); `None` when no reboot is pending.
    pub reboot_deferred_since: Option<i64>,
    /// Unix seconds `unattended-upgrades` last applied patches; `None` when never.
    pub last_patched_at: Option<i64>,
}

/// Parse the coordinator's flat `key=value` `host-status` body. Unknown keys are
/// ignored and a malformed value falls back to the field default — the nest
/// never trusts the file blindly; the worst case reads as "nothing pending".
/// Pure + unit-testable (no filesystem).
pub fn parse_host_status(body: &str) -> HostStatus {
    let mut s = HostStatus::default();
    for line in body.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "security_updates_pending" => s.security_updates_pending = value.parse().unwrap_or(0),
            "reboot_pending" => s.reboot_pending = value == "true",
            "reboot_deferred_since" => s.reboot_deferred_since = value.parse().ok(),
            "last_patched_at" => s.last_patched_at = value.parse().ok(),
            _ => {}
        }
    }
    s
}

/// Read `<data_dir>/maintenance-host/host-status` from the read-only root-owned
/// mount. Returns the default "nothing pending" [`HostStatus`] when the mount/file
/// is absent or unreadable (dev / desktop / bare-metal, or a host that hasn't
/// written it yet) — the same None-gate as [`readiness_writer_task`].
pub fn read_host_status(data_dir: &Path) -> HostStatus {
    let path = data_dir.join(HOST_STATE_SUBDIR).join(HOST_STATUS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(body) => parse_host_status(&body),
        Err(_) => HostStatus::default(),
    }
}

/// Resolve the data dir from the configured db path (the dir holding the SQLite
/// file) then [`read_host_status`]. An empty/in-memory db path (test /
/// `:memory:`) yields the default "nothing pending" status. The form
/// `setup_status_core` calls.
pub fn read_host_status_from_db_path(db_path: &str) -> HostStatus {
    match crate::mail_enable::data_dir_from_db_path(db_path) {
        Some(dir) => read_host_status(&dir),
        None => HostStatus::default(),
    }
}

// ── nest → host restart-now request (write side) ─────────────────────────────

/// Write the `restart-requested` flag into `<data_dir>/maintenance/` so the host
/// coordinator reboots (gracefully) on its next run, regardless of idle/ceiling.
/// Returns `Ok(false)` when there is **no** `<data_dir>/maintenance` mount (no
/// host box to reboot — dev / desktop / bare-metal), `Ok(true)` when the flag was
/// written. Idempotent: re-writing an existing flag is a no-op success (the
/// coordinator consumes it exactly once — a stray flag at worst causes one extra
/// reboot, the crash-safe direction).
pub fn request_host_restart(data_dir: &Path) -> std::io::Result<bool> {
    let maintenance_dir = data_dir.join(MAINTENANCE_SUBDIR);
    if !maintenance_dir.is_dir() {
        return Ok(false);
    }
    let path = maintenance_dir.join(RESTART_REQUESTED_FILE);
    use std::fs::OpenOptions;
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&path)?;
    Ok(true)
}

fn rpc_internal(err: impl std::fmt::Display) -> RpcError {
    crate::rpc_errors::internal(err)
}

/// `fauna.admin.request_host_restart` — the admin's "restart now" affordance.
/// Writes the `restart-requested` flag into the `/data/maintenance` bind mount;
/// the host coordinator reboots (gracefully — `systemctl reboot` → SIGTERM →
/// existing drain) on its next run and consumes the flag. **Rejected** on a nest
/// without the maintenance mount (dev / desktop / bare-metal): there is no host
/// box to reboot — a clean error, not a silent no-op (no config theatre). Admin-
/// class (`bridge_method_allowlist.rs`). Spec: `installers/vps.md` § Host OS
/// Maintenance § 4.
fn request_host_restart_handler() -> RpcHandler {
    Box::new(|state, actor_id, _payload| {
        Box::pin(async move {
            require_class(&state, &actor_id, "fauna.admin.request_host_restart").await?;
            let wrote = match crate::mail_enable::data_dir_from_db_path(&state.config.nest.db_path)
            {
                Some(dir) => request_host_restart(&dir).map_err(|e| {
                    rpc_internal(format!(
                        "write restart-requested flag in {}: {e}",
                        dir.display()
                    ))
                })?,
                None => false,
            };
            if !wrote {
                return Err(RpcError::new(
                    "fauna.host_maintenance.no_host",
                    "error.host_maintenance.no_host",
                )
                .with_details_text(
                    "this deployment has no host-OS maintenance channel; \
                     restart-now is available only on an onboarded VPS",
                ));
            }
            let _ = state
                .db
                .audit(
                    Some(actor_id.as_slice()),
                    "nest.host_restart_requested",
                    None,
                    None,
                )
                .await;
            encode_canonical(&RequestHostRestartReply {
                ok: true,
                extra: Default::default(),
            })
            .map(|v| Bytes::from(v.to_vec()))
            .map_err(|e| rpc_internal(format!("encode reply: {e}")))
        })
    })
}

/// Register the host-OS-maintenance admin kinds (currently just
/// `fauna.admin.request_host_restart`). Admin-class, `@5 s`,
/// `forbid_replay = false` (re-requesting is idempotent — the flag write is, and
/// the coordinator consumes the flag exactly once). Kind registry twin:
/// `kind.rs::register_admin_kinds`.
pub fn register_host_maintenance_handlers(b: &mut RpcRouterBuilder) {
    b.add(
        "fauna.admin.request_host_restart",
        RpcKindMeta {
            forbid_replay: false,
            default_deadline: Duration::from_secs(5),
            handler: request_host_restart_handler(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_tracker_stamps_once_then_holds() {
        let mut t = IdleTracker::default();
        // Busy → no idle stamp.
        assert_eq!(t.observe(3, 100), None);
        // First idle observation stamps the moment idleness began…
        assert_eq!(t.observe(0, 200), Some(200));
        // …and later idle ticks keep that original timestamp.
        assert_eq!(t.observe(0, 260), Some(200));
        assert_eq!(t.observe(0, 999), Some(200));
    }

    #[test]
    fn idle_tracker_clears_on_reconnect_and_restamps() {
        let mut t = IdleTracker::default();
        assert_eq!(t.observe(0, 50), Some(50));
        // A new connection clears idleness.
        assert_eq!(t.observe(1, 60), None);
        assert_eq!(t.idle_since(), None);
        // Going idle again stamps the *new* idle-since.
        assert_eq!(t.observe(0, 70), Some(70));
    }

    #[test]
    fn render_busy_omits_idle_since() {
        let body = render_readiness(2, None);
        assert_eq!(body, "connection_count=2\n");
        assert!(!body.contains("idle_since"));
    }

    #[test]
    fn render_idle_includes_idle_since() {
        let body = render_readiness(0, Some(1_719_560_000));
        assert!(body.contains("connection_count=0\n"));
        assert!(body.contains("idle_since=1719560000\n"));
    }

    #[test]
    fn write_readiness_is_atomic_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        write_readiness(dir.path(), "connection_count=0\nidle_since=42\n").unwrap();
        let read = std::fs::read_to_string(dir.path().join(READINESS_FILE)).unwrap();
        assert_eq!(read, "connection_count=0\nidle_since=42\n");
        // No leftover temp file.
        assert!(!dir.path().join("nest-readiness.tmp").exists());
    }

    #[test]
    fn parse_host_status_full_body() {
        let s = parse_host_status(
            "security_updates_pending=4\n\
             reboot_pending=true\n\
             reboot_deferred_since=1719500000\n\
             last_patched_at=1719400000\n",
        );
        assert_eq!(
            s,
            HostStatus {
                security_updates_pending: 4,
                reboot_pending: true,
                reboot_deferred_since: Some(1_719_500_000),
                last_patched_at: Some(1_719_400_000),
            }
        );
    }

    #[test]
    fn parse_host_status_empty_optionals_and_no_reboot() {
        // The coordinator writes empty values for the unset optionals and
        // `reboot_pending=false` when nothing is pending — they must read as the
        // "nothing pending" defaults, never as a false alarm.
        let s = parse_host_status(
            "security_updates_pending=0\n\
             reboot_pending=false\n\
             reboot_deferred_since=\n\
             last_patched_at=\n",
        );
        assert_eq!(s, HostStatus::default());
    }

    #[test]
    fn parse_host_status_ignores_unknown_and_malformed() {
        // Unknown keys ignored; a non-numeric count falls back to 0; a line with
        // no `=` is skipped. Worst case is always "nothing pending".
        let s = parse_host_status(
            "security_updates_pending=notanumber\n\
             future_key=whatever\n\
             a line with no equals\n\
             reboot_pending=maybe\n",
        );
        assert_eq!(s, HostStatus::default());
    }

    #[test]
    fn read_host_status_absent_mount_is_default() {
        // No maintenance dir at all (dev / desktop / bare-metal) → default.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_host_status(dir.path()), HostStatus::default());
    }

    #[test]
    fn read_host_status_reads_file() {
        // host-status lives in the root-owned `:ro` host-state dir, NOT the rw
        // `maintenance` dir (the HM-1/HM-2 trust split).
        let dir = tempfile::tempdir().unwrap();
        let host_state = dir.path().join(HOST_STATE_SUBDIR);
        std::fs::create_dir(&host_state).unwrap();
        std::fs::write(
            host_state.join(HOST_STATUS_FILE),
            "security_updates_pending=2\nreboot_pending=true\n",
        )
        .unwrap();
        let s = read_host_status(dir.path());
        assert_eq!(s.security_updates_pending, 2);
        assert!(s.reboot_pending);
    }

    #[test]
    fn read_host_status_ignores_rw_maintenance_dir() {
        // A compromised nest owns the rw `maintenance` dir; a host-status it
        // plants there must NOT be trusted — the nest only reads host-status from
        // the root-owned `:ro` host-state dir.
        let dir = tempfile::tempdir().unwrap();
        let rw = dir.path().join(MAINTENANCE_SUBDIR);
        std::fs::create_dir(&rw).unwrap();
        std::fs::write(
            rw.join(HOST_STATUS_FILE),
            "security_updates_pending=999\nreboot_pending=true\n",
        )
        .unwrap();
        // No host-state dir → default "nothing pending", forged file ignored.
        assert_eq!(read_host_status(dir.path()), HostStatus::default());
    }

    #[test]
    fn request_host_restart_no_mount_returns_false() {
        // No maintenance dir → no host to reboot → Ok(false), no file written.
        let dir = tempfile::tempdir().unwrap();
        assert!(!request_host_restart(dir.path()).unwrap());
        assert!(!dir.path().join(MAINTENANCE_SUBDIR).exists());
    }

    #[test]
    fn request_host_restart_writes_flag_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(MAINTENANCE_SUBDIR)).unwrap();
        let flag = dir
            .path()
            .join(MAINTENANCE_SUBDIR)
            .join(RESTART_REQUESTED_FILE);
        assert!(request_host_restart(dir.path()).unwrap());
        assert!(flag.exists());
        // Re-requesting while the flag is still present is a no-op success.
        assert!(request_host_restart(dir.path()).unwrap());
        assert!(flag.exists());
    }
}
