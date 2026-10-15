//! Landlock filesystem sandbox — per-profile path restrictions.
//!
//! Each profile specifies which filesystem paths a sidecar may access.
//! Everything not explicitly allowed is denied. On kernels that don't
//! support Landlock, we log a warning and continue unsandboxed.

use crate::Profile;
use anyhow::{Context, Result, bail};
use landlock::{
    ABI, Access, AccessFs, BitFlags, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreated,
    RulesetCreatedAttr, RulesetStatus,
};
use std::path::Path;

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
///
/// `job_dir` is the transcode profile's per-job directory (`--job-dir`; `main`
/// has already refused it for every other profile).
pub fn apply(profile: Profile, job_dir: Option<&Path>) -> Result<&'static str> {
    let ruleset = build(profile, job_dir)?;
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

/// Build the profile's ruleset without applying it — `apply`'s first half,
/// split out so a test can build it before a fork and restrict only the child.
fn build(profile: Profile, job_dir: Option<&Path>) -> Result<RulesetCreated> {
    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(LANDLOCK_ABI))
        .context("handle_access")?
        .create()
        .context("create ruleset")?;

    // Common read-only paths every wrapped binary needs to function:
    // - /usr, /lib: shared libraries and binaries
    // - /etc: ld.so.cache, localtime, ssl certs
    // - /dev: null, urandom
    // - /proc: process info (e.g. /proc/self/fd — read by `close_excess_fds`
    //   after this ruleset applies; another process's /proc entries stay
    //   behind Landlock's own ptrace restriction)
    // - /tmp: the sidecars' scratch. NOT for transcode: its one scratch is the
    //   job directory, and a parser over user bytes has no business reading
    //   whatever else the box keeps in /tmp.
    let ro = AccessFs::from_read(LANDLOCK_ABI);
    for path in ["/usr", "/lib", "/lib64", "/proc", "/dev", "/etc"] {
        ruleset = add_rule_if_exists(ruleset, path, ro);
    }
    if profile != Profile::Transcode {
        ruleset = add_rule_if_exists(ruleset, "/tmp", ro);
    }

    // Per-profile rules
    match profile {
        Profile::Bridge => apply_bridge(ruleset),
        Profile::BridgeImap => apply_bridge_imap(ruleset),
        Profile::Dns => apply_dns(ruleset),
        Profile::Transcode => {
            let Some(job_dir) = job_dir else {
                bail!("the transcode profile needs its job directory");
            };
            apply_transcode(ruleset, job_dir)
        }
    }
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

/// The nest's ffmpeg transcode: RW the one per-job directory the nest created
/// for this invocation (the input is linked in, the segments are written there)
/// and nothing else under `/data` — `nest.db`, `blobs`, `acme` and `keys` are
/// kernel-denied to a decoder bug, though ffmpeg runs as the nest's own uid
/// (`deployment-threat-model.md` § Concern 1b, item 2). The ATProto bridge's
/// mp4 remux runs under this same profile, its temp directory the job
/// directory, so its PDS key under `/data/keys/atproto` is denied the same way.
///
/// Unlike the fixed `/data` paths of the other profiles, a missing job
/// directory is an error rather than a silent skip: the nest creates it just
/// before the spawn, so its absence means a broken caller, and the transcode
/// would only fail later and less legibly.
fn apply_transcode(ruleset: RulesetCreated, job_dir: &Path) -> Result<RulesetCreated> {
    let fd = PathFd::new(job_dir)
        .with_context(|| format!("open job directory {}", job_dir.display()))?;
    ruleset
        .add_rule(PathBeneath::new(fd, AccessFs::from_all(LANDLOCK_ABI)))
        .with_context(|| format!("grant job directory {}", job_dir.display()))
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

    /// The transcode profile, built exactly as `apply` builds it: the child
    /// writes inside its job directory and is refused a file outside it — the
    /// stand-in for `/data/nest.db` — and refused `/tmp`, which every other
    /// profile may read.
    #[test]
    fn test_transcode_profile_grants_only_the_job_dir() {
        let job_dir = tempfile::tempdir().unwrap();
        let secrets = tempfile::tempdir().unwrap();
        let secret_file = secrets.path().join("nest.db");
        std::fs::write(&secret_file, b"SQLite format 3").unwrap();

        let ruleset = build(Profile::Transcode, Some(job_dir.path())).unwrap();
        let output_c = CString::new(job_dir.path().join("seg000.ts").to_str().unwrap()).unwrap();
        let secret_c = CString::new(secret_file.to_str().unwrap()).unwrap();
        let tmp_c = CString::new("/tmp").unwrap();

        let verdict = run_bounded(move || {
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
            let Ok(status) = ruleset.restrict_self() else {
                return RESTRICT_FAILED;
            };
            if matches!(status.ruleset, RulesetStatus::NotEnforced) {
                return NOT_ENFORCED;
            }
            // SAFETY: `output_c` is a live NUL-terminated C string.
            let fd = unsafe {
                libc::open(
                    output_c.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                    0o600,
                )
            };
            if fd < 0 {
                return ALLOWED_PATH_REFUSED;
            }
            unsafe { libc::close(fd) };
            for denied in [&secret_c, &tmp_c] {
                let r = open_ro(denied);
                if r >= 0 {
                    return BLOCKED_PATH_OPENED;
                }
                if -r != libc::EACCES && -r != libc::EPERM {
                    return BLOCKED_PATH_WRONG_ERRNO;
                }
            }
            OK
        });

        match verdict {
            ForkProbe::Exited(OK) => {}
            ForkProbe::Exited(NOT_ENFORCED) => {
                eprintln!("landlock not enforced by this kernel — nothing to assert");
            }
            ForkProbe::Exited(RESTRICT_FAILED) => panic!("restrict_self() failed in the child"),
            ForkProbe::Exited(ALLOWED_PATH_REFUSED) => {
                panic!("the transcode profile refused a write inside its own job directory")
            }
            ForkProbe::Exited(BLOCKED_PATH_OPENED) => {
                panic!("the transcode profile opened a path outside its job directory")
            }
            ForkProbe::Exited(BLOCKED_PATH_WRONG_ERRNO) => {
                panic!("a path outside the job directory was refused, but not with EACCES/EPERM")
            }
            ForkProbe::TimedOut => panic!(
                "the sandboxed child never exited — the fork-in-a-threaded-runner \
                 deadlock `fork_probe` documents; move what the child allocates \
                 before the fork"
            ),
            other => panic!("unexpected child outcome {other:?}"),
        }
    }

    #[test]
    fn test_transcode_profile_requires_its_job_dir() {
        assert!(build(Profile::Transcode, None).is_err());
        let missing = tempfile::tempdir().unwrap().path().join("gone");
        assert!(build(Profile::Transcode, Some(&missing)).is_err());
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
