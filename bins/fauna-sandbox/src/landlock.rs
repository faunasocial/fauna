//! Landlock filesystem sandbox — per-profile path restrictions.
//!
//! Each profile specifies which filesystem paths a sidecar may access.
//! Everything not explicitly allowed is denied. On kernels that don't
//! support Landlock, we log a warning and continue unsandboxed.

use crate::Profile;
use anyhow::{Context, Result};
use landlock::{
    ABI, Access, AccessFs, BitFlags, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreated,
    RulesetCreatedAttr, RulesetStatus,
};

/// Minimum ABI — Linux 5.13+. We don't need V2+ features (Refer, Truncate).
const LANDLOCK_ABI: ABI = ABI::V1;

/// Apply Landlock filesystem restrictions for the given profile.
///
/// After this returns, the current process can only access paths explicitly
/// allowed by the profile. Everything else returns EACCES.
///
/// Returns the enforcement status as the stable wire token the wrapped binary
/// self-reports to nest (`fully` / `partial` / `off` — `BridgeConfinement::landlock`).
/// **The wrapper is the only party that can know this**: `RulesetStatus` is
/// visible exactly here, and Landlock has no introspection call an already-
/// restricted process could make to ask "am I in a domain, and how completely?".
/// So the status must be handed forward across `execvp` (see `main`) or it is
/// lost — which is what forced the on-box SSH pass this diagnostic replaces
/// (`security.md` § Co-resident process trust boundary).
pub fn apply(profile: Profile) -> Result<&'static str> {
    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(LANDLOCK_ABI))
        .context("handle_access")?
        .create()
        .context("create ruleset")?;

    // Common read-only paths all sidecars need to function:
    // - /usr, /lib: shared libraries and binaries
    // - /etc: ld.so.cache, localtime, ssl certs
    // - /dev: null, urandom
    // - /proc: process info (e.g. /proc/self/fd)
    let ro = AccessFs::from_read(LANDLOCK_ABI);
    for path in ["/usr", "/lib", "/lib64", "/proc", "/dev", "/etc", "/tmp"] {
        ruleset = add_rule_if_exists(ruleset, path, ro);
    }

    // Per-profile rules
    ruleset = match profile {
        Profile::Bridge => apply_bridge(ruleset)?,
        Profile::BridgeImap => apply_bridge_imap(ruleset)?,
        Profile::Dns => apply_dns(ruleset)?,
    };

    let status = ruleset.restrict_self().context("restrict_self")?;

    // The log lines are load-bearing beyond human reading: the tier_4
    // `_assert_landlock_enforced` greps them. Keep the text and the returned
    // token in step.
    let reported = match status.ruleset {
        RulesetStatus::FullyEnforced => {
            tracing::info!("landlock: fully enforced");
            crate::LANDLOCK_STATUS_FULLY
        }
        RulesetStatus::PartiallyEnforced => {
            tracing::warn!("landlock: partially enforced (kernel may not support all rules)");
            crate::LANDLOCK_STATUS_PARTIAL
        }
        RulesetStatus::NotEnforced => {
            tracing::warn!("landlock: not enforced (kernel does not support Landlock)");
            crate::LANDLOCK_STATUS_OFF
        }
    };

    Ok(reported)
}

/// MTA bridge (fauna-mail-bridge MTA role): RW only its own UID-isolated keyfile
/// dir + RO the operator-hatch. The sealed store (/data/nest.db, /data/blobs,
/// /data/acme) is deliberately NOT granted, so a MIME-parser RCE in the MTA
/// (which parses hostile inbound internet mail on :25) cannot read it even on
/// in-container privilege escalation past the fauna-mta DAC UID
/// (security.md § Co-resident process trust boundary). These are the ONLY two
/// /data paths the MTA touches (the rest is WS-RPC to nest); clamd/rspamd are
/// dialed over the network, not the filesystem.
fn apply_bridge(mut ruleset: RulesetCreated) -> Result<RulesetCreated> {
    let rw = AccessFs::from_all(LANDLOCK_ABI);
    let ro = AccessFs::from_read(LANDLOCK_ABI);

    // RW: the MTA's own key dir (LoadOrCreate + UpdateEnrollment write the
    // keyfile atomically via temp+rename here — keyfile.go).
    ruleset = add_rule_if_exists(ruleset, "/data/keys/mta", rw);
    // RO: deployment-topology operator-hatch (clamd/rspamd addrs, MX overrides,
    // bind addresses — no secret).
    ruleset = add_rule_if_exists(ruleset, "/data/operator-hatch.toml", ro);

    Ok(ruleset)
}

/// MDA bridge (fauna-mail-bridge MDA role): the hostile-MIME IMAP/CalDAV parser.
/// Same shape as the MTA — RW only its own key dir, RO the operator-hatch; the
/// sealed store is NOT granted (the whole point of slice 4: a parser RCE here
/// cannot reach nest.db/blobs/acme, kernel-enforced by Landlock rather than only
/// by the DAC UID).
fn apply_bridge_imap(mut ruleset: RulesetCreated) -> Result<RulesetCreated> {
    let rw = AccessFs::from_all(LANDLOCK_ABI);
    let ro = AccessFs::from_read(LANDLOCK_ABI);

    ruleset = add_rule_if_exists(ruleset, "/data/keys/mda", rw);
    ruleset = add_rule_if_exists(ruleset, "/data/operator-hatch.toml", ro);

    Ok(ruleset)
}

/// DNS watcher: RW /data/dns/.
fn apply_dns(mut ruleset: RulesetCreated) -> Result<RulesetCreated> {
    let rw = AccessFs::from_all(LANDLOCK_ABI);

    // RW: DNS zone data directory
    ruleset = add_rule_if_exists(ruleset, "/data/dns", rw);

    Ok(ruleset)
}

/// Add a path rule, skipping silently if the path doesn't exist.
/// Returns the ruleset (possibly updated, possibly unchanged).
fn add_rule_if_exists(
    ruleset: RulesetCreated,
    path: &str,
    access: BitFlags<AccessFs>,
) -> RulesetCreated {
    let Ok(fd) = PathFd::new(path) else {
        return ruleset;
    };
    match ruleset.add_rule(PathBeneath::new(fd, access)) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(path, "landlock: failed to add rule: {e}");
            // The error consumed ruleset, so we can't return it.
            // This shouldn't happen in practice — create a fresh one as fallback.
            // In practice add_rule only fails for invalid ABI, which we control.
            panic!("landlock add_rule failed unexpectedly for {path}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::fork_probe::{ForkProbe, run_bounded};
    use std::ffi::CString;

    /// The sandboxed child's exit codes — integers rather than assertion
    /// messages, for `fork_probe`'s reason: a panic message formats, which
    /// allocates, which is the deadlock that wedged a build slot for a night.
    const OK: i32 = 0;
    const NOT_ENFORCED: i32 = 20;
    const RESTRICT_FAILED: i32 = 21;
    const ALLOWED_PATH_REFUSED: i32 = 22;
    const BLOCKED_PATH_OPENED: i32 = 23;
    const BLOCKED_PATH_WRONG_ERRNO: i32 = 24;

    /// `open(path, O_RDONLY)` — a raw syscall, so the child neither allocates
    /// nor formats. Returns the fd, or `-errno`.
    fn open_ro(path: &CString) -> i32 {
        // SAFETY: `path` is a live NUL-terminated C string.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            return fd;
        }
        // SAFETY: `__errno_location` returns this thread's valid errno pointer.
        -unsafe { *libc::__errno_location() }
    }

    #[test]
    fn test_landlock_blocks_disallowed_path() {
        // Create a temp file before sandboxing
        let dir = tempfile::tempdir().unwrap();
        let blocked_file = dir.path().join("secret.txt");
        std::fs::write(&blocked_file, b"secret data").unwrap();

        // Create an allowed temp dir
        let allowed_dir = tempfile::tempdir().unwrap();
        let allowed_file = allowed_dir.path().join("ok.txt");
        std::fs::write(&allowed_file, b"allowed data").unwrap();

        // EVERYTHING that allocates happens on this side of the fork — the
        // ruleset, the two path strings — leaving the child `restrict_self`,
        // two `open`s and an exit code. See `fork_probe`'s module doc for why
        // that division is the whole point.
        let ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(LANDLOCK_ABI))
            .unwrap()
            .create()
            .unwrap();
        let ruleset = ruleset
            .add_rule(PathBeneath::new(
                PathFd::new(allowed_dir.path()).unwrap(),
                AccessFs::from_all(LANDLOCK_ABI),
            ))
            .unwrap();
        let ruleset = ruleset
            .add_rule(PathBeneath::new(
                PathFd::new("/proc").unwrap(),
                AccessFs::from_read(LANDLOCK_ABI),
            ))
            .unwrap();
        let allowed_c = CString::new(allowed_file.to_str().unwrap()).unwrap();
        let blocked_c = CString::new(blocked_file.to_str().unwrap()).unwrap();

        // Fork so we don't sandbox the test runner itself
        let verdict = run_bounded(move || {
            // Need no_new_privs for landlock
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
            let Ok(status) = ruleset.restrict_self() else {
                return RESTRICT_FAILED;
            };
            if matches!(status.ruleset, RulesetStatus::NotEnforced) {
                // Kernel doesn't support Landlock — the parent skips.
                return NOT_ENFORCED;
            }
            // Should succeed — allowed path
            if open_ro(&allowed_c) < 0 {
                return ALLOWED_PATH_REFUSED;
            }
            // Should fail — blocked path
            let blocked = open_ro(&blocked_c);
            if blocked >= 0 {
                return BLOCKED_PATH_OPENED;
            }
            if -blocked != libc::EACCES && -blocked != libc::EPERM {
                return BLOCKED_PATH_WRONG_ERRNO;
            }
            OK
        });

        match verdict {
            ForkProbe::Exited(OK) => {}
            // A kernel without Landlock is an environment absence, not a
            // product verdict — the same class the production `apply` logs and
            // continues on.
            ForkProbe::Exited(NOT_ENFORCED) => {
                eprintln!("landlock not enforced by this kernel — nothing to assert");
            }
            ForkProbe::Exited(RESTRICT_FAILED) => panic!("restrict_self() failed in the child"),
            ForkProbe::Exited(ALLOWED_PATH_REFUSED) => {
                panic!("the ruleset refused the path it was built to ALLOW")
            }
            ForkProbe::Exited(BLOCKED_PATH_OPENED) => {
                panic!("the sandboxed child opened a path outside every allowed rule")
            }
            ForkProbe::Exited(BLOCKED_PATH_WRONG_ERRNO) => {
                panic!("the blocked path was refused, but not with EACCES/EPERM")
            }
            ForkProbe::TimedOut => panic!(
                "the sandboxed child never exited — the \
                 fork-in-a-threaded-runner deadlock `fork_probe` documents, not \
                 a slow machine. Move whatever the child still allocates before \
                 the fork rather than re-running this into green."
            ),
            other => panic!("unexpected child outcome {other:?}"),
        }
    }
}
