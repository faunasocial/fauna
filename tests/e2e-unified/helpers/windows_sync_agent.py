"""Which running sync agent belongs to the installed product — and whether
a foreign one is holding the per-user sync pipe.

**Why this exists.** ``App.xaml.cs``'s ``EnsureRunningAsync`` probes
``\\\\.\\pipe\\fauna-sync.<SID>`` and spawns the installed agent *only if that
probe fails*; ``CapabilityProvisioner`` then provisions over whatever answers.
The pipe is per-**user**, not per-install, so a sibling dev checkout's agent —
running as the same user out of its own build tree — answers it just as well as
the installed one. By image name the two are indistinguishable; **only the image
path separates them**, which is why every predicate here is path-scoped.

That path-scoping is the same property that keeps the MSI's own ``KillFaunaSync``
custom action from reaping a developer's build-tree agent
(``installers/windows.md``). Observation here is likewise **read-only**: another
checkout's agent is never ours to stop.

**The trap this encodes** (measured on Windows, 2026-07-17): the agent is spawned
*detached* so it survives the app exiting — which means it also outlives the
developer session that started it. A sibling dev checkout's agent was still
serving the pipe hours after the shell that spawned it had gone, so "the other
terminal is closed" is **not** evidence the box is clear. Only the pipe and the
process list are.
"""

from __future__ import annotations

import os
import re
import subprocess
from contextlib import contextmanager
from pathlib import Path

from . import sync_agent_ipc as ipc
from drivers.port_util import popen_group_kwargs, reap_descendants_of

#: The per-user sync pipe's stable prefix. The full name carries the user's SID
#: (``fauna-sync.S-1-5-21-…``), so the prefix is what identifies the family.
#: Deliberately NOT renamed alongside the A5 binary rename — the pipe name is the
#: app↔agent wire within a major version (``sync-agent.md``), a separate axis from
#: the artifact name.
_PIPE_PREFIX = "fauna-sync."

_PIPE_FS = r"\\.\pipe"

#: Every image name a Fauna sync agent can run under: ``fauna-sync-agent.exe``, the
#: shipped artifact. The pre-A5 name ``fauna-sync.exe`` is deliberately NOT matched —
#: the compat-remnant sweep removed it (no pre-rename install exists), and until then
#: it was also the unrelated ``bins/fauna-sync`` CLI daemon, so matching it flagged
#: the wrong process. A foreign agent is one this checkout did not build, still
#: running the shipped image name from another path.
_AGENT_IMAGE_NAMES = ("fauna-sync-agent.exe",)


def _norm(path: str) -> str:
    """Normalize for comparison: absolute, OS-cased, OS-separated.

    ``normcase`` both lowercases and folds ``/`` → ``\\`` on Windows, so a path
    Win32 hands back in any case or separator style compares equal to ours.
    """
    return os.path.normcase(os.path.abspath(path))


def foreign_agents(paths, install_dir):
    """Those of ``paths`` that are NOT the installed product's agent.

    The containment test is on a **path component boundary**, not a raw string
    prefix: ``C:\\Program Files\\Fauna-old\\fauna-sync.exe`` shares a prefix with
    ``C:\\Program Files\\Fauna`` but is a different product, and calling it ours
    would let exactly the intruder this guard exists to catch through.

    Order is preserved and the ORIGINAL (un-normalized) strings are returned —
    a diagnosis must quote the path the operator will actually see in the
    process list, not our lowercased rewrite of it.
    """
    root = _norm(install_dir)
    prefix = root + os.sep
    out = []
    for path in paths:
        normalized = _norm(path)
        if normalized == root or normalized.startswith(prefix):
            continue
        out.append(path)
    return out


def installed_agents(paths, install_dir):
    """The complement of :func:`foreign_agents` — those that ARE the product's."""
    foreign = set(foreign_agents(paths, install_dir))
    return [p for p in paths if p not in foreign]


def sync_pipe_is_served():
    """True if any per-user fauna-sync pipe is currently listening.

    Enumerating the named-pipe filesystem is the only read that answers "is
    *something* serving?" without connecting (a connect would be a side effect,
    and could itself be answered by the very agent we are trying to detect).
    Never raises: this runs ahead of a destructive install and must not be the
    thing that explodes.
    """
    try:
        return any(n.startswith(_PIPE_PREFIX) for n in os.listdir(_PIPE_FS))
    except OSError:
        return False


def running_sync_agent_paths():
    """Image paths of every running sync agent on this box, under any of
    :data:`_AGENT_IMAGE_NAMES`.

    OBSERVE ONLY — never kill. Sibling dev sessions run their own agent out of
    their own build tree, and killing another session's work is never ours to do.

    Read in-process (a Toolhelp snapshot + ``QueryFullProcessImageNameW``), never
    through a PowerShell/WMI round trip — the same move, for the same reason, as
    :func:`process_command_line`. The WMI form blew its 30 s bound in the
    installer journey's setup on a busy Windows (measured 2026-09-27), turning the
    foreign-agent guard itself into a red. A
    process whose path cannot be read (gone, or another user's) is skipped: it
    cannot be serving THIS user's per-SID pipe.
    """
    import ctypes
    import ctypes.wintypes as wt

    class PROCESSENTRY32W(ctypes.Structure):
        _fields_ = [
            ("dwSize", wt.DWORD), ("cntUsage", wt.DWORD),
            ("th32ProcessID", wt.DWORD), ("th32DefaultHeapID", ctypes.c_size_t),
            ("th32ModuleID", wt.DWORD), ("cntThreads", wt.DWORD),
            ("th32ParentProcessID", wt.DWORD), ("pcPriClassBase", ctypes.c_long),
            ("dwFlags", wt.DWORD), ("szExeFile", wt.WCHAR * 260),
        ]

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateToolhelp32Snapshot.restype = wt.HANDLE
    kernel32.CreateToolhelp32Snapshot.argtypes = [wt.DWORD, wt.DWORD]
    kernel32.Process32FirstW.argtypes = [wt.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.Process32NextW.argtypes = [wt.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.OpenProcess.restype = wt.HANDLE
    kernel32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
    kernel32.QueryFullProcessImageNameW.argtypes = [
        wt.HANDLE, wt.DWORD, wt.LPWSTR, ctypes.POINTER(wt.DWORD)]
    kernel32.CloseHandle.argtypes = [wt.HANDLE]

    wanted = {name.lower() for name in _AGENT_IMAGE_NAMES}
    snap = kernel32.CreateToolhelp32Snapshot(0x2, 0)  # TH32CS_SNAPPROCESS
    if snap in (None, wt.HANDLE(-1).value):
        raise OSError(ctypes.get_last_error(), "CreateToolhelp32Snapshot failed")
    pids = []
    try:
        entry = PROCESSENTRY32W()
        entry.dwSize = ctypes.sizeof(PROCESSENTRY32W)
        ok = kernel32.Process32FirstW(snap, ctypes.byref(entry))
        while ok:
            if entry.szExeFile.lower() in wanted:
                pids.append(entry.th32ProcessID)
            ok = kernel32.Process32NextW(snap, ctypes.byref(entry))
    finally:
        kernel32.CloseHandle(snap)

    paths = []
    for pid in pids:
        handle = kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
        if not handle:
            continue
        try:
            buf = ctypes.create_unicode_buffer(32768)
            size = wt.DWORD(len(buf))
            if kernel32.QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size)):
                paths.append(buf.value)
        finally:
            kernel32.CloseHandle(handle)
    return paths


def describe_blocker(paths, install_dir):
    """The operator-facing diagnosis, or ``None`` when ``paths`` are all ours.

    Pure: takes the process list rather than reading it, so the message itself
    is provable without a live agent.
    """
    foreign = foreign_agents(paths, install_dir)
    if not foreign:
        return None
    listed = "\n".join(f"    {p}" for p in foreign)
    return (
        "a foreign sync agent is serving the per-user sync pipe "
        f"({_PIPE_FS}\\{_PIPE_PREFIX}<SID>):\n"
        f"{listed}\n"
        "\n"
        "This suite tests the INSTALLED product, but the app probes that pipe "
        "and, finding it answered, will NOT spawn the installed agent "
        "(App.xaml.cs EnsureRunningAsync). This run would therefore assert "
        "against — and provision + bind INTO — the foreign agent above, "
        "corrupting whatever session owns it while reporting a red that looks "
        "exactly like an installer->agent product defect.\n"
        "\n"
        "To clear it: stop that process BY PID (never by image name — that "
        "would reap a sibling's agent too). NOTE: the agent is spawned "
        "detached, so closing the terminal session that started it does NOT "
        "stop it; check the process list, not the session list."
    )


def blocking_diagnosis(install_dir):
    """Read the box and diagnose it: ``None`` means clear to run.

    Both halves must hold to block — a foreign agent that is *not* serving the
    pipe is harmless (the app will spawn ours), and failing on it would abort
    good runs for no reason.
    """
    if not sync_pipe_is_served():
        return None
    return describe_blocker(running_sync_agent_paths(), install_dir)


# ---------------------------------------------------------------------------
# The one real-agent spawner every windows real-agent e2e shares.
# ---------------------------------------------------------------------------
#
# Direct-IPC tests (test_per_user_sync_agent.py) and the UI-driven
# `isolated_sync_agent` harness (test_sync_live_apply.py) both need OUR OWN
# fauna-sync-agent.exe running on a test-chosen pipe before they touch it —
# this is that one spawner — never a second one.
#
# "The pipe answers" is NOT "our agent answers it": an agent spawned onto a pipe
# another agent already serves exits as a duplicate (`service.rs`, the per-pipe
# `Local\FaunaSyncAgent.<leaf>` mutex) while the pipe goes on answering. Every
# windows launch spawns its own isolated agent on the SESSION pipe (convention
# 10, windows axis (a)), so a test that spawns after the launch's agent is up
# gets the launch's agent, not its own. `running_agent` therefore asks the OS
# which process serves the pipe and refuses legibly when it is not ours;
# `serving_agent` is the form for a test that only needs SOME agent keeping its
# state in a known dir to serve the pipe.

_DATA_DIR_ARG = re.compile(r'--data-dir(?:=|\s+)(?:"([^"]*)"|(\S+))')


class AgentNotServingError(AssertionError):
    """The pipe is served, but not by the agent the caller spawned (or expects).

    ``server_pid`` / ``server_command_line`` name the process that does serve it,
    and ``server_data_dir`` the ``--data-dir`` it runs on (``None`` when its
    command line names none — the box's own layout).
    """

    def __init__(self, message, *, server_pid, server_command_line):
        super().__init__(message)
        self.server_pid = server_pid
        self.server_command_line = server_command_line
        self.server_data_dir = data_dir_of(server_command_line)


def data_dir_of(command_line):
    """The ``--data-dir`` an agent command line names, or ``None``.

    Pure. Handles both spellings an agent is launched with: the harness's
    ``[..., "--data-dir", dir]`` argv (quoted by Windows when the dir holds a
    space) and a ``--data-dir=dir`` form.
    """
    match = _DATA_DIR_ARG.search(command_line or "")
    if not match:
        return None
    return match.group(1) if match.group(1) is not None else match.group(2)


def same_dir(a, b) -> bool:
    """Do two spellings of a directory name the same one? Pure."""
    return a is not None and b is not None and _norm(str(a)) == _norm(str(b))


_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_PROCESS_COMMAND_LINE_INFORMATION = 60  # PROCESSINFOCLASS, Windows 8.1+
_STATUS_INFO_LENGTH_MISMATCH = 0xC0000004


def process_command_line(pid) -> str:
    """The command line of process ``pid`` ("" when it is gone). OBSERVE ONLY.

    Read in-process (``NtQueryInformationProcess``), never through a
    PowerShell/WMI round trip: on a saturated box ``Get-CimInstance`` took over
    30 s to answer (measured 2026-09-26), turning a legible refusal into a
    ``TimeoutExpired`` from inside the diagnosis.
    """
    import ctypes
    import ctypes.wintypes as wt

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    ntdll = ctypes.WinDLL("ntdll")
    kernel32.OpenProcess.restype = wt.HANDLE
    kernel32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
    kernel32.CloseHandle.argtypes = [wt.HANDLE]
    ntdll.NtQueryInformationProcess.restype = ctypes.c_ulong
    ntdll.NtQueryInformationProcess.argtypes = [
        wt.HANDLE, ctypes.c_int, ctypes.c_void_p, wt.ULONG, ctypes.POINTER(wt.ULONG),
    ]

    class _UnicodeString(ctypes.Structure):
        _fields_ = [("Length", wt.USHORT), ("MaximumLength", wt.USHORT),
                    ("Buffer", ctypes.c_void_p)]

    handle = kernel32.OpenProcess(_PROCESS_QUERY_LIMITED_INFORMATION, False, int(pid))
    if not handle:
        return ""
    try:
        needed = wt.ULONG(0)
        status = ntdll.NtQueryInformationProcess(
            handle, _PROCESS_COMMAND_LINE_INFORMATION, None, 0, ctypes.byref(needed)
        )
        if status != _STATUS_INFO_LENGTH_MISMATCH or not needed.value:
            return ""
        buf = ctypes.create_string_buffer(needed.value)
        status = ntdll.NtQueryInformationProcess(
            handle, _PROCESS_COMMAND_LINE_INFORMATION, buf, needed, ctypes.byref(needed)
        )
        if status != 0:
            return ""
        text = _UnicodeString.from_buffer(buf)
        if not text.Buffer or not text.Length:
            return ""
        return ctypes.wstring_at(text.Buffer, text.Length // 2)
    finally:
        kernel32.CloseHandle(handle)


def describe_foreign_server(pipe_path, *, server_pid, server_command_line,
                            expected, log_text=""):
    """Why the agent serving ``pipe_path`` is not the one the caller expected.

    Pure. ``expected`` says what the caller wanted ("process 123 on --data-dir X",
    "an agent on --data-dir X"); ``log_text`` is the caller's own spawn's output,
    which for a duplicate names the mutex it lost on.
    """
    served_dir = data_dir_of(server_command_line)
    lines = [
        f"{pipe_path} is served by process {server_pid}, not by {expected}.",
        f"  serving process's --data-dir: {served_dir or '<none named: the default layout>'}",
        f"  serving process's command line: {server_command_line or '<gone>'}",
        "An agent spawned onto a pipe another agent already serves exits as a "
        "duplicate while the pipe goes on answering, so everything this test "
        "reads from its own agent's dir would be empty. On the windows e2e "
        "session pipe the other agent is usually the LAUNCH's own (convention "
        "10): use `serving_agent` with the launch's `--data-dir` "
        "(`driver.sync_agent_state_base`) instead of spawning a second agent.",
    ]
    tail = (log_text or "").strip()
    if tail:
        lines.append("  this spawn's own output (tail):")
        lines.extend(f"    {line}" for line in tail.splitlines()[-6:])
    return "\n".join(lines)


def _spawn(exe, pipe_path, data_dir, log_path):
    """Popen an agent with a private credential store; returns ``(proc, log)``."""
    cred_dir = Path(data_dir) / "credentials"
    cred_dir.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env["FAUNA_E2E_CREDENTIAL_DIR"] = str(cred_dir)
    log = open(log_path, "w")
    # The sync agent is the textbook point-9 case: it is BUILT to outlive the
    # process that starts it, so an unarmed spawn leaves it holding this run's
    # named pipe and data dir against the next launch — the contamination
    # `reap_descendants_of` was written for. Both halves armed 2026-08-14.
    proc = subprocess.Popen(
        [exe, "--foreground", "--pipe-name", pipe_path, "--data-dir", str(data_dir)],
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    return proc, log


def _stop(proc, log):
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    log.close()


def _refusal(pipe_path, server_pid, expected, log_path=None):
    command_line = process_command_line(server_pid)
    log_text = ""
    if log_path is not None:
        try:
            log_text = Path(log_path).read_text(errors="replace")
        except OSError:
            pass
    return AgentNotServingError(
        describe_foreign_server(
            pipe_path, server_pid=server_pid, server_command_line=command_line,
            expected=expected, log_text=log_text,
        ),
        server_pid=server_pid,
        server_command_line=command_line,
    )


@contextmanager
def running_agent(exe, pipe_path, data_dir, log_path):
    """Popen a fauna-sync agent, confirm IT serves ``pipe_path``, and guarantee
    teardown in finally.

    Raises :class:`AgentNotServingError` (after stopping its own spawn) when the
    pipe answers but another process serves it — see the block comment above.

    The spawned agent gets a PRIVATE credential store (``FAUNA_E2E_CREDENTIAL_DIR``
    under ``data_dir``), never the box's real credential manager. The agent's
    store is env-routed, NOT data-dir-routed, so a fresh ``--data-dir`` alone
    does not isolate it: an agent spawned with the bare pytest env restores the
    INSTALLED product's persisted capability (a real-nest URL — the engine then
    fights 401s against the production nest instead of serving the test's), and
    every provision/unprovision the test drives writes into — or deletes from —
    the real ``fauna-sync-agent`` credman record (found live 2026-07-22: the
    disk-delete media test's agent booted with ``nest_url=https://example.com``).
    Deriving the dir from ``data_dir`` keeps it stable across a kill+respawn on
    the same data dir, so restore-from-persisted-capability tests still work.

    Args:
        exe: Path to fauna-sync-agent.exe (str).
        pipe_path: Full Win32 pipe path, e.g. ``r'\\\\.\\pipe\\fauna-sync-test-a'``.
        data_dir: Path object for the agent's --data-dir (fresh per test).
        log_path: Path object where stdout+stderr are captured.
    """
    proc, log = _spawn(exe, pipe_path, data_dir, log_path)
    try:
        ipc.wait_for_pipe(pipe_path, timeout=20)
        server_pid = ipc.pipe_server_pid(pipe_path, timeout=20)
        if server_pid != proc.pid:
            log.flush()
            raise _refusal(
                pipe_path, server_pid,
                f"the agent this spawn started (process {proc.pid} on --data-dir {data_dir})",
                log_path,
            )
        yield proc
    finally:
        _stop(proc, log)


@contextmanager
def serving_agent(exe, pipe_path, data_dir, log_path):
    """An agent keeping its state in ``data_dir`` serves ``pipe_path`` for the
    ``with`` body — spawned here only if nothing serves the pipe yet.

    For a test that needs the agent up before it acts but does not care WHOSE it
    is, only where its state lives: on the windows e2e session pipe that is the
    launch's own agent (``driver.sync_agent_state_base``), which the app may
    already have spawned — or may spawn in the instant between the probe here and
    our own spawn. Either way an agent on ``data_dir`` serving the pipe is
    adopted (yields ``None``; not ours to stop). One on any other dir raises
    :class:`AgentNotServingError`. Nothing served → spawns via the same path as
    :func:`running_agent` and yields its process, stopped on exit.
    """
    try:
        ipc.wait_for_pipe(pipe_path, timeout=0.3)
        served = True
    except TimeoutError:
        served = False

    if not served:
        proc, log = _spawn(exe, pipe_path, data_dir, log_path)
        try:
            ipc.wait_for_pipe(pipe_path, timeout=20)
            server_pid = ipc.pipe_server_pid(pipe_path, timeout=20)
        except BaseException:
            _stop(proc, log)
            raise
        if server_pid == proc.pid:
            try:
                yield proc
            finally:
                _stop(proc, log)
            return
        # Lost the race to another spawner; fall through and judge the winner.
        _stop(proc, log)
    else:
        server_pid = ipc.pipe_server_pid(pipe_path, timeout=20)

    if not same_dir(data_dir_of(process_command_line(server_pid)), data_dir):
        raise _refusal(
            pipe_path, server_pid, f"an agent on --data-dir {data_dir}",
            None if served else log_path,
        )
    yield None


# ---------------------------------------------------------------------------
# Stopping the launch's own agent — through its own verb, never by name
# ---------------------------------------------------------------------------
#
# On the windows e2e session pipe the agent is usually spawned by the APP
# (`SpawnSyncAgentDetached`, or tui's shared convergence tick), so the harness
# holds no handle to it and `_stop` — which stops only an agent the harness
# spawned — does not apply. The agent's own `Shutdown` IPC request is the one
# stop that needs no handle and kills nothing by name (the e2e harness's
# process-safety rule: sessions share the box): it asks the agent to exit, and the app's
# convergence tick (`fauna_ipc::convergence::DEFAULT_TICK_INTERVAL`, 30 s)
# respawns it on the same pins the launch set.

_SYNCHRONIZE = 0x00100000
_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_STILL_ACTIVE = 259


def process_has_exited(pid: int) -> bool:
    """Whether ``pid`` has exited, asked through ``OpenProcess`` +
    ``GetExitCodeProcess`` — never ``os.kill(pid, 0)``, which on windows maps to
    ``TerminateProcess`` (or, for signal 0, a console-wide CTRL_C) and kills the
    process it asks about. A pid the OS no longer knows reads as exited."""
    import ctypes
    import ctypes.wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenProcess.restype = ctypes.wintypes.HANDLE
    k32.OpenProcess.argtypes = [
        ctypes.wintypes.DWORD, ctypes.wintypes.BOOL, ctypes.wintypes.DWORD,
    ]
    k32.GetExitCodeProcess.argtypes = [
        ctypes.wintypes.HANDLE, ctypes.POINTER(ctypes.wintypes.DWORD),
    ]
    k32.CloseHandle.argtypes = [ctypes.wintypes.HANDLE]
    handle = k32.OpenProcess(_SYNCHRONIZE | _PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return True
    try:
        code = ctypes.wintypes.DWORD()
        if not k32.GetExitCodeProcess(handle, ctypes.byref(code)):
            raise OSError(
                f"GetExitCodeProcess({pid}) failed: Win32 error {ctypes.get_last_error()}"
            )
        return code.value != _STILL_ACTIVE
    finally:
        k32.CloseHandle(handle)


def request(pipe_path: str, req: dict, *, timeout: float = 20.0) -> object:
    """Send one request to the agent on ``pipe_path`` and return its ``Ok``
    payload, retrying a busy open inside ``timeout``.

    The agent's accept loop mints each next server instance asynchronously, so an
    open that races it — right after :func:`sync_agent_ipc.pipe_server_pid` took an
    instance for an instant, or beside the app's own status polls — finds the pipe
    busy (measured: the first live on-demand floor run failed exactly there).
    ``send_request`` itself has no busy retry."""
    import time

    frame = ipc.encode_frame(req)
    deadline = time.monotonic() + timeout
    while True:
        try:
            ipc.wait_for_pipe(pipe_path, timeout=max(0.1, deadline - time.monotonic()))
            return ipc.unwrap_ok(ipc.send_request(pipe_path, frame))
        except (OSError, TimeoutError):
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.1)  # sleep-ok: poll cadence of a deadline poll, not a settle wait


def shutdown_agent(pipe_path: str, *, timeout: float = 60.0) -> int:
    """Stop the agent serving ``pipe_path`` through its own ``Shutdown`` request
    and wait until its PROCESS has exited — not merely until the pipe stops
    answering: the per-engine cfapi connections close as the process tears down,
    and a caller staging a disconnected root needs them closed. Returns the pid
    that exited.
    """
    import time

    pid = ipc.pipe_server_pid(pipe_path, timeout=20)
    request(pipe_path, {"id": 1, "method": "Shutdown"})
    deadline = time.monotonic() + timeout
    while not process_has_exited(pid):
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"the agent (pid {pid}) serving {pipe_path} accepted Shutdown but was "
                f"still running {timeout:.0f}s later"
            )
        time.sleep(0.2)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    return pid


def file_status(pipe_path: str, abs_path: str) -> str:
    """The agent's ``GetFileStatus`` verdict for ``abs_path`` — ``"Synced"``,
    ``"CloudOnly"``, ``"Syncing"``, ``"Error"`` or ``"NotTracked"``: the engine's
    OWN row state (a hydrated file is ``Synced``, a placeholder ``CloudOnly``),
    which is what the mass-delete floor counts — not the OS's attributes."""
    payload = request(pipe_path, {"id": 1, "method": {"GetFileStatus": {"path": abs_path}}})
    return payload["FileStatus"]["status"]
