"""Port allocation and process tracking/reaping for harness children.

The reaping half is the operational core of the self-terminating-harness
conventions (e2e-conventions.md § Cross-app e2e conventions, point 9): every
process the harness spawns (web bridge, nest, mail bridge, xvfb + desktop
app) must die with the run that spawned it — including when pytest itself is
SIGKILLed and no Python cleanup (fixture teardown, atexit) ever runs. Two
mechanisms, layered:

* `popen_group_kwargs()` — spawn kwargs making the child a session/process-
  group leader (so one `killpg` reaps its whole subtree), arming
  `PR_SET_PDEATHSIG=SIGTERM` on Linux so the kernel signals the child the
  moment the spawning thread dies, and registering the child's new process
  group with this run's reaper (below) — the pipe-EOF mechanism that carries
  the same guarantee on darwin, which has no parent-death signal at all.
  This is what survives `kill -9` of pytest.
* `terminate_tree(proc)` — group-wide TERM → wait → KILL teardown for the
  cooperative paths (fixture teardown, the atexit sweep below).

Scoping rule (process safety): both only ever touch process groups this run
created — never a pid/group discovered by name, which could belong to a
sibling session's run.
"""
import atexit
import os
import signal
import socket
import subprocess
import sys
import time
import weakref

_tracked: list[subprocess.Popen] = []
# Reader threads started by drain_pipes(), keyed by the Popen they drain —
# so wait_pipes_drained() can block on the real EOF signal instead of a
# `recent`-is-truthy heuristic. Weak so a forgotten proc doesn't leak threads.
_drain_threads: "weakref.WeakKeyDictionary[subprocess.Popen, list]" = (
    weakref.WeakKeyDictionary()
)


# ── the run-lifetime reaper: the die-with-parent half on a POSIX platform with
#    no PR_SET_PDEATHSIG (darwin), and a second belt where there is one ───────
#
# macOS has neither PDEATHSIG nor job objects, so every darwin harness child was
# held only by `terminate_tree`/atexit — cleanup code, which a SIGKILLed pytest
# by definition never runs. Measured: an e2e trio (app + sync agent + nest) plus
# two mail bridges outlived their run by 4–7 h on macOS, all reparented to PID 1,
# holding two ports and ~375 MB (2026-08-22).
#
# The portable primitive macOS does have is pipe EOF: when a pipe's last writer
# closes — including because the kernel tore that process down, with no cleanup
# code running — the reader sees EOF. So one small helper holds the read end of a
# pipe whose only writer is this interpreter, and every child registers its own
# process group on that pipe from inside its `preexec_fn`.
#
# Two design constraints, both load-bearing:
#
#  * **The arming rides INSIDE `Popen(**popen_group_kwargs())`.** A mechanism
#    armed by a SEPARATE call after the spawn will be forgotten — exactly how the
#    Windows half was omitted at four of five sites for eight days
#    (e2e-conventions.md § 9). Registration is therefore one `os.write` in the
#    preexec hook, never a `register_group(proc)` a new spawn site can skip.
#  * **It runs on every POSIX platform, Linux included.** That same section
#    records darwin as "the platform with no PDEATHSIG, i.e. the weakest reaping
#    story, [and] the one never running its own proofs". A mechanism that only
#    ever executes on one machine's runs is a mechanism that rots; running it
#    everywhere makes the Linux dev machines' runs red-verify macOS's guarantee.
#
# Process safety: the reaper only ever signals process groups THIS run created
# and registered — never a pid discovered by name — and a clean interpreter exit
# sends `BYE` first, so a normally-torn-down run never signals a pgid the OS may
# since have recycled.
_REAPER_SRC = r"""
import os, signal, sys, time

fd, groups, buf, clean = int(sys.argv[1]), [], b"", False
while True:
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break  # EOF: every writer is gone, i.e. the run that spawned us died
    buf += chunk
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        line = line.strip()
        if line == b"BYE":
            clean = True
        elif line:
            try:
                groups.append(int(line))
            except ValueError:
                pass
if clean:
    sys.exit(0)  # the run tore itself down; its pgids may already be recycled


def leads_a_group(pgid):
    try:
        return os.getpgid(pgid) == pgid
    except ProcessLookupError:
        return True  # leader already reaped; its group may still hold members
    except OSError:
        return False


live = [g for g in groups if leads_a_group(g)]
for sig in (signal.SIGTERM, signal.SIGKILL):
    for pgid in live:
        try:
            os.killpg(pgid, sig)
        except OSError:
            pass
    if sig == signal.SIGTERM and live:
        time.sleep(2.0)  # sleep-ok: the TERM -> wait -> KILL grace, as terminate_tree
"""

_reaper_proc: "subprocess.Popen | None" = None
_reaper_w: int = -1

# The reaper is always a REAL process, so it spawns through `Popen` as bound at
# import. A test that replaces `subprocess.Popen` with a stand-in and reaches
# `popen_group_kwargs()` first would otherwise get the stand-in memoized as the
# reaper: it holds no read end, so the pipe below has no reader and every LATER
# group spawn in the process dies of SIGPIPE in its preexec (returncode -13) —
# an order-dependent failure that surfaces in an unrelated test, far from the fake.
_REAL_POPEN = subprocess.Popen


def _start_reaper() -> None:
    """Start this run's reaper once, lazily, at the first group spawn."""
    global _reaper_proc, _reaper_w
    if _reaper_proc is not None or os.name != "posix":
        return
    r = w = -1
    try:
        r, w = os.pipe()
        os.set_inheritable(r, True)
        _reaper_proc = _REAL_POPEN(
            [sys.executable, "-c", _REAPER_SRC, str(r)],
            pass_fds=(r,),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            # Its own session: the reaper must survive every group it reaps, and
            # must not be swept by a killpg aimed at one of them.
            start_new_session=True,
        )
        os.close(r)
        _reaper_w = w  # held for the life of the run; EOF here IS the trigger
    except Exception:
        # Never block a spawn: reaping degrades to terminate_tree/atexit.
        _reaper_proc = None
        _reaper_w = -1
        for fd in (r, w):
            if fd >= 0:
                try:
                    os.close(fd)
                except OSError:
                    pass


def _release_reaper() -> None:
    """Clean-exit path: tell the reaper this run tore itself down, then close."""
    global _reaper_w
    if _reaper_w < 0:
        return
    try:
        os.write(_reaper_w, b"BYE\n")
    except OSError:
        pass
    try:
        os.close(_reaper_w)
    except OSError:
        pass
    _reaper_w = -1


def _die_with_parent_preexec() -> None:
    """(child-side, between fork and exec) Own session + die-with-parent."""
    os.setsid()
    if sys.platform == "linux":
        try:
            import ctypes

            # PR_SET_PDEATHSIG = 1: kernel delivers SIGTERM to this child when
            # the spawning *thread* exits — the guarantee that holds even when
            # the parent is SIGKILLed and no Python cleanup ever runs.
            ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGTERM, 0, 0, 0)
        except Exception:
            pass  # reaping degrades to terminate_tree/atexit, never blocks spawn
    if _reaper_w >= 0:
        try:
            # setsid() just made this process its own group leader, so its pid
            # IS the pgid the reaper must kill. One write, inside the spawn.
            os.write(_reaper_w, str(os.getpid()).encode() + b"\n")
        except OSError:
            pass


def popen_group_kwargs() -> dict:
    """Popen kwargs that make the child a reapable group leader (POSIX).

    Arms both POSIX halves of point 9's die-with-the-run guarantee: the child
    becomes a session/process-group leader, Linux arms `PR_SET_PDEATHSIG`, and on
    every POSIX platform the child's new pgid is registered with this run's
    reaper (above) — which is the whole mechanism on darwin, where no
    kernel-level parent-death signal exists.

    On Windows this returns {} — no preexec_fn/PDEATHSIG there. Use
    `reap_descendants_of()` instead, which is the Windows mechanism with the
    same guarantee.
    """
    if os.name == "posix":
        _start_reaper()
        return {"preexec_fn": _die_with_parent_preexec}
    return {}


# Job handles held for the life of the run. Closing one — explicitly at
# teardown, or by the OS when this process dies for ANY reason, `kill -9`
# included — terminates every process still in it.
_jobs: list[int] = []


def reap_descendants_of(pid: int) -> bool:
    """Windows: bind `pid` **and every descendant it ever spawns** to a
    kill-on-close job object owned by this run. Returns False off Windows or if
    the OS refused (the caller then degrades to single-process teardown).

    This is the Windows half of point 9's die-with-the-run guarantee, and it is
    needed for exactly the reason `_die_with_parent_preexec` exists on POSIX:
    `terminate_tree` can only reach the process it was handed, so a
    **grandchild** outlives the run. A client that spawns a helper is the normal
    case, not a corner one — fauna-tui spawns `fauna-sync-agent`, whose whole
    job is to keep running after the app that started it exits. Correct in
    production, machine-global contamination in a test (testing.md § point 10):
    the orphan holds its build artifact against the next compile and answers a
    pipe the next launch expects to serve itself.

    `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is what makes this survive a SIGKILLed
    pytest: no cleanup code has to run, because the *kernel* closes the handle
    when this process dies and the job dies with it. Nested jobs are allowed
    since Windows 8, so being inside a CI/agent job already does not break it.

    Scoping (process safety): only the pid handed in — a pid this run spawned —
    and its future descendants. Never a pid discovered by name.
    """
    if os.name != "nt":
        return False
    try:
        import ctypes
        from ctypes import wintypes

        JobObjectExtendedLimitInformation = 9
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000
        PROCESS_TERMINATE = 0x0001
        PROCESS_SET_QUOTA = 0x0100

        class IO_COUNTERS(ctypes.Structure):
            _fields_ = [(n, ctypes.c_ulonglong) for n in (
                "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
                "ReadTransferCount", "WriteTransferCount", "OtherTransferCount",
            )]

        class JOBOBJECT_BASIC_LIMIT_INFORMATION(ctypes.Structure):
            _fields_ = [
                ("PerProcessUserTimeLimit", wintypes.LARGE_INTEGER),
                ("PerJobUserTimeLimit", wintypes.LARGE_INTEGER),
                ("LimitFlags", wintypes.DWORD),
                ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t),
                ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t),  # ULONG_PTR
                ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD),
            ]

        class JOBOBJECT_EXTENDED_LIMIT_INFORMATION(ctypes.Structure):
            _fields_ = [
                ("BasicLimitInformation", JOBOBJECT_BASIC_LIMIT_INFORMATION),
                ("IoInfo", IO_COUNTERS),
                ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t),
            ]

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        # Explicit restypes: the default `c_int` TRUNCATES a 64-bit HANDLE, and
        # the truncated value then fails every later call for no visible reason.
        k32.CreateJobObjectW.restype = wintypes.HANDLE
        k32.OpenProcess.restype = wintypes.HANDLE

        job = k32.CreateJobObjectW(None, None)
        if not job:
            return False
        info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION()
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not k32.SetInformationJobObject(
            job, JobObjectExtendedLimitInformation, ctypes.byref(info), ctypes.sizeof(info)
        ):
            k32.CloseHandle(job)
            return False
        handle = k32.OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, False, pid)
        if not handle:
            k32.CloseHandle(job)
            return False
        assigned = bool(k32.AssignProcessToJobObject(job, handle))
        k32.CloseHandle(handle)
        if not assigned:
            k32.CloseHandle(job)
            return False
        _jobs.append(job)
        return True
    except Exception:
        return False  # containment is a belt; never block a spawn


def _close_jobs() -> None:
    """Close every held job handle — terminating whatever is still inside."""
    if os.name != "nt" or not _jobs:
        return
    try:
        import ctypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        for job in _jobs:
            try:
                k32.CloseHandle(job)
            except Exception:
                pass
    except Exception:
        pass
    _jobs.clear()


def _signal_group(pid: int, sig: int) -> bool:
    """Signal pid's process GROUP iff pid leads one; False → caller falls back.

    The leader check keeps this safe on a Popen spawned WITHOUT
    `popen_group_kwargs()`: such a child shares the invoking pytest's group,
    and an unguarded killpg would take down the run itself.
    """
    try:
        if os.getpgid(pid) != pid:
            return False
    except (ProcessLookupError, OSError):
        pass  # leader already reaped; its group id may still have members
    try:
        os.killpg(pid, sig)
        return True
    except (ProcessLookupError, PermissionError, OSError):
        return False


def terminate_tree(proc: subprocess.Popen | None, *, term_timeout: float = 5.0) -> None:
    """Terminate `proc` and its whole process group (children + grandchildren).

    Group-wide only for children spawned with `popen_group_kwargs()`; falls
    back to single-process terminate/kill otherwise (and on Windows). Always
    finishes with a group SIGKILL sweep so a TERM-ignoring grandchild (e.g. a
    wedged browser) cannot outlive the run.
    """
    if proc is None:
        return
    if os.name != "posix":
        try:
            proc.terminate()
            proc.wait(timeout=term_timeout)
        except Exception:
            try:
                proc.kill()
            except Exception:
                pass
        return
    if not _signal_group(proc.pid, signal.SIGTERM):
        try:
            proc.terminate()
        except Exception:
            pass
    try:
        proc.wait(timeout=term_timeout)
    except Exception:
        pass
    if not _signal_group(proc.pid, signal.SIGKILL):
        try:
            proc.kill()
        except Exception:
            pass
    try:
        proc.wait(timeout=3.0)
    except Exception:
        pass


def find_free_port() -> int:
    """Find a free TCP port by binding to port 0 and reading the assignment."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def find_free_ports(n: int) -> list[int]:
    """Find `n` DISTINCT free TCP ports.

    `find_free_port()` closes its socket before returning, so the kernel is free
    to hand the SAME just-freed port to the next rapid call — calling it N times
    in a row (e.g. a bridge spawn allocating imaps/imap/caldav/metrics binds) can
    yield duplicates, and the second listener to bind a colliding port crashes
    ("address already in use"), taking the whole bridge down. Holding all N
    sockets open SIMULTANEOUSLY until every port is chosen guarantees the kernel
    assigns N different ports; we close them only after reading all assignments
    (a brief TOCTOU window before the real listeners bind remains, but the ports
    are now distinct from each other — the failure mode this fixes)."""
    socks = []
    try:
        for _ in range(n):
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.bind(("127.0.0.1", 0))
            socks.append(s)
        return [s.getsockname()[1] for s in socks]
    finally:
        for s in socks:
            s.close()


def track_process(proc: subprocess.Popen) -> None:
    """Register a subprocess for cleanup on Python exit.

    If pytest crashes or the process exits without teardown, atexit
    ensures the subprocess is terminated — preventing orphaned bridges.
    """
    _tracked.append(proc)


def untrack_process(proc: subprocess.Popen) -> None:
    """Remove a subprocess from cleanup tracking (called during normal teardown)."""
    try:
        _tracked.remove(proc)
    except ValueError:
        pass


def _cleanup():
    for proc in _tracked:
        try:
            terminate_tree(proc)
        except Exception:
            pass
    _tracked.clear()
    # After the cooperative teardown: anything still alive in a job (a
    # grandchild `terminate_tree` cannot reach) goes with the handle.
    _close_jobs()
    # This atexit hook running at all IS the clean-exit proof — a SIGKILLed
    # run never reaches here, and its reaper fires on the resulting EOF.
    _release_reaper()


atexit.register(_cleanup)


def drain_pipes(
    proc: subprocess.Popen, maxlen: int = 2000, echo=None, echo_filter=None
) -> "collections.deque[str]":
    """Continuously drain ``proc``'s stdout/stderr for the life of the process.

    **Redirecting a pipe obliges you to read it, for as long as the child lives.**
    A ``subprocess.PIPE`` that nobody reads fills its ~64 KB OS buffer and then
    blocks the writer's calling thread *forever* on its next write. A handshake
    read (``readline()`` until a ``BRIDGE_PORT=`` line) is NOT a drain: it stops
    reading the moment the port arrives, and everything the child logs afterwards
    accumulates against that ceiling.

    This is not hygiene — it silently corrupts test results. The windows bridge hit
    it in the worst possible form: the app under test INHERITS the bridge's
    stdout/stderr (started with ``UseShellExecute=false`` and no redirect of its
    own), and its native tracing subscriber writes every log line to stderr. Once
    the buffer filled, the next log write blocked whichever thread emitted it — and
    when that was the UI thread, the WinUI dispatcher stopped draining, so queued
    test-agent post-actions never ran and ``set_state`` failed with "App did not
    acknowledge command ... within 10.0s" while the app was alive and its poll loop
    kept a perfect ~1s heartbeat. That signature was chased as a product bug across
    seven sessions.

    The tell for this failure class: it tracks cumulative LOG VOLUME, not product
    state — so it strikes late in a run, looks like a flake, and ADDING log lines
    makes it strike sooner.

    Returns a bounded deque holding the most recent lines, so the output is still
    available for diagnostics instead of being thrown away.

    ``echo`` (a writable text stream, default None = silent) additionally TEES
    each line as it arrives. Draining and showing-live pull apart for a
    long-running child: capture-then-report makes a healthy multi-minute build
    look hung, while stream-and-forget loses the output on paths where the
    child's inherited fd never reaches the pytest log. A tee escapes both, and it
    stays opt-in because echoing every driver's log volume by default would put
    exactly the wrong thing back on the critical path.

    ``echo_filter`` (a ``str -> bool`` predicate over the raw line, default None =
    tee everything) narrows that tee without narrowing the deque, which is what
    makes an ALWAYS-ON tee affordable. The returned deque is a post-mortem
    instrument and shares the fate of the reporting path that quotes it: when the
    pytest-timeout watchdog kills the process, no ``FAILURES`` section is ever
    formatted and every retained line dies with it — precisely when a hang is bad
    enough to need them. A line already written to the captured stream survives
    that kill (the timeout dump prints captured stderr), so tee the handful of
    lines a post-mortem actually reads and leave the child's bulk log volume off
    the critical path.
    """
    import collections
    import threading

    recent: "collections.deque[str]" = collections.deque(maxlen=maxlen)
    echo_lock = threading.Lock()  # two reader threads, one destination stream

    def _echo_wanted(predicate, line: str) -> bool:
        """Whether this line is tee'd. A predicate that raises must not silence
        the tee — the drain is the one thing here that may never grow a new way
        to fail, so a broken filter degrades to echoing rather than to nothing."""
        if predicate is None:
            return True
        try:
            return bool(predicate(line))
        except Exception:
            return True

    def _drain(stream, tag: str) -> None:
        if stream is None:
            return
        try:
            while True:
                line = stream.readline()
                if not line:  # EOF: "" on a text pipe, b"" on a binary one
                    break
                if isinstance(line, bytes):
                    line = line.decode("utf-8", "replace")
                recent.append(f"[{tag}] {line.rstrip()}")
                if echo is not None and _echo_wanted(echo_filter, line):
                    # Best-effort: a broken echo stream must never take down the
                    # drain, or we would reintroduce the blocked-writer deadlock
                    # this function exists to prevent.
                    try:
                        with echo_lock:
                            echo.write(line if line.endswith("\n") else line + "\n")
                            echo.flush()
                    except Exception:
                        pass
        except Exception:
            pass  # closed at teardown — nothing to report

    threads = []
    for stream, tag in ((proc.stdout, "out"), (proc.stderr, "err")):
        t = threading.Thread(target=_drain, args=(stream, tag), daemon=True)
        t.start()
        threads.append(t)
    _drain_threads[proc] = threads
    return recent


def wait_pipes_drained(proc: subprocess.Popen, timeout: float = 10.0) -> bool:
    """Block until ``drain_pipes(proc)``'s reader threads have both hit EOF.

    ``proc.wait()`` returning only means the child exited; it says nothing
    about whether the daemon reader threads have been scheduled to read the
    last buffered lines yet. A caller that quotes the ``recent`` tail right
    after ``proc.wait()`` by checking only that ``recent`` is non-empty can
    genuinely observe a PARTIAL tail under load: whichever stream's reader
    thread happens to get scheduled first "counts" as drained even though the
    other stream — often the one carrying the actual error, on stderr — has
    not been read at all yet. This blocks on the real completion signal (both
    reader threads returning, i.e. EOF) instead, per testing.md convention 14.

    Returns whether draining finished within ``timeout``; a caller can still
    quote whatever ``recent`` holds on a timeout, same as before this existed.
    """
    threads = _drain_threads.get(proc, [])
    deadline = time.monotonic() + timeout
    for t in threads:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        t.join(remaining)
        if t.is_alive():
            return False
    return True
