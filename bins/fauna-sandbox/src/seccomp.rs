//! Seccomp-bpf syscall filter — restrict dangerous syscalls.
//!
//! All profiles share a base deny-list:
//! - ptrace → EPERM (prevents debugging/inspecting other processes)
//! - process_vm_readv/writev → EPERM (prevents cross-process memory access)
//! - kill/tgkill → EPERM when target pid != self (prevents killing other processes)
//! - tkill → EPERM unconditionally (use tgkill with tgid check instead)
//!
//! The filter uses a deny-list model: unlisted syscalls are allowed.

use crate::Profile;
use anyhow::{Context, Result};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};
use std::collections::BTreeMap;

/// Apply seccomp syscall restrictions for the given profile.
pub fn apply(profile: Profile) -> Result<()> {
    let filter = build_filter(profile).context("build seccomp filter")?;
    seccompiler::apply_filter(&filter).context("apply seccomp filter")?;
    tracing::info!("seccomp: filter applied");
    Ok(())
}

/// Build the BPF program for the given profile.
fn build_filter(profile: Profile) -> Result<BpfProgram> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // --- Base rules (all profiles) ---

    // Unconditionally deny ptrace
    rules.insert(libc::SYS_ptrace, vec![unconditional_match()?]);

    // Unconditionally deny cross-process memory access
    rules.insert(libc::SYS_process_vm_readv, vec![unconditional_match()?]);
    rules.insert(libc::SYS_process_vm_writev, vec![unconditional_match()?]);

    // Deny kill(pid, sig) when pid != getpid()
    // kill arg0 = target pid
    let self_pid = std::process::id() as u64;
    rules.insert(
        libc::SYS_kill,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0, // arg0 = pid
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            self_pid,
        )?])?],
    );

    // Deny tgkill(tgid, tid, sig) when tgid != getpid()
    // tgkill arg0 = thread group ID (== pid)
    rules.insert(
        libc::SYS_tgkill,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0, // arg0 = tgid
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            self_pid,
        )?])?],
    );

    // Unconditionally deny tkill (deprecated — use tgkill)
    rules.insert(libc::SYS_tkill, vec![unconditional_match()?]);

    // --- Profile-specific rules ---
    match profile {
        Profile::Bridge | Profile::BridgeImap | Profile::Dns => {
            // Base deny-list is sufficient for these profiles
        }
    }

    // Build filter: deny-list model
    // - match_action = Errno(EPERM): when a rule matches, deny the syscall
    // - mismatch_action = Allow: unlisted syscalls (and unmatched conditions) pass through
    let arch = std::env::consts::ARCH
        .try_into()
        .context("unsupported architecture for seccomp")?;

    let filter = SeccompFilter::new(rules, SeccompAction::Allow, SeccompAction::Errno(1), arch)
        .context("SeccompFilter::new")?;

    let bpf: BpfProgram = filter.try_into().context("compile BPF program")?;
    Ok(bpf)
}

/// Create a rule that always matches.
/// Uses a tautological condition: arg0 (unsigned) >= 0 is always true.
fn unconditional_match() -> Result<SeccompRule> {
    Ok(SeccompRule::new(vec![SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Ge,
        0,
    )?])?)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::fork_probe::{ForkProbe, run_bounded};

    /// Exit codes the sandboxed children below carry. Integers rather than
    /// assertion messages on purpose: the child runs in a process that
    /// inherited libtest's locked mutexes, where formatting a panic message can
    /// deadlock — `fork_probe`'s module doc carries the 20-hour measurement.
    /// The parent turns a code into the sentence.
    const OK: i32 = 0;
    const APPLY_FAILED: i32 = 10;
    const SYSCALL_NOT_REFUSED: i32 = 11;
    const WRONG_ERRNO: i32 = 12;
    const SELF_CALL_REFUSED: i32 = 13;

    /// `errno` without `std::io::Error::last_os_error()`, which allocates on
    /// the way to the `raw_os_error()` the child only compares.
    fn errno() -> i32 {
        // SAFETY: `__errno_location` returns this thread's valid errno pointer.
        unsafe { *libc::__errno_location() }
    }

    fn expect_ok(verdict: ForkProbe, what: &str) {
        match verdict {
            ForkProbe::Exited(OK) => {}
            ForkProbe::Exited(APPLY_FAILED) => {
                panic!("{what}: the sandboxed child could not apply the filter")
            }
            ForkProbe::Exited(SYSCALL_NOT_REFUSED) => {
                panic!("{what}: the syscall SUCCEEDED under a filter that must refuse it")
            }
            ForkProbe::Exited(WRONG_ERRNO) => {
                panic!("{what}: the syscall was refused, but not with EPERM")
            }
            ForkProbe::Exited(SELF_CALL_REFUSED) => {
                panic!("{what}: the filter refused a call the profile must ALLOW")
            }
            ForkProbe::TimedOut => panic!(
                "{what}: the sandboxed child never exited — this is the \
                 fork-in-a-threaded-runner deadlock `fork_probe` documents, not a \
                 slow machine. Do not re-run it into green; move the work the \
                 child does before the fork."
            ),
            other => panic!("{what}: unexpected child outcome {other:?}"),
        }
    }

    /// Apply the Bridge profile's filter to THIS process, returning a child
    /// exit code.
    ///
    /// ⚠ **`build_filter` has to run here, in the child, and cannot be hoisted
    /// before the fork** the way `landlock`'s ruleset was: the filter bakes
    /// `std::process::id()` into its `kill`/`tgkill` conditions
    /// ([`build_filter`]'s `self_pid`), so one built by the parent governs the
    /// parent's pid and would refuse the child's own `kill(self, 0)` —
    /// measured, not reasoned: hoisting it turned
    /// `test_seccomp_allows_kill_self` red on the first run. So this one
    /// allocation stays on the wrong side of the fork, and what protects the
    /// build pool from it is the watchdog rather than its absence
    /// (`fork_probe`). Everything else the child does is a raw syscall.
    fn apply_bridge_filter_here() -> i32 {
        // Need no_new_privs for seccomp.
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        let Ok(filter) = build_filter(Profile::Bridge) else {
            return APPLY_FAILED;
        };
        if seccompiler::apply_filter(&filter).is_err() {
            return APPLY_FAILED;
        }
        OK
    }

    #[test]
    fn test_seccomp_blocks_ptrace() {
        // Fork so we don't apply seccomp to the test runner.
        let verdict = run_bounded(|| {
            let applied = apply_bridge_filter_here();
            if applied != OK {
                return applied;
            }
            // ptrace should be denied with EPERM.
            if unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) } != -1 {
                return SYSCALL_NOT_REFUSED;
            }
            if errno() != libc::EPERM {
                return WRONG_ERRNO;
            }
            OK
        });
        expect_ok(verdict, "ptrace under the Bridge filter");
    }

    #[test]
    fn test_seccomp_allows_kill_self() {
        // Fork so we don't apply seccomp to the test runner.
        let verdict = run_bounded(|| {
            let applied = apply_bridge_filter_here();
            if applied != OK {
                return applied;
            }
            // kill(self, 0) should succeed (signal 0 = check permission only).
            if unsafe { libc::kill(libc::getpid(), 0) } != 0 {
                return SELF_CALL_REFUSED;
            }
            // kill(1, 0) should fail with EPERM (target pid 1 != self).
            if unsafe { libc::kill(1, 0) } != -1 {
                return SYSCALL_NOT_REFUSED;
            }
            if errno() != libc::EPERM {
                return WRONG_ERRNO;
            }
            OK
        });
        expect_ok(verdict, "kill(1, 0) under the Bridge filter");
    }
}
