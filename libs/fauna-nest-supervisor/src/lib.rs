//! Platform-agnostic serving-port supervision for the `fauna-nest`
//! direct-listener deployments.
//!
//! On a **direct-listener** nest (no SNI router) the admin's client-set
//! `serving_port` overrides the bind seed's port, and the nest cannot hot-rebind
//! its own `TcpListener` — so a per-OS **supervisor** restarts the nest to apply a
//! change. The nest materializes its current serving port as a value-flag file in
//! its data dir (`<data_dir>/serving-port`, decimal text) whenever the admin calls
//! `fauna.admin.set_serving_port` (`fauna_nest::mail_enable::set_serving_port_flag`,
//! re-asserted by a reconcile tick); the supervisor reads that flag — rather than
//! re-opening `nest.db` — to detect the change. See
//! `docs/goal/architecture/nest/common.md` § Serving ports.
//!
//! This crate holds the **reconcile decision only** — the cross-platform policy.
//! The per-OS *shells* that drive it differ and live with their consumer (the
//! sibling pattern of [`fauna-mda-supervisor`](../fauna_mda_supervisor)):
//! - **Windows** — `apps/fauna-windows/fauna-nest-service` runs `start_server` in a
//!   serve-and-restart loop under the SCM, default serving port 443.
//! - **macOS** — a `LaunchDaemon`-run supervisor (`_fauna`) drives the same loop,
//!   default serving port 3000 (the per-user/desktop seed; the machine-daemon's
//!   `:443` comes from launchd socket activation).
//!
//! The default serving port is a **parameter** of every resolver here precisely
//! because it differs per deployment (Windows passes
//! `fauna_protocol::node_policy::DEFAULT_SERVING_PORT` = 443; macOS passes its
//! `3000` seed) — the policy is identical, only the seed differs.

use std::path::Path;
use std::time::Duration;

/// How often a supervisor re-reads the `<data_dir>/serving-port` flag to detect an
/// admin port change. Mirrors the MDA supervisor's 15 s reconcile cadence
/// ([`fauna_mda_supervisor`]'s `RECONCILE_POLL`); a serving-port change is a rare
/// admin action, so a coarse tick is ample and cheap.
pub const SERVING_PORT_POLL: Duration = Duration::from_secs(15);

/// Serving-port value-flag filename the nest materializes in its data dir.
///
/// This is an on-disk cross-process contract between the nest (writer) and a
/// supervisor (reader), so it is not written here: it is re-exported from
/// [`fauna_deployment_flags`], the zero-dependency crate that owns the whole flag
/// set for the nest and both supervisor shells alike. Re-exported rather than
/// merely used so this crate's own consumers — the per-OS service shells — keep
/// the name they already import.
pub use fauna_deployment_flags::SERVING_PORT_FLAG;

/// The admin-set client-facing serving port the nest has materialized in its data
/// dir (`<data_dir>/serving-port`, decimal text), or `None` when the flag is
/// absent / unparseable / `0`. The nest's own listener twin of the MDA
/// supervisor's `admin_caldav_port`. Authoritative flag name + writer:
/// [`SERVING_PORT_FLAG`] / `fauna_nest::mail_enable::set_serving_port_flag`.
pub fn serving_port_flag(data_dir: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(data_dir.join(SERVING_PORT_FLAG)).ok()?;
    fauna_deployment_flags::parse_port_flag(&raw)
}

/// The client-facing port the nest should currently bind: the admin's flag value,
/// or `default_port` (the deployment seed — 443 on Windows, 3000 on macOS desktop)
/// when the admin never picked one. `start_server` performs the actual boot-resolve
/// from the DB singleton; this mirror lets a supervisor detect a change off the
/// flag the nest writes (exactly as the MDA supervisor reads the `caldav-port`
/// flag), without re-opening the DB.
pub fn effective_serving_port(data_dir: &Path, default_port: u16) -> u16 {
    serving_port_flag(data_dir).unwrap_or(default_port)
}

/// The new port to rebind to when the admin has changed the serving port since the
/// running nest was started on `started_with`, else `None` (no restart).
/// Edge-triggered on the flag value — `started_with` is re-anchored to the flag at
/// each (re)start — so a transiently divergent flag/DB can never cause a restart
/// storm. Per `nest/common.md` § Serving ports: the nest cannot hot-rebind its own
/// `TcpListener`, so the supervisor restarts it on a change. `default_port` is the
/// deployment seed used when the flag is absent (so a flag-removal back to the
/// default from a non-default running port correctly yields a restart to the seed).
pub fn serving_port_restart_target(
    started_with: u16,
    data_dir: &Path,
    default_port: u16,
) -> Option<u16> {
    let current = effective_serving_port(data_dir, default_port);
    (current != started_with).then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // The two real deployment seeds, exercised against every resolver so the
    // policy is proven independent of which default a shell passes.
    const WINDOWS_DEFAULT: u16 = 443;
    const MACOS_DEFAULT: u16 = 3000;

    /// Create a fresh, uniquely-named temp dir for a test (no `tempfile` dep —
    /// mirrors `fauna-mda-supervisor`'s tests).
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fauna-nest-sup-{tag}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// With no `serving-port` flag in the data dir, the effective port is the
    /// caller's deployment default — the admin never picked one. Proven for both
    /// the Windows (443) and macOS (3000) seeds.
    #[test]
    fn effective_serving_port_defaults_when_flag_absent() {
        for default in [WINDOWS_DEFAULT, MACOS_DEFAULT] {
            let dir = temp_dir("eff-absent");
            assert_eq!(effective_serving_port(&dir, default), default);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An admin-set `serving-port` flag (decimal text, with trailing newline
    /// tolerated) overrides the default — regardless of which default the shell
    /// passes.
    #[test]
    fn effective_serving_port_reads_admin_flag() {
        for default in [WINDOWS_DEFAULT, MACOS_DEFAULT] {
            let dir = temp_dir("eff-set");
            std::fs::write(dir.join(SERVING_PORT_FLAG), b"8443\n").unwrap();
            assert_eq!(effective_serving_port(&dir, default), 8443);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A malformed / zero / empty / out-of-range flag is ignored — fall back to the
    /// caller's default rather than trying to bind a garbage port.
    #[test]
    fn effective_serving_port_ignores_malformed_flag() {
        for default in [WINDOWS_DEFAULT, MACOS_DEFAULT] {
            let dir = temp_dir("eff-bad");
            for bad in ["not-a-port", "0", "70000", ""] {
                std::fs::write(dir.join(SERVING_PORT_FLAG), bad).unwrap();
                assert_eq!(
                    effective_serving_port(&dir, default),
                    default,
                    "malformed flag {bad:?} must fall back to the default {default}"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An unchanged port (flag absent, nest started on the default) is no restart —
    /// for either deployment seed.
    #[test]
    fn restart_target_none_when_unchanged_at_default() {
        for default in [WINDOWS_DEFAULT, MACOS_DEFAULT] {
            let dir = temp_dir("rt-unchanged-default");
            assert_eq!(serving_port_restart_target(default, &dir, default), None);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// An unchanged port (flag matches the port the nest was started on) is no
    /// restart — the edge-trigger must not storm on a stable value.
    #[test]
    fn restart_target_none_when_flag_matches_started() {
        let dir = temp_dir("rt-unchanged-set");
        std::fs::write(dir.join(SERVING_PORT_FLAG), b"8443").unwrap();
        assert_eq!(
            serving_port_restart_target(8443, &dir, WINDOWS_DEFAULT),
            None
        );
        assert_eq!(serving_port_restart_target(8443, &dir, MACOS_DEFAULT), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The admin raising the port from the default yields the new target — the
    /// supervisor must restart the nest to rebind.
    #[test]
    fn restart_target_some_when_admin_sets_new_port() {
        let dir = temp_dir("rt-raised");
        std::fs::write(dir.join(SERVING_PORT_FLAG), b"8443").unwrap();
        assert_eq!(
            serving_port_restart_target(WINDOWS_DEFAULT, &dir, WINDOWS_DEFAULT),
            Some(8443)
        );
        assert_eq!(
            serving_port_restart_target(MACOS_DEFAULT, &dir, MACOS_DEFAULT),
            Some(8443)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The admin resetting the port back to the default (flag removed) from a
    /// non-default running port yields a restart back to that deployment's default
    /// seed — 443 on Windows, 3000 on macOS.
    #[test]
    fn restart_target_some_when_admin_resets_to_default() {
        // Flag removed entirely → effective default, differs from the running 8443.
        let dir = temp_dir("rt-reset");
        assert_eq!(
            serving_port_restart_target(8443, &dir, WINDOWS_DEFAULT),
            Some(WINDOWS_DEFAULT)
        );
        assert_eq!(
            serving_port_restart_target(8443, &dir, MACOS_DEFAULT),
            Some(MACOS_DEFAULT)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `serving_port_flag` itself: present-and-valid → Some; absent/zero/garbage →
    /// None (the default-substitution is the caller's job).
    #[test]
    fn serving_port_flag_reads_valid_and_rejects_garbage() {
        let dir = temp_dir("flag-direct");
        assert_eq!(serving_port_flag(&dir), None, "absent flag → None");
        std::fs::write(dir.join(SERVING_PORT_FLAG), b"9443\n").unwrap();
        assert_eq!(serving_port_flag(&dir), Some(9443));
        std::fs::write(dir.join(SERVING_PORT_FLAG), b"0").unwrap();
        assert_eq!(serving_port_flag(&dir), None, "zero → None");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
