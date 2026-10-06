//! Sandbox wrapper: apply Landlock filesystem restrictions and seccomp syscall
//! filters, then exec the target binary.
//!
//! Usage:
//!   fauna-sandbox <profile> -- <command> [args...]
//!
//! Profiles: bridge, bridge-imap, dns

#[cfg(test)]
mod fork_probe;
mod landlock;
mod seccomp;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::ffi::CString;

/// Environment variable carrying the Landlock enforcement status from this
/// wrapper to the binary it execs (`fully` / `partial` / `off`; absent = the
/// wrapper never ran). **Artifact-set IPC, not a configuration knob** — no
/// human ever writes it, it is one process telling the next what the kernel
/// just did (`principles.md` § One configuration surface, bucket 1).
///
/// The Go mirror of this name is `internal/confinement.landlockStatusEnv`;
/// a cross-language pin lives in that package's tests.
pub const LANDLOCK_STATUS_ENV: &str = "FAUNA_SANDBOX_LANDLOCK";

/// The three tokens [`LANDLOCK_STATUS_ENV`] can carry — one per
/// `RulesetStatus`, emitted by [`landlock::apply`] and switched on by the Go
/// side's `internal/confinement.landlockStatus`.
///
/// Declared as named constants rather than left as literals in `apply`'s match
/// so the Go package's `TestLandlockTokensMatchTheRustWrapper` has something
/// stable to pin against. The env-var *name* has been pinned since this
/// contract landed; the *values* were not, and that gap is the same shape the
/// WS-RPC `reply-*.cbor` fixtures have one boundary over: a rename here would
/// leave the name pin green while every deployed bridge silently reported
/// `unknown`, which then reads as a broken sandbox rather than a broken
/// contract.
///
/// `unknown` is deliberately **not** here: it is what the *reader* says when
/// the variable is absent (the wrapper never ran), never something this wrapper
/// emits.
pub const LANDLOCK_STATUS_FULLY: &str = "fully";
/// Partially enforced — an ABI-negotiation artifact on current kernels, with
/// the denials provably working. Counts as enforcing; see the Go
/// `Report.Confined`.
pub const LANDLOCK_STATUS_PARTIAL: &str = "partial";
/// Not enforced — the kernel does not support Landlock.
pub const LANDLOCK_STATUS_OFF: &str = "off";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    Bridge,
    BridgeImap,
    Dns,
}

impl Profile {
    /// Profiles whose wrapped binary binds a privileged (<1024) port and so
    /// needs `CAP_NET_BIND_SERVICE` to survive the sandbox's `no_new_privs`:
    /// the MTA binds 25/465/587, the MDA binds 993/143. The dns
    /// profile is a pure client and needs no bind capability.
    fn needs_net_bind(self) -> bool {
        matches!(self, Profile::Bridge | Profile::BridgeImap)
    }
}

#[derive(Parser)]
#[command(
    name = "fauna-sandbox",
    about = "Apply Landlock + seccomp sandbox, then exec a sidecar binary"
)]
struct Args {
    /// Sandbox profile to apply
    #[arg(value_enum)]
    profile: Profile,

    /// Command and arguments to exec (after --)
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("fauna_sandbox=info")
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    if args.command.is_empty() {
        bail!("no command specified after --");
    }

    let binary = &args.command[0];
    let argv = &args.command;

    tracing::info!(
        profile = ?args.profile,
        binary = binary.as_str(),
        "applying sandbox"
    );

    // 1. For port-binding profiles, lift CAP_NET_BIND_SERVICE into the ambient
    //    set BEFORE no_new_privs. fauna-sandbox carries the cap as a file
    //    capability (setcap +ep in the image); no_new_privs (set next) drops the
    //    *bridge* binary's own file-cap at execve, and ambient caps are the only
    //    caps that survive no_new_privs across execve — so this is how the
    //    sandboxed bridge keeps the ability to bind 993/143 (MDA) / 25/465/587
    //    (MTA). The client-only profile (dns) skips this (no privileged bind).
    if args.profile.needs_net_bind() {
        raise_net_bind_ambient().context("raise CAP_NET_BIND_SERVICE into ambient set")?;
    }

    // 2. PR_SET_NO_NEW_PRIVS — prevent setuid escalation
    set_no_new_privs().context("PR_SET_NO_NEW_PRIVS")?;

    // 3. Apply Landlock filesystem restrictions
    let landlock_status = landlock::apply(args.profile).context("landlock")?;

    // 3b. Hand the enforcement status forward to the wrapped binary, which
    //     self-reports it to nest as a provisioning diagnostic
    //     (`BridgeConfinement::landlock`; security.md § Co-resident process
    //     trust boundary → Confinement self-probe). Only we can know it —
    //     `RulesetStatus` exists exactly at the `restrict_self` above and a
    //     restricted process has no way to ask the kernel about its own domain
    //     — so without this hand-off the fact is provable only over SSH, which
    //     a provisioned box has no key for (testing.md § Gap 3).
    //
    //     Set UNCONDITIONALLY, so a value injected from outside the image (a
    //     compose `environment:` claiming `fully`) is always overwritten by what
    //     actually happened whenever the sandbox really runs. A launch that
    //     bypasses the wrapper leaves the variable absent, which the wrapped
    //     binary reports as `unknown` rather than guessing.
    //
    //     SAFETY: `set_var` requires no other thread be reading the environment
    //     concurrently. We are still single-threaded here — nothing between
    //     `main`'s entry and this line spawns a thread (the tracing subscriber
    //     and clap parse do not) — and the very next steps are FD cleanup and
    //     `execvp`.
    unsafe { std::env::set_var(LANDLOCK_STATUS_ENV, landlock_status) };

    // 4. Apply seccomp syscall filter
    seccomp::apply(args.profile).context("seccomp")?;

    // 5. Close all FDs > 2
    close_excess_fds();

    // 6. Exec the target binary
    let c_binary = CString::new(binary.as_bytes()).context("binary path contains null byte")?;
    let c_argv: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_bytes()).context("argument contains null byte"))
        .collect::<Result<_>>()?;

    tracing::info!("execing {binary}");
    nix::unistd::execvp(&c_binary, &c_argv).context("execvp")?;
    unreachable!()
}

/// Raise `CAP_NET_BIND_SERVICE` into the inheritable + ambient capability sets,
/// so the bridge exec'd after `no_new_privs` inherits the ability to bind its
/// privileged listener ports. Requires the cap in our permitted set — provided
/// by `setcap cap_net_bind_service=+ep /usr/local/bin/fauna-sandbox` in the
/// image. Ambient caps require the cap in BOTH permitted and inheritable, so we
/// add it to inheritable first (we already hold it permitted), then ambient.
fn raise_net_bind_ambient() -> Result<()> {
    use caps::{CapSet, Capability};
    caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_BIND_SERVICE)
        .context("raise CAP_NET_BIND_SERVICE into inheritable set")?;
    caps::raise(None, CapSet::Ambient, Capability::CAP_NET_BIND_SERVICE)
        .context("raise CAP_NET_BIND_SERVICE into ambient set")?;
    tracing::debug!("CAP_NET_BIND_SERVICE raised into ambient set");
    Ok(())
}

/// Set PR_SET_NO_NEW_PRIVS to prevent privilege escalation via setuid/setgid.
fn set_no_new_privs() -> Result<()> {
    // SAFETY: prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) is safe — it only
    // restricts the calling thread (and future children) from gaining privileges.
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        bail!(
            "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    tracing::debug!("PR_SET_NO_NEW_PRIVS set");
    Ok(())
}

/// Close all file descriptors above stderr (fd > 2).
fn close_excess_fds() {
    // Collect FDs first — the ReadDir iterator holds a directory FD that
    // must not be closed during iteration. After collect(), the iterator
    // drops and closedir runs cleanly. We then close the remaining FDs.
    let fds: Vec<i32> = match std::fs::read_dir("/proc/self/fd") {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| e.file_name().to_string_lossy().parse::<i32>().ok())
            .filter(|&fd| fd > 2)
            .collect(),
        Err(_) => {
            tracing::warn!("cannot read /proc/self/fd, skipping FD cleanup");
            return;
        }
    };
    // ReadDir is now dropped (closedir ran). Close remaining FDs.
    // Some may already be closed (like the dir FD itself) — ignore errors.
    for fd in fds {
        unsafe { libc::close(fd) };
    }
    tracing::debug!("closed FDs > 2");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_new_privs_is_set() {
        set_no_new_privs().expect("should succeed");

        // Verify via PR_GET_NO_NEW_PRIVS (returns 1 if set, 0 if not)
        let val = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
        assert_eq!(val, 1, "PR_GET_NO_NEW_PRIVS should return 1 after setting");
    }
}
