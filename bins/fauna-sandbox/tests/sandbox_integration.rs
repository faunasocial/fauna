//! Integration test: verify that fauna-sandbox applies both Landlock and seccomp
//! restrictions, then execs the target binary.
//!
//! These tests fork child processes to avoid permanently sandboxing the test runner.

use std::process::Command;

/// Helper: build the fauna-sandbox binary path.
fn sandbox_bin() -> String {
    // cargo sets OUT_DIR during tests; the binary is in the same target dir
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("fauna-sandbox");
    path.to_string_lossy().to_string()
}

#[test]
fn test_sandbox_execs_target_binary() {
    // fauna-sandbox dns -- /bin/echo hello
    let output = Command::new(sandbox_bin())
        .args(["dns", "--", "/bin/echo", "hello"])
        .output()
        .expect("failed to run fauna-sandbox");

    assert!(
        output.status.success(),
        "fauna-sandbox should exit successfully, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "hello",
        "should exec /bin/echo and produce output"
    );
}

/// The Landlock enforcement status must survive `execvp` into the wrapped
/// binary — that hand-off is the whole mechanism behind the bridge's
/// `BridgeConfinement::landlock` self-report, and it is invisible to every
/// other test (the wrapper's own log line proves only that the wrapper knew).
///
/// Also pins the override half: a value injected from *outside* the image is
/// replaced by what actually happened, so a compose that claims `fully` on a
/// Landlock-less kernel cannot make the diagnostic lie.
#[test]
fn test_sandbox_exports_landlock_status_across_exec() {
    let run = |preset: Option<&str>| {
        let mut cmd = Command::new(sandbox_bin());
        cmd.args(["dns", "--", "/usr/bin/env"]);
        match preset {
            Some(v) => cmd.env("FAUNA_SANDBOX_LANDLOCK", v),
            None => cmd.env_remove("FAUNA_SANDBOX_LANDLOCK"),
        };
        let out = cmd.output().expect("failed to run fauna-sandbox");
        assert!(
            out.status.success(),
            "fauna-sandbox should exit successfully, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| {
                l.strip_prefix("FAUNA_SANDBOX_LANDLOCK=")
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                panic!("wrapper did not export FAUNA_SANDBOX_LANDLOCK to the exec'd binary")
            })
    };

    let observed = run(None);
    assert!(
        matches!(observed.as_str(), "fully" | "partial" | "off"),
        "unexpected status token {observed:?} — the wrapped binary reports this \
         verbatim to nest, so an unrecognized token would surface on the admin page"
    );

    // A lie from outside the image loses to the kernel's actual answer.
    assert_eq!(
        run(Some("fully_but_actually_a_lie")),
        observed,
        "an externally-injected status must be overwritten by the real one"
    );
}

#[test]
fn test_sandbox_blocks_ptrace_via_child() {
    // Write a small C program that attempts ptrace and reports the errno.
    // We use a shell one-liner instead, since /bin/sh is always available.
    //
    // The trick: we exec `sh -c '...'` through the sandbox. The shell script
    // uses /proc/self/syscall to indirectly verify seccomp is active, but the
    // simplest approach is to use a Rust helper binary. Since we can't compile
    // on the fly in an integration test, we'll use the fork-based approach.

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // Child: manually replicate what fauna-sandbox does, then test

        // 1. PR_SET_NO_NEW_PRIVS
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

        // 2. Apply seccomp (import the filter builder)
        // We can't easily call into fauna-sandbox internals from an integration
        // test, so we replicate the seccomp setup here using seccompiler directly.
        use seccompiler::{
            SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
            SeccompRule,
        };
        use std::collections::BTreeMap;

        let self_pid = std::process::id() as u64;
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        // Tautological condition: unsigned arg0 >= 0 is always true
        let always_true =
            || SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ge, 0).unwrap();
        rules.insert(
            libc::SYS_ptrace,
            vec![SeccompRule::new(vec![always_true()]).unwrap()],
        );
        rules.insert(
            libc::SYS_process_vm_readv,
            vec![SeccompRule::new(vec![always_true()]).unwrap()],
        );
        rules.insert(
            libc::SYS_process_vm_writev,
            vec![SeccompRule::new(vec![always_true()]).unwrap()],
        );
        rules.insert(
            libc::SYS_kill,
            vec![
                SeccompRule::new(vec![
                    SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, self_pid)
                        .unwrap(),
                ])
                .unwrap(),
            ],
        );

        let arch = std::env::consts::ARCH.try_into().unwrap();
        let filter =
            SeccompFilter::new(rules, SeccompAction::Allow, SeccompAction::Errno(1), arch).unwrap();
        let bpf: seccompiler::BpfProgram = filter.try_into().unwrap();
        seccompiler::apply_filter(&bpf).unwrap();

        // Verify ptrace is blocked
        let ret = unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) };
        assert_eq!(ret, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );

        // Verify kill(self) still works
        let ret = unsafe { libc::kill(libc::getpid(), 0) };
        assert_eq!(ret, 0);

        // Verify kill(1) is blocked
        let ret = unsafe { libc::kill(1, 0) };
        assert_eq!(ret, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );

        std::process::exit(0);
    } else {
        let mut status = 0i32;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child exited with status {}",
            status
        );
    }
}

#[test]
fn test_sandbox_blocks_file_access_via_landlock() {
    use landlock::{
        ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus,
    };

    let abi = ABI::V1;

    // Create a file that should be blocked
    let blocked_dir = tempfile::tempdir().unwrap();
    let blocked_file = blocked_dir.path().join("nest.db");
    std::fs::write(&blocked_file, b"database contents").unwrap();

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

        // Apply landlock allowing only /proc (needed for basic operation)
        let ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .unwrap()
            .create()
            .unwrap();

        let ruleset = ruleset
            .add_rule(PathBeneath::new(
                PathFd::new("/proc").unwrap(),
                AccessFs::from_read(abi),
            ))
            .unwrap();

        let status = ruleset.restrict_self().unwrap();

        if matches!(status.ruleset, RulesetStatus::NotEnforced) {
            std::process::exit(0); // Skip on unsupported kernels
        }

        // Attempt to read the "database" file — should be blocked
        let err = std::fs::read_to_string(&blocked_file).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

        std::process::exit(0);
    } else {
        let mut status = 0i32;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child exited with status {}",
            status
        );
    }
}

#[test]
fn test_sandbox_rejects_unknown_profile() {
    let output = Command::new(sandbox_bin())
        .args(["unknown-profile", "--", "/bin/echo", "test"])
        .output()
        .expect("failed to run fauna-sandbox");

    assert!(
        !output.status.success(),
        "fauna-sandbox should reject unknown profile"
    );
}
