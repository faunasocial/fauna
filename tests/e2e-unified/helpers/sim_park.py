"""The simulator park: a verify drain's iOS seat devices kept booted across
its pytest processes (mac-simulator-hold.md § Per-core tokens, *The e2e lane's
simulator boot*, the park sentence; § Implementation status today, *The
park*).

Without a park, every pytest process clones its own `fauna-e2e-ios-<pid>-<seat>`
devices from the golden device, boots them under a `sim_boot` grant and deletes
them at exit, so a drain turn of N iOS requests (each a warm step, a shard and
its flaky re-runs, each a process of its own) paid each seat's boot — a mean
578 core-s over its first 180 s, the clone's figure — once a process.

With the drain's park open (`PARK_ENV` names its directory, internal wiring the
drain sets, never a knob), a process's exit hands each device it made BACK,
still booted, after a reset (`reset`: the Fauna apps terminated and
uninstalled, the keychain and the privacy grants reset — the per-launch
uninstall+install stays the isolation, as it is between two tests of one
process), and the next process's seat LEASES a parked device of its runtime and
device type instead of cloning and booting one. The drain closes the park at
the end of its turns (`close`: every parked device shut down and deleted).

Lifetime and the orphan reclaim: a parked device is renamed
`fauna-e2e-ios-<owner pid>-<n>`, the owner being the drain's own process, so the
session-startup orphan reclaim (which keys on the creator pid in the
name) keeps it while the drain lives and
takes it if the drain dies unclosed. A device whose process was killed is never
parked (its `atexit` never ran) and stays named after that dead pid — Job C's,
as before. A leased device is renamed after its new process at once.

The File Provider seats (`fp_device`, `conftest.create_ios_fp_device`) are never
parked: each test needs a device whose domain was never registered.

The park's grant: a parked device sits booted between two processes, after
its last process's own `sim_idle` grant went and before the next process leases
it. `open` starts a holder (`hold`, a process of its own) that takes ONE
`sim_idle` grant at the first parked device and holds it to the park's close,
its note naming every device the park holds (`sims`); `close` deletes the
devices under it, then ends the owner and waits for the holder to release
(mac-simulator-hold.md § Per-core tokens, *A booted device is always under a
grant*).

Usage from the drain (`delayed_e2e_drain`):

    python sim_park.py open <dir> --owner-pid <pid>   # starts `hold <dir>`
    python sim_park.py close <dir>

Stdlib only; every `simctl` call goes through an injectable `run` so the tier_1
pins (`tests/e2e-unified/tests/test_sim_park.py`) drive it with a fake.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Callable, Iterable

#: The environment variable naming the open park's directory. Set by the
#: drain for its turns' pytest processes and by nothing else.
PARK_ENV = "FAUNA_E2E_SIM_PARK"

#: The device-name prefix of the e2e family (Job C's `PREFIXES` lists it).
DEVICE_PREFIX = "fauna-e2e-ios-"

_OWNER_FILE = "owner"
_HOLDER_FILE = "holder"
_RECORD_SUFFIX = ".json"
_LEASING = ".leasing-"

#: How often the park's grant holder (`hold`) re-reads the park, and the
#: step of `close`'s wait for it to end.
HOLD_POLL_S = 1.0
#: One pass's bounded wait for the `sim_idle` grant; the next pass asks again.
HOLD_ACQUIRE_WAIT_S = 60.0
#: How long `close` waits for the holder to release before removing the park.
HOLDER_EXIT_WAIT_S = 30.0

Run = Callable[..., "subprocess.CompletedProcess[str]"]


def _run(cmd: list[str], timeout: float = 300) -> "subprocess.CompletedProcess[str]":
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as exc:
        return subprocess.CompletedProcess(cmd, 1, "", str(exc))


def park_dir(env: dict | None = None) -> Path | None:
    """The open park's directory, or None when no park is open (every run
    outside the drain: a session's own pytest deletes its devices as before)."""
    value = (os.environ if env is None else env).get(PARK_ENV, "").strip()
    return Path(value) if value else None


def _pid_alive(pid: int) -> bool:
    """True while `pid` names a live process; signals nothing on any OS.

    The park runs on macOS alone, but its tier_1 pins run on every machine, and
    on Windows `os.kill(pid, 0)` is `GenerateConsoleCtrlEvent(CTRL_C_EVENT, …)`:
    it Ctrl-C'd the very pytest that asked and ended a Windows tier_1 run
    (2026-11-18). There the probe is `OpenProcess` + `GetExitCodeProcess`, the
    build-slot tool's own signal-free shape; a process we may not open still
    exists. NEVER put `os.kill` back on the Windows path."""
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.OpenProcess.restype = wintypes.HANDLE
        k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        k32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        k32.CloseHandle.argtypes = [wintypes.HANDLE]
        handle = k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not handle:
            return ctypes.get_last_error() == 5  # ERROR_ACCESS_DENIED: it exists
        try:
            code = wintypes.DWORD()
            if not k32.GetExitCodeProcess(handle, ctypes.byref(code)):
                return True
            return code.value == 259  # STILL_ACTIVE
        finally:
            k32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


def owner_pid(directory: Path) -> int | None:
    """The park owner's pid, or None when the park is not open (no owner
    file) or its owner has exited — a park nobody will close takes nothing."""
    try:
        pid = int((directory / _OWNER_FILE).read_text().strip())
    except (OSError, ValueError):
        return None
    return pid if _pid_alive(pid) else None


def _booted(run: Run) -> set[str] | None:
    res = run(["xcrun", "simctl", "list", "devices", "-j"], timeout=60)
    if res.returncode != 0:
        return None
    try:
        devices = json.loads(res.stdout)["devices"]
    except (ValueError, KeyError, TypeError):
        return None
    return {
        str(d.get("udid", "")).upper()
        for devs in devices.values() for d in devs
        if d.get("state") == "Booted"
    }


def _delete(udid: str, run: Run) -> None:
    run(["xcrun", "simctl", "shutdown", udid])
    run(["xcrun", "simctl", "delete", udid])


def _records(directory: Path) -> list[Path]:
    try:
        return sorted(p for p in directory.iterdir()
                      if p.name.endswith(_RECORD_SUFFIX) and _LEASING not in p.name)
    except OSError:
        return []


def reset(udid: str, bundle_ids: Iterable[str], run: Run = _run) -> None:
    """What a parked device carries of its last process, removed: every Fauna
    app terminated and uninstalled (its data and app-group containers with
    it), the keychain reset and every privacy grant reset. Best-effort — the
    next launch's uninstall+install is the isolation either way."""
    for bundle in bundle_ids:
        run(["xcrun", "simctl", "terminate", udid, bundle], timeout=60)
        run(["xcrun", "simctl", "uninstall", udid, bundle], timeout=120)
    run(["xcrun", "simctl", "keychain", udid, "reset"], timeout=60)
    run(["xcrun", "simctl", "privacy", udid, "reset", "all"], timeout=60)


def park(
    directory: Path, udid: str, runtime: str, devicetype: str,
    bundle_ids: Iterable[str], run: Run = _run,
) -> bool:
    """Hand `udid` back to the park, still booted, after `reset` → True; False
    when the park is not open or the device is not booted (the caller deletes
    it, as without a park)."""
    owner = owner_pid(directory)
    if owner is None:
        return False
    booted = _booted(run)
    if booted is None or udid.upper() not in booted:
        return False
    reset(udid, bundle_ids, run)
    seq = len(_records(directory)) + 1
    if run(["xcrun", "simctl", "rename", udid, f"{DEVICE_PREFIX}{owner}-{seq}"],
           timeout=60).returncode != 0:
        return False
    record = {"udid": udid, "runtime": runtime, "devicetype": devicetype,
              "parked_at": time.time()}
    tmp = directory / f".{udid}.tmp"
    try:
        tmp.write_text(json.dumps(record))
        os.replace(tmp, directory / f"{udid}{_RECORD_SUFFIX}")
    except OSError:
        return False
    return True


def lease(
    directory: Path, runtime: str, devicetype: str, name: str, run: Run = _run,
) -> str | None:
    """A parked, still-booted device of `runtime` and `devicetype`, renamed
    `name` → its UDID; None when the park holds none (the caller clones).
    The claim is an atomic rename of the record, so two processes never take
    one device; a parked device found no longer booted is deleted."""
    if owner_pid(directory) is None:
        return None
    for rec in _records(directory):
        claimed = rec.with_name(f"{rec.name}{_LEASING}{os.getpid()}")
        try:
            os.rename(rec, claimed)
        except OSError:
            continue  # another process took it
        try:
            data = json.loads(claimed.read_text())
        except (OSError, ValueError):
            claimed.unlink(missing_ok=True)
            continue
        udid = str(data.get("udid", ""))
        if data.get("runtime") != runtime or data.get("devicetype") != devicetype:
            os.rename(claimed, rec)  # another pin's device: leave it parked
            continue
        claimed.unlink(missing_ok=True)
        booted = _booted(run)
        if not udid or booted is None or udid.upper() not in booted:
            if udid:
                _delete(udid, run)
            continue
        if run(["xcrun", "simctl", "rename", udid, name], timeout=60).returncode != 0:
            _delete(udid, run)
            continue
        return udid
    return None


def parked_udids(directory: Path) -> list[str]:
    """The UDIDs of every device the park holds booted: parked, or mid-lease
    (its record claimed by a process, live or not, that has not yet taken it
    — a live lessee's own grant names it from its registration, and until
    then the park's still does)."""
    out: list[str] = []
    try:
        entries = sorted(directory.iterdir())
    except OSError:
        return out
    for p in entries:
        base = p.name.split(_LEASING, 1)[0]
        if not base.endswith(_RECORD_SUFFIX) or base.startswith("."):
            continue
        try:
            udid = str(json.loads(p.read_text()).get("udid", ""))
        except (OSError, ValueError):
            continue
        if udid:
            out.append(udid)
    return out


def _holder_pid(directory: Path) -> int | None:
    try:
        return int((directory / _HOLDER_FILE).read_text().strip())
    except (OSError, ValueError):
        return None


def _take_grant(slots, log: Callable[..., None]):
    """One `sim_idle` grant for the park → its fd; None when this pass got
    none (the next pass asks again); `False` when none will ever come here
    (no such pool on this machine, or an ancestor's grant already holds this
    tree's devices)."""
    pool = getattr(slots, "SIM_IDLE_POOL", None)
    enabled = getattr(slots, "sim_idle_enabled", None)
    if pool is None or enabled is None or not enabled():
        return False
    try:
        fd = slots.acquire_slot(pool, cmd=["xcrun", "simctl", "(the verify drain's parked devices)"],
                                timeout_s=HOLD_ACQUIRE_WAIT_S)
    except TimeoutError as exc:
        log(f"sim-park: no room for a '{pool}' grant after {HOLD_ACQUIRE_WAIT_S:.0f}s — "
            f"the parked devices stay unnamed until the next pass asks again ({exc})")
        return None
    return False if fd is None else fd


def _held(fd) -> bool:
    return fd is not None and fd is not False


def hold(
    directory: Path, slots, *, step: Callable[[float], None] = time.sleep,
    log: Callable[..., None] = print,
) -> int:
    """The park's grant (mac-simulator-hold.md § Per-core tokens, *A booted
    device is always under a grant*): from the first parked device to the
    park's close, ONE `sim_idle` grant whose note names every device the park
    holds (`sims`, `parked_udids`), re-noted each pass when the set changes.

    A device the park holds sits booted between two pytest processes of a
    drain turn, after its last process's own grant went at that process's
    exit and before the next process leases it — under no grant, its load
    read as unslotted, until this. The same one-grant shape as the conftest's
    kept devices (`_hold_simulators_idle`): a leased device is named by its
    lessee's grant as well while the record lasts, the figures summing.

    Ends when the owner has closed the park or exited (`owner_pid` None) or
    another holder has replaced this one (the holder file names another
    pid), releasing the grant last — `close` deletes the devices first, so
    their shutdown runs under it."""
    fd = None
    noted: list[str] | None = None
    pool = getattr(slots, "SIM_IDLE_POOL", None)
    try:
        # A holder file not yet written (the opener writes it once this
        # process is started) is not another holder's.
        while owner_pid(directory) is not None and _holder_pid(directory) in (None, os.getpid()):
            sims = sorted(parked_udids(directory))
            if fd is None and sims:
                fd = _take_grant(slots, log)
            if _held(fd) and sims != noted:
                annotate = getattr(slots, "annotate_note", None)
                if annotate is not None:
                    annotate(fd, {"sims": sims})
                noted = sims
            step(HOLD_POLL_S)
    finally:
        if _held(fd):
            slots.release_slot(fd, pool)
    return 0


def _load_slots():
    """The machine's build-slot library, resolved as the conftest resolves it
    (`conftest._build_slot_module`: this checkout's copy asks which copy the
    machine runs) → None when the script is absent (a solo checkout)."""
    local = Path(__file__).resolve().parents[3] / "scripts" / "build-slot.py"
    if not local.exists():
        return None
    import importlib.util

    def _load(path):
        spec = importlib.util.spec_from_file_location("fauna_build_slot", str(path))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    mod = _load(local)
    target = mod._machine_policy_script()
    return _load(target) if target else mod


def _spawn_holder(directory: Path) -> int | None:
    """Start `hold` for `directory` in a session of its own (so the drain's
    command substitution never waits on it and its stops never reach it);
    its output goes to `<park>.holder.log` beside the park → its pid."""
    try:
        logf = open(directory.parent / f"{directory.name}.holder.log", "a")
    except OSError:
        return None
    try:
        proc = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), "hold", str(directory)],
            stdin=subprocess.DEVNULL, stdout=logf, stderr=subprocess.STDOUT,
            start_new_session=True, close_fds=True,
        )
    except OSError:
        return None
    finally:
        logf.close()
    return proc.pid


def _end_holder(
    directory: Path, holder_alive: Callable[[int], bool], step: Callable[[float], None],
) -> None:
    pid = _holder_pid(directory)
    if pid is not None:
        waited = 0.0
        while holder_alive(pid) and waited < HOLDER_EXIT_WAIT_S:
            step(HOLD_POLL_S)
            waited += HOLD_POLL_S
    (directory / _HOLDER_FILE).unlink(missing_ok=True)


def close(
    directory: Path, run: Run = _run, *,
    holder_alive: Callable[[int], bool] = _pid_alive,
    step: Callable[[float], None] = time.sleep,
) -> list[str]:
    """Shut down and delete every device the park holds (parked or
    mid-lease by a process that has exited), end the owner, wait for the
    holder to release the park's grant (`hold` — the deletes ran under it),
    and remove the directory → the deleted UDIDs."""
    deleted: list[str] = []
    try:
        entries = list(directory.iterdir())
    except OSError:
        return deleted
    for p in entries:
        if not p.name.endswith(_RECORD_SUFFIX) and _LEASING not in p.name:
            continue
        if _LEASING in p.name:
            try:
                if _pid_alive(int(p.name.rsplit(_LEASING, 1)[1])):
                    continue
            except ValueError:
                pass
        try:
            udid = str(json.loads(p.read_text()).get("udid", ""))
        except (OSError, ValueError):
            udid = ""
        if udid:
            _delete(udid, run)
            deleted.append(udid)
        p.unlink(missing_ok=True)
    for p in directory.glob(".*.tmp"):
        p.unlink(missing_ok=True)
    (directory / _OWNER_FILE).unlink(missing_ok=True)
    _end_holder(directory, holder_alive, step)
    try:
        directory.rmdir()
    except OSError:
        pass
    return deleted


def open_park(
    directory: Path, owner: int, run: Run = _run,
    spawn: Callable[[Path], int | None] | None = None,
) -> list[str]:
    """Open the park for `owner`: what an earlier, unclosed park left there is
    deleted first (→ those UDIDs), then the owner is written and, given a
    `spawn` (the CLI's `_spawn_holder`), the park's grant holder (`hold`) is
    started and its pid written."""
    stale = close(directory, run)
    directory.mkdir(parents=True, exist_ok=True)
    (directory / _OWNER_FILE).write_text(f"{owner}\n")
    if spawn is not None:
        pid = spawn(directory)
        if pid:
            (directory / _HOLDER_FILE).write_text(f"{pid}\n")
    return stale


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    o = sub.add_parser("open", help="open the park for the drain's turns")
    o.add_argument("dir", type=Path)
    o.add_argument("--owner-pid", type=int, required=True)
    c = sub.add_parser("close", help="delete every parked device and the park")
    c.add_argument("dir", type=Path)
    h = sub.add_parser("hold", help="hold the park's sim_idle grant until it closes (open starts it)")
    h.add_argument("dir", type=Path)
    args = ap.parse_args(argv)
    if args.cmd == "hold":
        return hold(args.dir, _load_slots(), log=lambda m: print(m, flush=True))
    if args.cmd == "open":
        stale = open_park(args.dir, args.owner_pid, spawn=_spawn_holder)
        print(f"sim-park: open at {args.dir} for pid {args.owner_pid}"
              + (f"; deleted {len(stale)} left by an unclosed park" if stale else ""))
    else:
        deleted = close(args.dir)
        print(f"sim-park: closed {args.dir}; deleted {len(deleted)} parked device(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
