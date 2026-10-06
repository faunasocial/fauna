"""Android's run venue (A2) — the fixed ports, their leases, and the tunnel.

`testing.md` § Default app and nest mode → *Android's run venue*: the emulator
and its adb server run on the machine that can virtualize one, pytest runs on
the primary dev VM, and the two meet through ONE SSH tunnel dialed from the
emulator's machine. A tunnel carries only ports named when it is dialed, so in
a venue run nothing may come from `find_free_port()`: every port either side
listens on is a constant in this file, and the command that forwards them is
generated from the same constants (`tunnel_command`, printed by
`just android-tunnel-spec`). This module is the ONE home of all three.

The topology, by direction (the side that LISTENS is named first):

  * dev VM `127.0.0.1:ADB_SERVER_PORT`  -> emulator machine `127.0.0.1:5037`
    (`-R`): the driver's adb runs as a pure CLIENT of the remote adb server.
  * dev VM `127.0.0.1:<bridge port>`    -> emulator machine, same port (`-R`):
    `adb forward tcp:<p> tcp:18500` listens where the adb SERVER runs.
  * emulator machine `127.0.0.1:<nest port>` -> dev VM, same port (`-L`): the
    device's `adb reverse` lands on the adb server's loopback, which is NOT the
    machine the test nest runs on, so the tunnel carries it the rest of the way.

Every listener at both ends is `127.0.0.1` (constraint 2 of that section).

**The ranges, ratified 2026-10-01** against every fixed port in `tests/`,
`scripts/`, the `justfile` and the android sources: nothing else names
18509-18517 or 18600-18631 (the device-side bridge port, 18500, lives on the
DEVICE and collides with nothing here). All sit below Linux's ephemeral range
(32768+), so `find_free_port()` in a sibling session can never be handed one.

**Why the dev VM's adb listener is not 5037.** 5037 is adb's default, and any
bare `adb` call on the dev VM -- every non-venue run's availability probe is one
-- starts a local server there. A tunnel that wanted 5037 on the dev VM
would refuse to come up beside it (`ExitOnForwardFailure`), and while up it
would hand the remote emulator to every sibling session's bare probe. A port of
the venue's own keeps the two apart in both directions.
"""

from __future__ import annotations

import os
import re
import socket
import sys

#: adb's default server port, on the machine the emulator runs on. The far end
#: of the adb forward; never listened on by the venue on the dev VM (above).
REMOTE_ADB_PORT = 5037

#: The dev VM's loopback port that reaches that adb server through the tunnel.
ADB_SERVER_PORT = 18509

#: What `--adb-server` is given for a venue run.
ADB_SERVER_SPEC = f"tcp:127.0.0.1:{ADB_SERVER_PORT}"

#: Local ports for `adb forward tcp:<p> tcp:18500`, one per device seat.
BRIDGE_FORWARD_PORTS = range(18510, 18518)

#: Ports the test nests of a venue run listen on.
NEST_PORTS = range(18600, 18632)

#: The forwarding-only key the venue's setup creates on the emulator's machine.
TUNNEL_KEY = "~/.ssh/fauna_android_e2e_tunnel"


class AndroidVenueError(RuntimeError):
    """The venue cannot serve the request -- a range exhausted, a port outside
    it, a malformed tunnel argument."""


# ── Port leases: one port, one holder, across every process on the machine ──
#
# Two e2e lanes can run android at once, and a multi-nest journey starts several
# nests in one process, so "the next free port of the range" needs an arbiter
# every pytest process on the box agrees on. A lease is an exclusive kernel lock
# on `<lease dir>/<port>.lock`, held on an open descriptor for as long as the
# port is in use: it is released by `release()`, and by the kernel when the
# holder dies however it dies, so a SIGKILLed run strands nothing and no lease
# is ever broken by age or by pid-liveness guesswork.


def lease_dir() -> str:
    """Machine-wide and cross-session, by the same rule as the build slots'
    lock directory. The `/work/tmp` probe is gated on linux because that path
    is absolute only on POSIX: on Windows it is drive-relative and would name a
    different directory per current drive -- one machine, two lease sets."""
    if sys.platform.startswith("linux") and os.path.isdir("/work/tmp"):
        return "/work/tmp/fauna-android-venue"
    return os.path.join(os.path.expanduser("~"), ".cache", "fauna-android-venue")


def _try_lock(fd: int) -> bool:
    """Nonblocking exclusive lock; POSIX `flock`, `msvcrt` on Windows. `flock`
    conflicts between open file descriptions, so a second lease on the same port
    from inside ONE process is refused exactly like one from another process."""
    try:
        import fcntl
    except ImportError:
        import msvcrt

        try:
            msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
            return True
        except OSError:
            return False
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except OSError:
        return False


def _bindable(port: int) -> bool:
    """Can a listener bind `127.0.0.1:<port>` right now?"""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        try:
            s.bind(("127.0.0.1", port))
        except OSError:
            return False
    return True


class PortLease:
    """One leased port. `release()` is idempotent."""

    def __init__(self, port: int, fd: int):
        self.port = port
        self._fd: int | None = fd

    def release(self) -> None:
        fd, self._fd = self._fd, None
        if fd is not None:
            os.close(fd)  # closing the descriptor is what drops the lock


def lease_port(ports, *, must_bind: bool, directory: str | None = None) -> PortLease:
    """Lease the first port of `ports` nobody holds.

    `must_bind` also skips a port something is already listening on -- right
    for a nest port, which the nest is about to bind itself (a nest that
    outlived the run that leased its port is skipped rather than collided
    with). It is WRONG for a bridge-forward port: under the tunnel that port is
    listened on by the tunnel on this machine by design, and the lease exists
    only to keep two drivers off one forward.
    """
    directory = directory or lease_dir()
    os.makedirs(directory, exist_ok=True)
    for port in ports:
        fd = os.open(os.path.join(directory, f"{port}.lock"), os.O_RDWR | os.O_CREAT, 0o644)
        if _try_lock(fd) and (not must_bind or _bindable(port)):
            return PortLease(port, fd)
        os.close(fd)
    raise AndroidVenueError(
        f"no free port in {ports.start}-{ports.stop - 1}: every one is leased "
        f"by a live run (lease files in {directory}) or already listened on. "
        f"The range is as wide as the tunnel `just android-tunnel-spec` prints; "
        f"widen both together, in tests/e2e-unified/helpers/android_venue.py."
    )


def in_venue() -> bool:
    """Is this a venue run -- was the run given `--adb-server`?"""
    # Imported here so the `__main__` form below (the recipe) needs no package
    # path: it prints constants and never asks about a run.
    from helpers import android_device

    return android_device.adb_server() is not None


def lease_bridge_forward_port() -> PortLease:
    return lease_port(BRIDGE_FORWARD_PORTS, must_bind=False)


def nest_port(find_free_port):
    """A port for a nest this run is about to start -> `(port, release)`.

    Outside a venue run this is `find_free_port()` and a no-op release, i.e.
    exactly what every nest provider did before; in one it is a lease on
    `NEST_PORTS`, the only ports the device can reach.
    """
    if not in_venue():
        return find_free_port(), lambda: None
    lease = lease_port(NEST_PORTS, must_bind=True)
    return lease.port, lease.release


def require_tunnelled_nest_port(port: int) -> None:
    """Refuse an `adb reverse` for a port the tunnel does not carry. Called by
    the driver for a launch that names a remote adb server.

    The reverse itself would succeed and the app would then fail its first nest
    call with a bare connection error naming nothing; the cause is knowable
    here, so it is said here.
    """
    if port not in NEST_PORTS:
        raise AndroidVenueError(
            f"nest port {port} is outside the android venue's tunnelled range "
            f"{NEST_PORTS.start}-{NEST_PORTS.stop - 1}: the device's `adb reverse` "
            f"lands on the adb server's machine, and the tunnel carries only that "
            f"range back to this one. The nest was started with a port of its own "
            f"choosing; take it from `helpers.android_venue.nest_port` instead."
        )


# ── The tunnel ──────────────────────────────────────────────────────────────

_SSH_WORD = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")


def tunnel_forwards() -> list[tuple[str, int, int]]:
    """Every forward of the tunnel as `(flag, listen port, target port)`."""
    forwards = [("-R", ADB_SERVER_PORT, REMOTE_ADB_PORT)]
    forwards += [("-R", p, p) for p in BRIDGE_FORWARD_PORTS]
    forwards += [("-L", p, p) for p in NEST_PORTS]
    return forwards


def tunnel_command(login: str, dev_vm_address: str) -> list[str]:
    """The `ssh` argv the emulator's machine runs to dial the dev VM."""
    for name, value in (("login", login), ("address", dev_vm_address)):
        if not _SSH_WORD.match(value or ""):
            # The output is pasted into a shell on another machine.
            raise AndroidVenueError(f"tunnel {name} {value!r} is not a plain user/host word")
    argv = ["ssh", "-N", "-i", TUNNEL_KEY,
            "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=30"]
    for flag, listen, target in tunnel_forwards():
        argv += [flag, f"127.0.0.1:{listen}:127.0.0.1:{target}"]
    return [*argv, f"{login}@{dev_vm_address}"]


def _this_machine_address() -> str:
    """The address this machine's default route sources from. A UDP `connect`
    sends nothing; it only makes the kernel pick the interface."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.connect(("192.0.2.1", 9))  # TEST-NET-1, never routed
        return s.getsockname()[0]


def tunnel_spec(login: str = "", dev_vm_address: str = "") -> str:
    """What `just android-tunnel-spec` prints."""
    import getpass

    argv = tunnel_command(login or getpass.getuser(), dev_vm_address or _this_machine_address())
    head, forwards, dest = argv[:8], argv[8:-1], argv[-1]
    lines = [" ".join(head)]
    lines += [f"    {forwards[i]} {forwards[i + 1]}" for i in range(0, len(forwards), 2)]
    lines.append(f"    {dest}")
    return "\n".join([
        "# Run on the emulator's machine, after its emulator and adb server are up",
        "# and the forwarding-only key exists. Keep it running for the whole e2e run.",
        "# It refuses to start if any forward cannot bind (ExitOnForwardFailure).",
        " \\\n".join(lines),
        "",
        "# Then, on the dev VM, an android e2e run names the tunnelled adb server:",
        f"#   --app android --adb-server {ADB_SERVER_SPEC} --device-serial emulator-5554",
    ])


if __name__ == "__main__":
    print(tunnel_spec(*sys.argv[1:3]))
