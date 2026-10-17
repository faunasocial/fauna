//! Per-profile resource limits, set on the wrapper itself just before
//! `execvp` so the wrapped binary inherits them.
//!
//! Only the `transcode` profile sets any: the bridges are long-lived servers
//! whose budgets are the container's, while a transcode is one bounded job over
//! attacker-chosen bytes, where a decoder bug should cost a failed job, not the
//! box's memory, CPU or disk (`deployment-threat-model.md` § Concern 1b,
//! item 2). The values are constants, not knobs — no user or admin chooses how
//! much address space a decoder may map (`principles.md` § One configuration
//! surface, bucket 1). The wall-clock bound is the nest's
//! (`bins/fauna-nest/src/video/transcode.rs`); `RLIMIT_CPU` here is the
//! backstop that holds even for a process that escaped the nest's kill.

use crate::Profile;
use anyhow::{Result, bail};

#[cfg(target_env = "gnu")]
type Resource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type Resource = libc::c_int;

const GIB: u64 = 1024 * 1024 * 1024;

/// The transcode profile's limits — soft and hard alike, so the wrapped
/// process cannot raise them back.
///
/// - `RLIMIT_AS` 4 GiB: x264 at 720p maps well under 1 GiB; the rest is
///   headroom for glibc's per-thread malloc arenas, which reserve address
///   space they never touch.
/// - `RLIMIT_CPU` 2 h of CPU time, summed over every thread — far past any
///   upload the nest transcodes, short of a decoder spinning for ever.
/// - `RLIMIT_FSIZE` 2 GiB per file written: a segment is seconds of video.
/// - `RLIMIT_NOFILE` 256: ffmpeg holds an input, a playlist and a segment.
/// - `RLIMIT_CORE` 0: a crash must not dump decoded user content to disk.
const TRANSCODE: [(Resource, u64, &str); 5] = [
    (libc::RLIMIT_AS, 4 * GIB, "RLIMIT_AS"),
    (libc::RLIMIT_CPU, 2 * 60 * 60, "RLIMIT_CPU"),
    (libc::RLIMIT_FSIZE, 2 * GIB, "RLIMIT_FSIZE"),
    (libc::RLIMIT_NOFILE, 256, "RLIMIT_NOFILE"),
    (libc::RLIMIT_CORE, 0, "RLIMIT_CORE"),
];

fn limits(profile: Profile) -> &'static [(Resource, u64, &'static str)] {
    match profile {
        Profile::Transcode => &TRANSCODE,
        Profile::Bridge | Profile::BridgeImap | Profile::Dns => &[],
    }
}

/// Set every limit in `table`, raw syscalls only (a forked test child runs
/// this — `fork_probe`'s no-allocation discipline). Returns the index of the
/// first limit the kernel refused, with its errno.
fn set(table: &[(Resource, u64, &str)]) -> Result<(), (usize, i32)> {
    for (i, &(resource, value, _)) in table.iter().enumerate() {
        let lim = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        // SAFETY: `lim` is a valid, initialised rlimit for the call's duration.
        if unsafe { libc::setrlimit(resource, &lim) } != 0 {
            // SAFETY: `__errno_location` returns this thread's valid errno pointer.
            return Err((i, unsafe { *libc::__errno_location() }));
        }
    }
    Ok(())
}

/// Apply the profile's resource limits to this process.
pub fn apply(profile: Profile) -> Result<()> {
    let table = limits(profile);
    if let Err((i, errno)) = set(table) {
        bail!(
            "setrlimit({}) failed: {}",
            table[i].2,
            std::io::Error::from_raw_os_error(errno)
        );
    }
    if !table.is_empty() {
        tracing::info!(count = table.len(), "rlimits: applied");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fork_probe::{ForkProbe, run_bounded};

    const OK: i32 = 0;
    const SET_FAILED: i32 = 30;
    const NOT_APPLIED: i32 = 31;
    const RAISED_BACK: i32 = 32;

    #[test]
    fn test_transcode_limits_apply_and_cannot_be_raised() {
        let table = limits(Profile::Transcode);
        let verdict = run_bounded(move || {
            if set(table).is_err() {
                return SET_FAILED;
            }
            for &(resource, value, _) in table {
                let mut got = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                // SAFETY: `got` is a valid out-pointer for the call's duration.
                if unsafe { libc::getrlimit(resource, &mut got) } != 0
                    || got.rlim_cur != value as libc::rlim_t
                    || got.rlim_max != value as libc::rlim_t
                {
                    return NOT_APPLIED;
                }
            }
            // Raising the hard limit back needs CAP_SYS_RESOURCE, which an
            // unprivileged test runner (and the nest's uid) does not hold.
            let raised = libc::rlimit {
                rlim_cur: libc::RLIM_INFINITY,
                rlim_max: libc::RLIM_INFINITY,
            };
            // SAFETY: as above.
            if unsafe { libc::geteuid() } != 0
                && unsafe { libc::setrlimit(libc::RLIMIT_CPU, &raised) } == 0
            {
                return RAISED_BACK;
            }
            OK
        });
        match verdict {
            ForkProbe::Exited(OK) => {}
            ForkProbe::Exited(SET_FAILED) => panic!("setrlimit refused a transcode limit"),
            ForkProbe::Exited(NOT_APPLIED) => {
                panic!("getrlimit does not read back the transcode limits")
            }
            ForkProbe::Exited(RAISED_BACK) => {
                panic!("an unprivileged child raised its hard RLIMIT_CPU back")
            }
            other => panic!("unexpected child outcome {other:?}"),
        }
    }

    #[test]
    fn test_only_transcode_sets_limits() {
        for profile in [Profile::Bridge, Profile::BridgeImap, Profile::Dns] {
            assert!(
                limits(profile).is_empty(),
                "{profile:?} must set no rlimits"
            );
        }
        assert!(!limits(Profile::Transcode).is_empty());
    }
}
