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

Usage from the drain (`delayed_e2e_drain`):

    python sim_park.py open <dir> --owner-pid <pid>
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
_RECORD_SUFFIX = ".json"
_LEASING = ".leasing-"

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


def close(directory: Path, run: Run = _run) -> list[str]:
    """Shut down and delete every device the park holds (parked or
    mid-lease by a process that has exited), and remove the directory →
    the deleted UDIDs."""
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
    try:
        directory.rmdir()
    except OSError:
        pass
    return deleted


def open_park(directory: Path, owner: int, run: Run = _run) -> list[str]:
    """Open the park for `owner`: what an earlier, unclosed park left there is
    deleted first (→ those UDIDs), then the owner is written."""
    stale = close(directory, run)
    directory.mkdir(parents=True, exist_ok=True)
    (directory / _OWNER_FILE).write_text(f"{owner}\n")
    return stale


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    o = sub.add_parser("open", help="open the park for the drain's turns")
    o.add_argument("dir", type=Path)
    o.add_argument("--owner-pid", type=int, required=True)
    c = sub.add_parser("close", help="delete every parked device and the park")
    c.add_argument("dir", type=Path)
    args = ap.parse_args(argv)
    if args.cmd == "open":
        stale = open_park(args.dir, args.owner_pid)
        print(f"sim-park: open at {args.dir} for pid {args.owner_pid}"
              + (f"; deleted {len(stale)} left by an unclosed park" if stale else ""))
    else:
        deleted = close(args.dir)
        print(f"sim-park: closed {args.dir}; deleted {len(deleted)} parked device(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
