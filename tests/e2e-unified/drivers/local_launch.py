"""The launch-timeout rider's witnesses for an app the harness spawned itself.

Convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention): a
readiness gate that ends names its suspect from four witnesses the launching
process holds. This module is the driver side for every app launched as a local
child process — linux's `fauna-desktop` (under `xvfb-run` where that leads) and
tui's `fauna-tui` — so the two gather them one way; the verdict and its grammar
are `http_bridge.launch_timeout_report`'s, as for every driver.

(a) the process is the handle the driver holds (`poll()`), and the CPU the APP was
given is read from `/proc/<pid>/stat` for the app's own pid — resolved under the
handle, because the handle is `xvfb-run`'s shell wherever that wrapper leads, and
the wrapper's CPU time says nothing about the app's. (b) is the launch's
`app.err` and the app's own rolling log, (c) the timeline from the spawn, (d) this
machine's agent ledger (`helpers/launch_load.py`). Where `/proc` is absent (the
tui on macOS or Windows) the CPU witness is an absence stated, never guessed.

The apple legs (`drivers/macos.py`, `drivers/ios.py`) come through here too, with
`ps` for the CPU and, because darwin keeps no run-queue wait, a `ThreadStateSampler`
over each launch gate for the wait.

Nothing here raises: it runs inside a ``raise``.

The module also holds convention 6's close rider for the same local children
(`read_exit_before_close`, at its end): the exit read before the harness's close,
a crash recorded for the phase that found it.
"""

from __future__ import annotations

import os
import re
import signal
import subprocess
import threading
import time
import weakref
from pathlib import Path

from .http_bridge import LaunchWitnesses, launch_timeout_report

PROC_ROOT = "/proc"
#: The kernel's clock tick, which `/proc/<pid>/stat` counts CPU in. Read where the
#: platform has it, so the fake-`/proc` pins run on every machine; only a real
#: `/proc` read ever uses it, and that is linux's.
CLK_TCK = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100


def _stat_fields(pid: int, proc_root: str) -> list[str]:
    """`/proc/<pid>/stat`'s fields from the state on (field 3 is index 0). The
    command name sits in parentheses and may hold spaces, so split after the last
    ')'."""
    text = Path(proc_root, str(pid), "stat").read_text()
    return text[text.rindex(")") + 2 :].split()


def _comm(pid: int, proc_root: str) -> str:
    text = Path(proc_root, str(pid), "stat").read_text()
    return text[text.index("(") + 1 : text.rindex(")")]


def _boot_time(proc_root: str) -> float:
    for line in Path(proc_root, "stat").read_text().splitlines():
        if line.startswith("btime "):
            return float(line.split()[1])
    raise ValueError(f"no btime line in {proc_root}/stat")


def stat_cpu(stat_text: str, hz: int) -> tuple[float, float]:
    """(user+kernel CPU seconds, start in seconds since boot) from one
    `/proc/<pid>/stat` line — this machine's, or a device's read over `adb`."""
    fields = stat_text[stat_text.rindex(")") + 2 :].split()
    utime, stime, starttime = int(fields[11]), int(fields[12]), int(fields[19])
    return (utime + stime) / hz, starttime / hz


def proc_cpu_reading(pid: int, proc_root: str = PROC_ROOT) -> tuple[float, float]:
    """(user+kernel CPU seconds, start instant in epoch seconds) of `pid`."""
    cpu_s, since_boot = stat_cpu(Path(proc_root, str(pid), "stat").read_text(), CLK_TCK)
    return cpu_s, _boot_time(proc_root) + since_boot


def parse_ps_cputime(text: str) -> float:
    """Seconds from `ps -o time=` — `[[dd-]hh:]mm:ss[.cc]` (macOS prints `m:ss.cc`)."""
    text = text.strip()
    days = 0
    if "-" in text:
        d, text = text.split("-", 1)
        days = int(d)
    secs = 0.0
    for part in text.split(":"):
        secs = secs * 60 + float(part)
    return days * 86400 + secs


def ps_cpu_reading(pid: int) -> tuple[float, float]:
    """(user+kernel CPU seconds, start instant in epoch seconds) of `pid` from
    `ps` — the darwin witness, where there is no `/proc`. `lstart` is local time,
    to the second."""
    def _ps(field: str) -> str:
        out = subprocess.run(
            ["ps", "-o", f"{field}=", "-p", str(pid)],
            capture_output=True, text=True, timeout=10,
        ).stdout.strip()
        if not out:
            raise ProcessLookupError(f"ps has no process {pid}")
        return out

    cpu_s = parse_ps_cputime(_ps("time"))
    started = time.mktime(time.strptime(_ps("lstart"), "%a %b %d %H:%M:%S %Y"))
    return cpu_s, started


#: Darwin's sampled CPU wait: one thread-state read a second through a launch gate,
#: stopped by the gate's end and, whatever else, by the ceiling (windows' bridge
#: keeps the same 600), so a sampler can never run on.
THREAD_SAMPLE_INTERVAL_S = 1.0
THREAD_SAMPLE_CEILING = 600


def ps_thread_states(pid: int) -> list[str]:
    """One `STAT` per thread of `pid`, from `ps -M`. `-M` keeps darwin's fixed table
    and appends the asked column, and only the first row carries USER and COMMAND,
    so the state is each row's LAST column. A pid `ps` does not know exits 1 with
    the header alone: that is a failed read, never a process with no runnable
    thread, so it raises."""
    answer = subprocess.run(
        ["ps", "-M", "-o", "state", "-p", str(pid)],
        capture_output=True, text=True, timeout=10,
    )
    states = [row.split()[-1] for row in answer.stdout.splitlines()[1:] if row.strip()]
    if answer.returncode != 0 or not states:
        raise ProcessLookupError(f"ps -M has no thread row for pid {pid} (exit {answer.returncode})")
    return states


def any_runnable(states: list[str]) -> bool:
    """`R` is the Mach `TH_STATE_RUNNING`: on a CPU or waiting for one. Every other
    letter is a wait for something else."""
    return any(state.startswith("R") for state in states)


class ThreadStateSampler:
    """Thread states read once a second through one launch gate — convention 6's
    launch-timeout rider, where the kernel keeps no run-queue wait. A thread in the
    driver, because darwin has no bridge process to hold the clock; a context
    manager, so it cannot outlive the gate; and `tick` catches everything, so it
    cannot raise into it.

    `pid_source` answers per tick: a gate that holds no pid (a LaunchServices `open`)
    resolves it each time, and a tick with no pid is not a sample. A read that fails
    is not a sample either, only a count and the reason."""

    def __init__(self, pid_source, reader=ps_thread_states):
        self._pid_source = pid_source
        self._reader = reader
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self.samples = self.runnable = self.failures = 0
        self.last_failure = ""

    def tick(self) -> None:
        try:
            pid = self._pid_source()
            if pid is None:
                return
            states = self._reader(pid)
        except Exception as exc:  # noqa: BLE001 - a failed read is a stated absence
            self.failures += 1
            self.last_failure = f"{type(exc).__name__}: {exc}"
            return
        self.samples += 1
        self.runnable += any_runnable(states)

    def _run(self) -> None:
        for _ in range(THREAD_SAMPLE_CEILING):
            self.tick()
            if self._stop.wait(THREAD_SAMPLE_INTERVAL_S):
                return

    def start(self) -> ThreadStateSampler:
        self._thread = threading.Thread(target=self._run, name="launch-thread-states", daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        """A read already in flight holds this up for at most `ps`'s own timeout."""
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=15)

    def __enter__(self) -> ThreadStateSampler:
        return self.start()

    def __exit__(self, *_exc) -> None:
        self.stop()

    def unread(self) -> str:
        """Why there is no reading."""
        why = self.last_failure or "no tick had a pid to read"
        return (
            "darwin keeps no run-queue wait an unprivileged reader can take (no "
            f"schedstat), and the thread-state sampler took no sample: {why}"
        )

    def wait_reading(self, cpu_s: float, cpu_wall_s: float) -> tuple[float | None, str, str]:
        """(`cpu_wait_share`, its basis, why unread). `R` covers running as well as
        runnable and the rider's wait is runnable AND NOT running, so what the app
        ran is taken back off: the `R` share less its own share of a core, floored
        at 0. Without that a blocked launch that ran 0.2 of a core counts its own
        running reads as waiting and can cross the quarter into a false STARVED."""
        if not self.samples:
            return None, "", self.unread()
        ran = cpu_s / cpu_wall_s
        share = max(0.0, self.runnable / self.samples - ran)
        failed = f"; {self.failures} read(s) failed" if self.failures else ""
        basis = (
            f"a thread runnable at {self.runnable} of {self.samples} one-a-second `ps -M` "
            f"reads, less the {ran:.2f} of a core it ran{failed}"
        )
        return share, basis, ""


def schedstat_wait_s(lines) -> float | None:
    """The longest run-queue wait among a process's threads, in seconds, from their
    `schedstat` lines (`<run ns> <wait ns> <timeslices>`; `CONFIG_SCHED_INFO`) —
    or None with no such line. The longest, not the sum: a process waits for a CPU
    while any thread does, and one thread's wait is bounded by the window where a
    sum over a dozen idle threads is not."""
    waits = []
    for line in lines:
        fields = line.split()
        if len(fields) == 3 and all(f.isdigit() for f in fields):
            waits.append(int(fields[1]))
    return None if not waits else max(waits) / 1e9


def proc_wait_reading(pid: int, proc_root: str = PROC_ROOT) -> float:
    """The longest run-queue wait among `pid`'s threads, in seconds — the kernel's
    own reading of "runnable and not running", `/proc/<pid>/task/*/schedstat`."""
    task_dir = Path(proc_root, str(pid), "task")
    lines = []
    for tid in sorted(os.listdir(task_dir)):
        try:
            lines.append((task_dir / tid / "schedstat").read_text().strip())
        except OSError:
            continue  # the thread ended since the listing
    wait_s = schedstat_wait_s(lines)
    if wait_s is None:
        raise ValueError(f"no schedstat line under {task_dir} (CONFIG_SCHED_INFO off?)")
    return wait_s


def descendant_named(root_pid: int, comm: str, proc_root: str = PROC_ROOT) -> int | None:
    """`root_pid` itself or its nearest descendant whose command name is `comm`
    (the kernel keeps 15 characters of it), or None."""
    comm = comm[:15]
    parents: dict[int, list[int]] = {}
    for entry in os.listdir(proc_root):
        if not entry.isdigit():
            continue
        try:
            ppid = int(_stat_fields(int(entry), proc_root)[1])
        except (OSError, ValueError, IndexError):
            continue  # gone since the listing, or not a process
        parents.setdefault(ppid, []).append(int(entry))
    queue = [root_pid]
    while queue:
        pid = queue.pop(0)
        try:
            if _comm(pid, proc_root) == comm:
                return pid
        except (OSError, ValueError):
            pass
        queue += sorted(parents.get(pid, []))
    return None


def _exit_phrase(name: str, rc: int) -> str:
    if rc < 0:
        return f"{name} was killed by signal {-rc}"
    return f"{name} exited with code {rc}"


def _lines(text: str) -> list[str]:
    return [ln for ln in text.splitlines() if ln.strip()]


def local_launch_witnesses(
    *,
    proc,
    comm: str,
    budget_s: float,
    posted_at: float,
    stderr_paths: tuple[str, ...],
    log_text: str,
    apps: tuple[str, ...],
    proc_root: str = PROC_ROOT,
    machine=None,
    pid: int | None = None,
    pid_unread: str = "",
    cpu_reader=None,
    wait_sampler: ThreadStateSampler | None = None,
    stderr_text: str = "",
    notes: tuple[str, ...] = (),
) -> LaunchWitnesses:
    """The four witnesses of one local launch. `proc` is the Popen-compatible
    handle the driver spawned (`poll()`, `pid`); `comm` the app's command name,
    looked for at or under it; `stderr_paths` where the launch's stderr may have
    landed, the first with content read (tui on Windows has no `app.err` of its own:
    ConPTY carries stderr into the pty stream); `log_text` the app's own rolling
    log; `machine` a
    precomputed `MachineWindow` (the default reads this machine's ledger).

    The darwin legs (`drivers/macos.py`, `drivers/ios.py`) pass `cpu_reader=
    ps_cpu_reading` and, where they hold no `Popen` (a LaunchServices `open`, a
    simulator's app), `proc=None` with the `pid` they resolved — or with
    `pid_unread` saying why there is none, which reads UNKNOWN, never a suspect.
    `wait_sampler` is the gate's `ThreadStateSampler`, their CPU wait.
    `stderr_text` is the inherited stderr already narrowed to this launch."""
    deadline_at = time.time()
    rc = proc.poll() if proc is not None else None
    alive: bool | None = rc is None
    process_unread = gone = ""
    cpu_s = cpu_wall_s = started_at = wait_share = None
    cpu_unread = wait_basis = wait_unread = ""
    if proc is None and pid is None:
        alive, process_unread = None, pid_unread or "no process handle and no pid to read"
        cpu_unread = process_unread
    elif not alive:
        cpu_unread = "the process has exited"
    elif cpu_reader is not None:
        # A reader that does not need `/proc` (darwin's `ps`): the app is the
        # handle's own process unless the caller resolved its pid.
        target = pid if pid is not None else proc.pid
        wait_unread = (
            "darwin keeps no run-queue wait an unprivileged reader can take (no schedstat), "
            "and no thread-state sampler ran for this launch"
        )
        try:
            cpu_s, started_at = cpu_reader(target)
            cpu_wall_s = deadline_at - started_at
            if cpu_wall_s and wait_sampler is not None:
                wait_share, wait_basis, wait_unread = wait_sampler.wait_reading(cpu_s, cpu_wall_s)
        except ProcessLookupError:
            # Without a handle, a pid `ps` no longer knows is the process gone.
            if proc is None:
                alive, gone = False, f"{comm} (pid {target}) is no longer running"
            cpu_unread = "the process has exited"
        except Exception as exc:  # noqa: BLE001 - an unreadable CPU is a stated absence
            cpu_unread = f"pid {target}'s CPU time could not be read ({type(exc).__name__}: {exc})"
            if alive and proc is None:
                alive, process_unread = None, cpu_unread
    elif not os.path.isdir(proc_root):
        cpu_unread = f"no {proc_root} on this platform to read the app's CPU time from"
    else:
        try:
            app_pid = descendant_named(proc.pid, comm, proc_root)
        except OSError as exc:
            app_pid, cpu_unread = None, f"{proc_root} could not be listed ({exc})"
        if app_pid is None:
            cpu_unread = cpu_unread or f"no {comm} process at or under the launch's pid {proc.pid}"
        else:
            try:
                cpu_s, started_at = proc_cpu_reading(app_pid, proc_root)
                cpu_wall_s = deadline_at - started_at
            except (OSError, ValueError, IndexError) as exc:
                cpu_unread = f"{proc_root}/{app_pid}/stat could not be read ({type(exc).__name__}: {exc})"
            if cpu_wall_s:
                try:
                    wait_s = proc_wait_reading(app_pid, proc_root)
                    wait_share = wait_s / cpu_wall_s
                    wait_basis = (
                        f"its longest-waiting thread's run-queue wait, {wait_s:.1f} s of "
                        f"{cpu_wall_s:.1f} s, from {proc_root}/{app_pid}/task/*/schedstat"
                    )
                except (OSError, ValueError) as exc:
                    wait_unread = f"{proc_root}/{app_pid}/task/*/schedstat could not be read ({exc})"
    wait_unread = wait_unread or ("" if wait_share is not None else cpu_unread)
    stderr_lines: list[str] = _lines(stderr_text)
    notes: list[str] = list(notes)
    for path in stderr_paths if not stderr_lines else ():
        try:
            stderr_lines = _lines(Path(path).read_text(errors="replace"))
        except OSError as exc:
            notes.append(f"(b) {path} could not be read: {exc}")
        if stderr_lines:
            break
    if machine is None:
        from helpers import launch_load

        machine = launch_load.machine_window(posted_at, deadline_at, apps=apps)
    return LaunchWitnesses(
        budget_s=budget_s,
        posted_at=posted_at,
        deadline_at=deadline_at,
        process_started_at=started_at,
        alive=alive,
        process_unread=process_unread,
        exit_phrase="" if alive is not False else (gone or _exit_phrase(comm, rc)),
        cpu_s=cpu_s,
        cpu_wall_s=cpu_wall_s,
        cpu_unread=cpu_unread,
        cpu_wait_share=wait_share,
        cpu_wait_basis=wait_basis,
        cpu_wait_unread=wait_unread,
        log_lines=_lines(log_text),
        stderr_lines=stderr_lines,
        machine=machine,
        notes=notes,
    )


def local_launch_timeout_report(**witness_args) -> str:
    """`launch_timeout_report` over `local_launch_witnesses(**witness_args)`.
    Never raises: it runs inside a ``raise``."""
    try:
        witnesses = local_launch_witnesses(**witness_args)
    except Exception as exc:  # noqa: BLE001 - must never mask the real failure
        return (
            f"launch timeout ({witness_args.get('budget_s', '?')} s): UNKNOWN — "
            f"the witnesses could not be gathered: {type(exc).__name__}: {exc}"
        )
    return launch_timeout_report(witnesses)


# --- Convention 6's close rider: the exit, read before the close ------------------
#
# A harness close of a local child (`terminate_tree`: SIGTERM to the group, then
# SIGKILL) ends the app however it was, so an app already dead of a crash reads
# exactly like a clean close — and once `_stop_app` has dropped the handle, what it
# died of is unreadable. `read_exit_before_close` runs at the head of every close of
# a local app (linux's, tui's and macOS's `_stop_app`, `quit_app`, `kill_uncleanly`,
# the close inside `recover()`), reads the exit, and records a crash with
# `helpers/app_deaths.py`, whose phase wrapper fails the phase that found it
# (`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention, the
# close rider). Windows' bridge-held leg is `drivers/windows.py::_read_exit_before_close`.

#: The signals a crash ends a POSIX app with: a fault (`SIGSEGV`, `SIGBUS`), a bad
#: instruction (`SIGILL`, `SIGFPE`), `SIGABRT` — a Rust `abort()`, a panic under
#: `panic = "abort"`, a double panic — and `SIGTRAP`, a trap instruction: what
#: Swift's `fatalError`, a failed precondition and a forced unwrap of nil end the
#: apple apps with on arm64 (`EXC_BREAKPOINT`). `SIGTERM`, `SIGKILL`, `SIGINT`,
#: `SIGHUP` and `SIGPIPE` are absent on purpose: they are what a kill — the
#: harness's own `terminate_tree`, `kill_uncleanly`'s group SIGKILL, a pty hang-up —
#: leaves, and a journey may end its app that way.
CRASH_SIGNALS = frozenset(
    int(getattr(signal, name))
    for name in ("SIGSEGV", "SIGABRT", "SIGBUS", "SIGILL", "SIGFPE", "SIGTRAP")
    if hasattr(signal, name)
)

#: A Rust panic's exit code under the default `panic = "unwind"` strategy, when the
#: panic reaches the main thread.
RUST_PANIC_EXIT = 101

#: The NTSTATUS endings a severity test would otherwise take for a crash:
#: `TerminateProcess(-1)` is what .NET's `Process.Kill` (and ConPTY's unknown status)
#: leaves, and CTRL_C_EXIT is a signal, not a crash.
NTSTATUS_KILLED_NOT_CRASHED = frozenset({0xFFFFFFFF, 0xC000013A})

#: How long a wrapper (`xvfb-run`) whose app is gone from under it gets to finish its
#: own cleanup and hand back the app's status. A deadline wait (convention 14): the
#: wrapper exits the moment it has stopped its Xvfb, so a close pays this only when
#: the app is already dead, and a wrapper still alive at the deadline is read as
#: unreadable, never as a crash.
WRAPPER_SETTLE_S = 5.0


def is_ntstatus_crash(raw: object) -> bool:
    """True when a windows exit code is a crash: an NTSTATUS of error severity (the
    `0xC…` codes, the CLR's unhandled-exception `0xE0434352`) or `STATUS_BREAKPOINT`,
    never a kill or a signal (`NTSTATUS_KILLED_NOT_CRASHED`). A clean quit (0) and an
    ordinary exit code (the launch fault hook's 73) are not crashes: a journey may
    end the app that way on purpose."""
    try:
        code = int(raw) & 0xFFFFFFFF
    except (TypeError, ValueError):
        return False
    if code in NTSTATUS_KILLED_NOT_CRASHED:
        return False
    return (code & 0xC0000000) == 0xC0000000 or code == 0x80000003


def _signal_name(n: int) -> str:
    try:
        return signal.Signals(n).name
    except ValueError:
        return f"signal {n}"


def crash_exit_phrase(rc: int | None, *, through: str | None = None, nt: bool | None = None) -> str:
    """How `rc` — a Popen-style return code — names a crash, or "" when it is none.

    POSIX: a negative code is the signal the process died by; `through` names the
    shell wrapper the code came through (`xvfb-run`), which reports a signal death of
    its command as `128 + n` — read as a signal only then, because the app's own
    `exit(139)` is an ordinary code. A Rust panic's 101 is a crash either way. On
    windows (`nt`) the code is an NTSTATUS (`is_ntstatus_crash`)."""
    if rc is None:
        return ""
    nt = (os.name == "nt") if nt is None else nt
    if rc == RUST_PANIC_EXIT:
        return "exited with code 101 — a Rust panic (the unwinding strategy's exit)"
    if nt:
        if is_ntstatus_crash(rc):
            return f"exited with code {rc} (0x{rc & 0xFFFFFFFF:08X}) — an NTSTATUS of error severity"
        return ""
    if rc < 0 and -rc in CRASH_SIGNALS:
        return f"was killed by {_signal_name(-rc)} (signal {-rc})"
    if through and rc > 128 and rc - 128 in CRASH_SIGNALS:
        return (
            f"was killed by {_signal_name(rc - 128)} (signal {rc - 128}), "
            f"read as exit code {rc} through {through}"
        )
    return ""


def zombie_exit_status(pid: int, proc_root: str = PROC_ROOT) -> int | None:
    """The Popen-style return code of `pid` when it is a zombie — dead, its status not
    yet collected by its parent (`xvfb-run`'s shell, between the app's death and its
    `wait`) — from `/proc/<pid>/stat`'s `exit_code` field (field 52, Linux 3.5+, the
    `waitpid` status); None for a live process or an unreadable one."""
    try:
        fields = _stat_fields(pid, proc_root)
        if fields[0] != "Z" or len(fields) < 50:
            return None
        status = int(fields[49])
    except (OSError, ValueError, IndexError):
        return None
    return -(status & 0x7F) if status & 0x7F else (status >> 8) & 0xFF


def _read_exit(proc, comm: str, wrapper: str | None, proc_root: str, settle_s: float):
    """(return code, the wrapper it came through or None) of a dead app, or None
    while it lives — or while its exit cannot be read."""
    rc = proc.poll()
    if rc is not None:
        return rc, wrapper
    if wrapper is None or not os.path.isdir(proc_root):
        return None
    try:
        app_pid = descendant_named(proc.pid, comm, proc_root)
    except OSError:
        return None
    if app_pid is not None and app_pid != proc.pid:
        status = zombie_exit_status(app_pid, proc_root)
        return None if status is None else (status, None)
    # The wrapper lives and its app is gone from under it: the app has exited and
    # the wrapper is cleaning up. Its status is the app's, once it hands it back.
    try:
        return proc.wait(timeout=settle_s), wrapper
    except subprocess.TimeoutExpired:
        return None


#: The handles whose death this module has recorded, or whose early exit a launch
#: report already diagnosed (`mark_exit_reported`): one death, one failure. A
#: `kill_uncleanly` keeps its dead handle for the next teardown, and a recorded
#: death's handle meets the teardown's read again after `recover()`'s.
_exits_reported: "weakref.WeakSet" = weakref.WeakSet()


def mark_exit_reported(proc) -> None:
    """Say that `proc`'s exit is already reported — the launch gate's `FAULTED`
    verdict carries it — so the close after it stays silent."""
    try:
        _exits_reported.add(proc)
    except TypeError:
        pass


_POST_MORTEM_RE = re.compile(r"panicked at|\[fauna\] FATAL|fatal runtime error|stack backtrace:")


def _post_mortem_lines(text: str, *, after: int = 2) -> list[str]:
    """Every panic or FATAL line of `text` with the `after` lines that follow it
    (a panic's message sits on the line after `panicked at`)."""
    lines = text.splitlines()
    keep: set[int] = set()
    for i, line in enumerate(lines):
        if _POST_MORTEM_RE.search(line):
            keep.update(range(i, min(len(lines), i + after + 1)))
    return [lines[i] for i in sorted(keep)]


def _read_text(source) -> str:
    try:
        return (source() if callable(source) else source) or ""
    except Exception:  # noqa: BLE001 - a witness that cannot be read is a stated absence
        return ""


def app_death_report(
    *,
    close: str,
    comm: str,
    phrase: str,
    launched_at: float | None,
    stderr_text: str,
    log_text: str,
    now: float | None = None,
    tail_lines: int = 40,
    crash_report: str = "",
) -> str:
    """The recorded death: the verdict first (the live-reason line carries the first
    line alone), then when it was found on the harness's clock, then the post-mortem
    — the app's panic and FATAL lines, its stderr tail, its own rolling log's tail."""
    now = time.time() if now is None else now
    found = (
        f"+{max(0.0, now - launched_at):.0f} s after its launch"
        if launched_at is not None
        else "at an unrecorded offset from its launch"
    )
    parts = [
        f"the app died before {close} closed it: {comm} {phrase}",
        f"it was found dead at this close, {found}; when it died is unread — POSIX "
        "keeps no exit instant for a child nobody waits on, and the harness reads it "
        "only at its launch gate and its closes. The close terminates the process "
        "group, so the app died on its own before it: during the last command the "
        "harness sent it (a reset, a teardown's sign-out) or after the test's last step.",
    ]
    post_mortem = _post_mortem_lines(stderr_text) + _post_mortem_lines(log_text)
    if post_mortem:
        parts.append("--- the app's panic / FATAL lines ---\n" + "\n".join(post_mortem))
    if crash_report:
        parts.append(crash_report)
    errs = _lines(stderr_text)
    if errs:
        parts.append(f"--- app stderr (last {tail_lines} lines) ---\n" + "\n".join(errs[-tail_lines:]))
    else:
        parts.append("--- app stderr: empty ---")
    logs = _lines(log_text)
    if logs:
        half = tail_lines // 2
        parts.append(f"--- on-device log (last {half} lines) ---\n" + "\n".join(logs[-half:]))
    return "\n".join(parts)


def read_exit_before_close(
    proc,
    *,
    close: str,
    comm: str,
    launched_at: float | None = None,
    wrapper: str | None = None,
    stderr_text=None,
    log_text=None,
    proc_root: str = PROC_ROOT,
    settle_s: float = WRAPPER_SETTLE_S,
    nt: bool | None = None,
    crash_report=None,
) -> None:
    """Convention 6's close rider for a local child: was the app already dead of a
    crash? Records it with `helpers/app_deaths.py` if so; the close goes on either way.

    `proc` is the Popen-compatible handle the driver holds (`poll()`, `pid`,
    `wait()`); `wrapper` the wrapper's name when the handle leads with one
    (`xvfb-run`), so a live wrapper's app is looked for under it by `comm`;
    `stderr_text` / `log_text` the post-mortem's sources, read only on a crash
    (callables or text), before the close's cleanup reclaims them; `crash_report`
    a callable of the dead pid answering the OS's crash report for it (darwin's
    `crash_report_naming`), read only on a crash too. Silent on no
    handle, a live app, a kill or signal the harness sends, a clean quit, an
    ordinary exit code, an exit already reported, and an exit it cannot read.
    Never raises: it runs at the head of every close, failure paths included."""
    if proc is None:
        return
    try:
        if proc in _exits_reported:
            return
        read = _read_exit(proc, comm, wrapper, proc_root, settle_s)
        if read is None:
            return
        rc, through = read
        phrase = crash_exit_phrase(rc, through=through, nt=nt)
        if not phrase:
            return
    except Exception:  # noqa: BLE001 - an unreadable exit is silence, never a broken close
        return
    try:
        report = app_death_report(
            close=close,
            comm=comm,
            phrase=phrase,
            launched_at=launched_at,
            stderr_text=_read_text(stderr_text),
            log_text=_read_text(log_text),
            crash_report=_read_text(lambda: crash_report(proc.pid)) if crash_report else "",
        )
    except Exception as exc:  # noqa: BLE001 - a diagnosis must never break the close
        report = (
            f"the app died before {close} closed it: {comm} {phrase} "
            f"(diagnostics unavailable: {type(exc).__name__}: {exc})"
        )
    mark_exit_reported(proc)
    from helpers import app_deaths

    app_deaths.record(report, launch=None if launched_at is None else f"{comm}@{launched_at:.3f}")


# The apple legs. A spawned macOS app has a Popen and goes through
# `read_exit_before_close`; an app the harness does not parent — the iOS simulator's
# (`simctl launch` prints a pid and owns the process), macOS's Launch Services
# launch (launchd's child) — leaves no exit code to read, only a pid that is or is
# not gone. The first rider forbids guessing, so a gone pid is a crash only on a
# witness of one: the app's own `[fauna] FATAL` or panic line since its launch, or
# the crash report darwin's ReportCrash wrote naming that pid. A gone pid with
# neither is printed as UNKNOWN, the absences stated, and fails nothing.

#: Where darwin's ReportCrash writes a crashed process's report — the simulator's
#: apps' included, which are host processes. Read as plain files only, never through
#: an API that may raise a privacy dialog: nobody answers one on an unattended box.
DIAGNOSTIC_REPORTS_DIR = Path.home() / "Library" / "Logs" / "DiagnosticReports"

#: How long a close that found its app dead waits for ReportCrash to write the
#: report: it writes asynchronously, after symbolicating. Measured 2026-11-05 on a
#: loaded macOS: an iOS simulator app's `fatalError` at 09:48:46, its report written
#: 09:48:58 — 12 s, past the 10 s this first was. A deadline poll (convention 14)
#: paid only when the app is already dead — a live app's close never waits — and a
#: report not there by the deadline is stated as absent.
CRASH_REPORT_SETTLE_S = 30.0

#: An `.ips` report older than its launch by more than this is a predecessor's: the
#: report's mtime is the write, which follows the death.
_REPORT_CLOCK_SLACK_S = 5.0


def _ips_summary(path: Path, pid: int) -> str:
    """The lines of the `.ips` report at `path` that name what killed `pid`, or ""
    when the report is not about `pid`. An `.ips` is a one-line JSON header and a
    JSON body; the body carries `pid`, `procName`, `exception`, `termination` and,
    for a Swift trap or an abort with a message, `asi` (the crash's message)."""
    import json

    text = path.read_text(errors="replace")
    _header, _, body_text = text.partition("\n")
    try:
        body = json.loads(body_text)
    except ValueError:
        return ""
    if body.get("pid") != pid:
        return ""
    lines = [f"--- crash report {path} ---", f"process: {body.get('procName')} (pid {pid})"]
    for key in ("exception", "termination", "asi"):
        if body.get(key):
            lines.append(f"{key}: {json.dumps(body[key])[:600]}")
    # Where it crashed: the faulting thread's top frames. A Swift trap's report
    # carries no message (measured 2026-11-05), so the frames are what names it.
    try:
        thread = body["threads"][body.get("faultingThread", 0)]
        symbols = [f.get("symbol") or f"<image {f.get('imageIndex')}+{f.get('imageOffset')}>"
                   for f in thread.get("frames", [])[:_REPORT_FRAMES]]
    except (KeyError, IndexError, TypeError, AttributeError):
        symbols = []
    if symbols:
        lines.append("faulting thread, top frames:")
        lines.extend(f"  {s}" for s in symbols)
    return "\n".join(lines)


#: How many of the faulting thread's frames a crash report summary quotes.
_REPORT_FRAMES = 8


def crash_report_naming(
    pid: int,
    *,
    proc_name: str,
    since: float | None,
    root: Path | None = None,
    settle_s: float | None = None,
    sleep=time.sleep,
) -> str:
    """The crash report ReportCrash wrote for `pid` of `proc_name` (its summary), or
    a line saying none named it within `settle_s`. Only `<proc_name>-*.ips` files
    newer than the launch are opened. `root` and `settle_s` default to the module's
    constants, read at the call. Never raises."""
    root = DIAGNOSTIC_REPORTS_DIR if root is None else root
    settle_s = CRASH_REPORT_SETTLE_S if settle_s is None else settle_s
    deadline = time.monotonic() + settle_s
    floor = 0.0 if since is None else since - _REPORT_CLOCK_SLACK_S
    while True:
        try:
            for path in sorted(root.glob(f"{proc_name}-*.ips")):
                try:
                    if path.stat().st_mtime < floor:
                        continue
                    found = _ips_summary(path, pid)
                except OSError:
                    continue
                if found:
                    return found
        except OSError:
            pass
        if time.monotonic() >= deadline:
            return (
                f"--- crash report: none naming pid {pid} under {root} within "
                f"{settle_s:.0f} s (ReportCrash writes one for a signal death, not for "
                "a kill or an exit) ---"
            )
        sleep(0.5)


def _report_found(text: str) -> bool:
    return text.startswith("--- crash report ") and not text.startswith("--- crash report: none")


def pid_alive(pid: int) -> bool:
    """`kill(pid, 0)`: True while `pid` names a live process (one we may not signal
    counts as live). Signals nothing."""
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


#: The (comm, pid) pairs whose gone pid this module has recorded or printed, or a
#: launch gate already diagnosed (`mark_pid_reported`): one death, one finding.
_pids_reported: set[tuple[str, int]] = set()


def mark_pid_reported(comm: str, pid: int | None) -> None:
    """Say that `pid`'s end is already reported — the launch gate's verdict carries
    it — so the close after it stays silent."""
    if pid is not None:
        _pids_reported.add((comm, pid))


def read_gone_pid_before_close(
    pid: int | None,
    *,
    close: str,
    comm: str,
    unread: str,
    launched_at: float | None = None,
    alive=pid_alive,
    stderr_text=None,
    log_text=None,
    crash_report=None,
) -> None:
    """Convention 6's close rider for an app the harness holds only a pid of: was
    the app already gone, and is there a witness that it crashed?

    `unread` says why no exit code is readable (it goes into both outcomes); the
    rest is `read_exit_before_close`'s. A crash — the pid gone, plus a FATAL or
    panic line in `stderr_text`/`log_text` or a crash report `crash_report(pid)`
    found — is recorded with `helpers/app_deaths.py`; a gone pid with neither is
    printed UNKNOWN and fails nothing. Silent on no pid, a live pid, and a pid
    already reported. Never raises: it runs at the head of every close."""
    if pid is None:
        return
    key = (comm, pid)
    try:
        if key in _pids_reported or alive(pid):
            return
    except Exception:  # noqa: BLE001 - an unreadable liveness is silence
        return
    _pids_reported.add(key)
    try:
        err, log = _read_text(stderr_text), _read_text(log_text)
        fatal = _post_mortem_lines(err) + _post_mortem_lines(log)
        report = _read_text(lambda: crash_report(pid)) if crash_report else ""
        found_report = _report_found(report)
        if not fatal and not found_report:
            print(
                f"[e2e] app exit UNKNOWN before {close}: {comm} (pid {pid}) was gone "
                f"before the close; no exit code is readable ({unread}), its log since "
                "its launch has no [fauna] FATAL or panic line, and no crash report "
                "names the pid — not failed: a gone pid alone is not read as a crash",
                flush=True,
            )
            return
        witness = " and ".join(
            w for w, have in (
                ("its crash report", found_report),
                ("a FATAL/panic line in its log", bool(fatal)),
            ) if have
        )
        phrase = (
            f"(pid {pid}) was gone before the close, a crash by {witness} — no exit "
            f"code is readable ({unread})"
        )
        text = app_death_report(
            close=close,
            comm=comm,
            phrase=phrase,
            launched_at=launched_at,
            stderr_text=err,
            log_text=log,
            crash_report=report,
        )
    except Exception as exc:  # noqa: BLE001 - a diagnosis must never break the close
        text = (
            f"the app died before {close} closed it: {comm} (pid {pid}) was gone "
            f"(diagnostics unavailable: {type(exc).__name__}: {exc})"
        )
    from helpers import app_deaths

    app_deaths.record(text, launch=None if launched_at is None else f"{comm}@{launched_at:.3f}")
