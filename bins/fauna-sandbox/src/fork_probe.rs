//! A **bounded** fork-and-wait for the sandbox tests, and the async-signal-safe
//! discipline the child half of one has to keep.
//!
//! ## The 20-hour build slot
//!
//! Every test in this crate has to apply its sandbox (seccomp filter, Landlock
//! ruleset) in a process that is not the test runner — the restrictions are
//! one-way, so a test that applied them to itself would poison every later test
//! in the binary. `fork()` is the obvious way to get that process, and it is
//! also a trap: **libtest is multi-threaded, and `fork()` keeps only the calling
//! thread.** Every lock another thread held at that instant — the malloc arena
//! above all — is inherited *locked*, by a thread that no longer exists to
//! release it. The child then allocates (building a filter, formatting a panic
//! message, flushing stdout at `std::process::exit`), blocks on that lock
//! forever, and the parent's unbounded `waitpid` blocks with it.
//!
//! That is not theory. Measured on the primary Linux dev VM 2026-08-27: the gate's
//! `workspace-test-check` had been wedged in exactly this state for **19 h
//! 53 m** — parent and child both in `futex_do_wait`, the last line of its log
//! reading `test seccomp::tests::test_seccomp_blocks_ptrace has been running
//! for over 60 seconds` — holding one of the machine's two build slots the
//! whole time. A merge gate that can never report green *or* red, and a build
//! pool at half capacity for every session on the box.
//!
//! ## What this module fixes, and what it deliberately does not
//!
//! Two independent halves, because they close two different failure classes:
//!
//! * **The watchdog ([`run_bounded`]) makes the class survivable.** The parent
//!   never blocks indefinitely again: it reaps with `WNOHANG` against a
//!   generous deadline and, on expiry, `SIGKILL`s the child and reports
//!   [`ForkProbe::TimedOut`]. An inherited-lock deadlock in *any* child — this
//!   crate's or a future one's — becomes a red test in seconds instead of an
//!   unbounded hold on shared machinery. The deadline is a watchdog, never an
//!   assertion: what a test asserts is the child's exit code, and the ceiling
//!   is sized far above any non-pathological run so a green pass pays nothing
//!   for it.
//! * **The child-side discipline makes the class rarer.** A caller does its
//!   allocating work *before* the fork and hands the child only syscalls and an
//!   exit code — see [`run_bounded`]'s contract. `_exit` rather than
//!   `std::process::exit`, because the latter runs atexit handlers and flushes
//!   stdio, both of which take locks.
//!
//! Neither half makes `fork()` in a threaded process *safe* — nothing can, short
//! of fork+exec. They make it bounded, and they remove the allocation from
//! every child that can live without it.
//!
//! ⚠ **One child still allocates, and it is not an oversight.** The seccomp
//! tests' filter bakes `std::process::id()` into its `kill`/`tgkill` conditions,
//! so a filter built by the parent governs the *parent's* pid and refuses the
//! child's own `kill(self, 0)` — measured on the first attempt to hoist it. That
//! build stays inside the child, and the watchdog is what keeps it from costing
//! the machine a build slot. `landlock`'s ruleset carries no such binding and
//! **is** built before the fork, leaving its child two `open`s and an exit code.

use std::time::{Duration, Instant};

/// How long a sandboxed child may take before the watchdog kills it.
///
/// Generous by design (convention 14's reasoning, applied to a Rust watchdog):
/// every one of these children does a handful of syscalls and exits in
/// milliseconds, so anything approaching this ceiling is a deadlock, not a slow
/// machine. A green run never waits on it.
const CHILD_BUDGET: Duration = Duration::from_secs(60);

/// How often the parent re-reaps while waiting. Small enough that a normal run
/// finishes in one or two polls, large enough not to spin a core.
const REAP_POLL: Duration = Duration::from_millis(20);

/// What became of a forked child.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForkProbe {
    /// It exited on its own, with this code.
    Exited(i32),
    /// It was killed by a signal (the child crashed, or a seccomp filter
    /// answered `SIGSYS` where the test expected an errno).
    Signalled(i32),
    /// It was still alive when [`CHILD_BUDGET`] ran out and has been
    /// `SIGKILL`ed and reaped. **This is the inherited-lock deadlock**; the
    /// module doc says why it happens and why it must not be retried into
    /// green.
    TimedOut,
    /// `fork()` itself failed.
    ForkFailed(i32),
}

/// Fork, run `child` in the child, and wait for it under a watchdog.
///
/// `child` returns the exit code the child process should carry; it is passed
/// to `_exit` verbatim and never returns.
///
/// ## The caller's half of the contract
///
/// The closure runs in a process that inherited this one's locked mutexes (see
/// the module doc), so it must be **async-signal-safe in practice**:
///
/// * Do every allocation, every file read, every `String` format **before**
///   calling this — build the filter, open the paths, then fork.
/// * Inside the closure, prefer raw syscalls and integer comparisons. Return a
///   distinct code per failure the test wants to tell apart, and let the parent
///   turn codes into sentences.
/// * Never `panic!`, `assert!`, `expect` or `unwrap` in the closure: the panic
///   machinery formats a message, which allocates, which is the deadlock.
///
/// The watchdog means breaking this contract costs a red test rather than a
/// wedged machine — but it is still a red test, so keep it.
pub(crate) fn run_bounded(child: impl FnOnce() -> i32) -> ForkProbe {
    run_bounded_within(CHILD_BUDGET, child)
}

/// [`run_bounded`] with the watchdog's ceiling passed in — the seam the
/// watchdog's own pin drives, so proving the kill path costs milliseconds
/// instead of a full [`CHILD_BUDGET`]. Production callers take the constant.
fn run_bounded_within(budget: Duration, child: impl FnOnce() -> i32) -> ForkProbe {
    // Captured BEFORE the fork so the child's die-with-parent guard has a
    // comparand no post-fork read can race — see die_with_parent's doc.
    // SAFETY: a plain syscall, valid at any time.
    let parent = unsafe { libc::getpid() };
    // SAFETY: `fork` is always safe to call; what is delicate is what the child
    // then does, which is the closure's contract above.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return ForkProbe::ForkFailed(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
    }
    if pid == 0 {
        die_with_parent(parent);
        let code = child();
        // `_exit`, never `std::process::exit`: the latter runs atexit handlers
        // and flushes stdio, and both take locks this process may have
        // inherited already held.
        unsafe { libc::_exit(code) };
    }

    let deadline = Instant::now() + budget;
    let mut status = 0i32;
    loop {
        // SAFETY: `pid` is our own child and `status` is a live `i32`.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if reaped == pid {
            return classify(status);
        }
        if reaped < 0 {
            return ForkProbe::ForkFailed(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(-1),
            );
        }
        if Instant::now() >= deadline {
            // SAFETY: `pid` is our own child; SIGKILL is unblockable, so the
            // blocking reap that follows cannot hang in turn.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
            return ForkProbe::TimedOut;
        }
        std::thread::sleep(REAP_POLL);
    }
}

/// Ask the kernel to `SIGKILL` this child if its parent dies. Runs in the
/// child, first thing, and is two raw syscalls — the closure's contract.
///
/// **The watchdog above only works while there is a parent to enforce it.** If
/// the parent dies between the fork and the deadline — the agent harness reaping
/// a background task, the machine's OOM pressure, a `SIGKILL` at any of the
/// places this fleet delivers one — nothing is left to reap the child, and the
/// child keeps *every fd the fork copied*: the build slot the run held, the
/// gate-check singleton, any flock at all. The orphan then blocks the machine
/// for as long as nobody notices it by hand.
///
/// Measured, and it is the same incident twice. The 20-hour wedge in the module
/// doc was diagnosed and the watchdog written for it on 2026-08-27 — but the
/// child that had caused it was never killed, so it went on holding
/// `build-0.lock` **and** the gate-check's `check.lock` until a replay pass
/// found it on 2026-08-29: **2 d 23 h**, two full days after the fix landed,
/// with that machine's entire heavy-gate tier silently unable to run a pass in
/// that window (every kick took `flock -n || exit 0` and said nothing) and the
/// build pool at half capacity for every session on the box. The watchdog was
/// the right fix for the failure it saw; this is the half it structurally
/// cannot cover, because it needs a live parent to run at all.
///
/// `PR_SET_PDEATHSIG` fires on the death of the parent *thread*, which is
/// exactly right here: libtest gives each test its own thread, and that thread
/// is the one whose reap loop is the child's only bound. `parent` is the
/// caller's pid, read in the PARENT before the fork ([`run_bounded_within`]) —
/// not a post-fork `getppid()`, which is exactly the value that changes when
/// the parent dies. A post-fork baseline only closes the race in the window
/// *after* that first read; a parent death before it leaves both reads
/// agreeing and the guard blind. Comparing against a pre-fork comparand
/// depends on no post-fork read at all, so there is no window left to race.
fn die_with_parent(parent: libc::pid_t) {
    // SAFETY: both are plain syscalls on this process, valid at any time, and
    // `_exit` is the async-signal-safe exit the module doc requires.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        if libc::getppid() != parent {
            libc::_exit(0);
        }
    }
}

fn classify(status: i32) -> ForkProbe {
    if libc::WIFEXITED(status) {
        ForkProbe::Exited(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        ForkProbe::Signalled(libc::WTERMSIG(status))
    } else {
        // Neither exited nor signalled: `waitpid` without `WUNTRACED` should
        // not produce this, so report the raw status rather than guessing.
        ForkProbe::Signalled(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_that_exits_reports_its_code() {
        assert_eq!(run_bounded(|| 0), ForkProbe::Exited(0));
        assert_eq!(run_bounded(|| 7), ForkProbe::Exited(7));
    }

    /// The watchdog itself — the half that turns the measured 20-hour wedge
    /// into a bounded red. Driven with a child that pauses forever rather than
    /// one that deadlocks on an inherited lock, because the two are
    /// indistinguishable to the parent and only one of them is stageable on
    /// purpose.
    #[test]
    fn a_child_that_never_exits_is_killed_and_reported() {
        let budget = Duration::from_millis(200);
        let started = Instant::now();
        let verdict = run_bounded_within(budget, || {
            // SAFETY: `pause` blocks until a signal arrives; SIGKILL is what
            // arrives.
            unsafe {
                libc::pause();
            }
            0
        });
        assert_eq!(verdict, ForkProbe::TimedOut);
        assert!(
            started.elapsed() >= budget,
            "the watchdog returned before its own deadline, so something other \
             than the budget ended this wait"
        );
    }

    /// The half the watchdog cannot reach: a child whose PARENT dies.
    ///
    /// Staged as the real incident rather than as a pid check — what cost this
    /// fleet three days was an orphan **holding fds the fork had copied**, so
    /// that is the observable. An intermediate process calls
    /// `run_bounded_within` with a budget far longer than this test; its child
    /// takes the write end of a pipe, announces itself down it, and blocks
    /// forever. The intermediate is `SIGKILL`ed — the harness reap, the OOM
    /// kill, every way this happens for real — and the pipe must reach EOF,
    /// which it can only do once the grandchild's copy of the write end is
    /// closed too, i.e. once it died with its parent.
    ///
    /// A pipe and not an `flock`: `fork` SHARES an open file description, so a
    /// lock the child takes on an inherited fd is the same lock the parent
    /// holds — the parent could re-take it while the child still lived, and the
    /// test would pass having proved nothing. A pipe's EOF counts *holders*,
    /// which is exactly the property at issue.
    ///
    /// Without `die_with_parent` the grandchild survives its parent and the
    /// poll below times out with the fd still held — precisely the state the
    /// machine was found in.
    #[test]
    fn a_child_dies_with_its_parent_instead_of_holding_its_fds() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a live two-element array, which is `pipe`'s contract.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe failed");
        let (read_fd, write_fd) = (fds[0], fds[1]);

        // SAFETY: an ordinary fork; the intermediate below only makes syscalls.
        let middle = unsafe { libc::fork() };
        assert!(middle >= 0, "fork failed");
        if middle == 0 {
            // SAFETY: the intermediate drops the read end so its own copy
            // cannot keep the pipe open, then waits on a child under a budget
            // this test never reaches.
            unsafe { libc::close(read_fd) };
            let _ = run_bounded_within(Duration::from_secs(300), || {
                // SAFETY: `write_fd` is open here (inherited); `pause` blocks
                // until the SIGKILL that PR_SET_PDEATHSIG delivers. The pid
                // goes down the pipe so the parent can reclaim this process on
                // the FAILING path — see the cleanup note below.
                unsafe {
                    let pid = libc::getpid();
                    libc::write(write_fd, std::ptr::addr_of!(pid).cast(), 4);
                    loop {
                        libc::pause();
                    }
                }
            });
            // SAFETY: async-signal-safe exit; unreachable in practice.
            unsafe { libc::_exit(0) };
        }

        // Only the intermediate and its child may hold the write end now.
        // SAFETY: closing this process's own copy.
        unsafe { libc::close(write_fd) };

        // Wait for the grandchild to announce its pid, so the kill below cannot
        // race ahead of the thing under test — and so the cleanup below has
        // something to aim at.
        let mut buf = [0u8; 4];
        // SAFETY: reading four bytes into a live buffer from our own fd.
        let announced = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), 4) };
        assert_eq!(
            announced, 4,
            "the grandchild never signalled that it was alive"
        );
        let grandchild = i32::from_ne_bytes(buf);

        // SAFETY: `middle` is our own child, and SIGKILL is unblockable.
        unsafe {
            libc::kill(middle, libc::SIGKILL);
            let mut status = 0i32;
            libc::waitpid(middle, &mut status, 0);
        }

        let mut poll_fd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd, ten seconds — a budget, never an assertion.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, 10_000) };

        // Reclaim the grandchild BEFORE asserting, unconditionally. On the
        // failing path it is exactly the orphan under test — and it holds this
        // process's stdout and stderr, so leaving it behind wedges whatever ran
        // the suite, which is the disease rather than the diagnosis. Measured
        // while red-verifying this very test: the pipeline `cargo test | grep`
        // never saw EOF and hung until the orphan was killed by hand. On the
        // passing path it is already dead and this is a harmless ESRCH.
        // SAFETY: a signal to a pid this test created; SIGKILL is unblockable.
        unsafe { libc::kill(grandchild, libc::SIGKILL) };

        assert!(
            ready > 0,
            "the parent is dead and its child still holds the fd it inherited -- \
             this is the orphan that held a build slot AND the gate-check \
             singleton for 2 d 23 h"
        );
        // SAFETY: same fd, same live buffer.
        let eof = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), 1) };
        assert_eq!(eof, 0, "the pipe reported readable without reaching EOF");
        // SAFETY: our own fd, done with.
        unsafe { libc::close(read_fd) };
    }

    /// A child killed by a signal is NOT reported as a clean exit — the
    /// distinction is what lets a seccomp test tell "the syscall returned the
    /// errno I expected" from "the filter answered SIGSYS".
    #[test]
    fn a_signalled_child_is_distinguishable_from_an_exited_one() {
        let verdict = run_bounded(|| {
            // SAFETY: killing ourselves with SIGKILL from inside the child.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            0
        });
        assert_eq!(verdict, ForkProbe::Signalled(libc::SIGKILL));
    }
}
