"""The android device axis — which device this run drives, and whether it is there.

`testing.md` § Default app and nest mode, *Mode mechanics* (a **run-level
harness input**, the same class as `--app` / `--nest`) and *Android's run venue*
(constraint 3's third leg, "any run-level way to name the device").

Sibling of `nest_mode.py`, and built in its shape for the same reason: the value
is needed by module-level conftest code that is handed no pytest `config` --
`_android_available()`, which runs during collection -- so the run pushes it in
once at `pytest_configure` and everything reads it from here.

**This is a harness input, never product configuration.** `principles.md` § One
configuration surface puts every value in one of two buckets, and an adb serial
is squarely bucket 1: nothing a user or admin would ever want to choose. It
names which physical device the *test harness* talks to, exactly as `--nest`
names which nest fills the run's real-nest slot.

**Why the serial must reach the availability probe, not just the driver.** A
bare `adb devices` probe answers "some device is attached", which on a shared
machine -- or a box with both an emulator and a USB handset -- is not the same
question as "the device this run was told to use is attached". Answering the
first and then driving the second is how a run silently adopts a foreign device
and reports its results as though they came from the named one. So the probe
below takes the serial and demands that exact serial, in state `device`.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess

#: The env equivalent of `--device-serial`, in the `E2E_NEST` / `E2E_APPS`
#: family. The flag beats the environment, which beats "whatever is attached".
ENV_VAR = "E2E_DEVICE_SERIAL"


class AndroidDeviceError(Exception):
    """A malformed `--device-serial` / `E2E_DEVICE_SERIAL` value."""


def resolve_device_serial(raw: str | None, environ=None) -> str | None:
    """The run's device serial, from the flag, else the env, else None.

    Same precedence and same empty-value posture as `resolve_nest_mode`:
    `None` means the flag was absent (fall through to the env, then to
    "whichever single device is attached"); `""` means it was PRESENT and empty
    -- `--device-serial "$UNSET_VAR"` in a recipe -- and is an error rather than
    a request for the default, because silently adopting an arbitrary device is
    the exact failure this axis exists to prevent.
    """
    if raw is not None:
        text = raw.strip()
        if not text:
            raise AndroidDeviceError(
                "--device-serial was given an empty value; pass a serial as "
                "`adb devices` prints it, or omit the flag entirely to use "
                "whichever single device is attached"
            )
        return text
    env = os.environ if environ is None else environ
    override = (env.get(ENV_VAR) or "").strip()
    return override or None


def parse_adb_devices(output: str) -> list[tuple[str, str]]:
    """`adb devices` stdout -> [(serial, state), ...].

    Deliberately a real parse rather than a substring test. The bare
    `any("device" in line)` form this replaces answered True for a line whose
    *serial* happened to contain the word, and could not tell `device` from
    `unauthorized` / `offline` / `recovery` in any reading that survives a new
    adb state being added.

    The header (`List of devices attached`) and adb's `* daemon started *`
    chatter carry no second field or start with `*`, so both drop out here.
    """
    rows: list[tuple[str, str]] = []
    for line in (output or "").splitlines():
        if line.startswith("*") or line.lower().startswith("list of devices"):
            continue
        parts = line.split()
        if len(parts) < 2:
            continue
        rows.append((parts[0], parts[1]))
    return rows


def device_present(output: str, serial: str | None) -> bool:
    """Is the run's device attached and usable, per `adb devices` output?

    With a serial: that exact serial, in state `device`. Without one: any device
    in state `device`. `unauthorized` and `offline` are NOT usable -- a driver
    would fail on its first `adb install` with a message that diagnoses nothing,
    where an honest False here produces the harness's own skip instead.
    """
    ready = [s for s, state in parse_adb_devices(output) if state == "device"]
    if serial is not None:
        return serial in ready
    return bool(ready)


#: The env equivalent of `--adb-server`.
ADB_SERVER_ENV_VAR = "E2E_ADB_SERVER"

_ADB_SERVER_SPEC = re.compile(r"tcp:(?P<host>[^:\s]+):(?P<port>\d{1,5})\Z")


def resolve_adb_server(raw: str | None, environ=None) -> str | None:
    """The run's adb server socket spec, from the flag, else the env, else None.

    None means "adb's own default" -- a local server, started on demand, which
    is every run that is not a tunnelled-venue run. A value names a server that
    is ALREADY RUNNING somewhere else and is reached as a pure client
    (`testing.md` § Default app and nest mode → *Android's run venue*).

    The grammar is adb's own `-L` socket spec, narrowed to the one form that is
    safe here -- `tcp:127.0.0.1:<port>`:

      * **The host is mandatory.** `tcp:<port>` alone is adb's LOCAL form: a
        client that finds nothing listening starts a server there. With a host
        the client treats the server as remote and never starts one -- measured
        2026-10-01 (adb 37.0.0): `adb -L tcp:127.0.0.1:<dead port> devices`
        exits 1 in under 0.1 s with "cannot start server on remote host" and
        leaves nothing listening. That is what lets a run whose tunnel is down
        report android unavailable instead of silently driving a fresh, empty
        local server.
      * **The host is loopback.** adb's server protocol is an unauthenticated
        device shell, so the harness reaches it over a tunnel's loopback end
        and never across a network interface (constraint 2 of that section).

    Empty-but-present is an error for the reason `resolve_device_serial` gives.
    """
    if raw is not None:
        text = raw.strip()
        if not text:
            raise AndroidDeviceError(
                "--adb-server was given an empty value; pass a socket spec such "
                "as tcp:127.0.0.1:18509, or omit the flag to use a local adb server"
            )
    else:
        env = os.environ if environ is None else environ
        text = (env.get(ADB_SERVER_ENV_VAR) or "").strip()
        if not text:
            return None
    match = _ADB_SERVER_SPEC.match(text)
    if not match:
        raise AndroidDeviceError(
            f"adb server spec {text!r} is not tcp:127.0.0.1:<port>. The host is "
            f"required: adb reads a bare tcp:<port> as a LOCAL server and starts "
            f"one when nothing is listening"
        )
    if match["host"] != "127.0.0.1":
        raise AndroidDeviceError(
            f"adb server spec {text!r} names a non-loopback host. adb's server "
            f"protocol is unauthenticated; reach a remote server through a "
            f"tunnel's loopback end (tcp:127.0.0.1:<port>)"
        )
    if not 0 < int(match["port"]) < 65536:
        raise AndroidDeviceError(f"adb server spec {text!r} has no valid port")
    return text


def adb_argv(serial: str | None, adb_server: str | None = None) -> list[str]:
    """The adb command prefix for this run: the executable, then `-L <spec>`
    when the run names an adb server, then `-s <serial>` when it names a device.

    The ONE home of the prefix: the availability probe and the driver both build
    every adb command line from it, so they cannot drift into probing one
    server or device and driving another. The server rides each invocation as
    `-L` rather than as an `ADB_SERVER_SOCKET` in the process environment, so it
    cannot leak into a child that was never meant to reach the remote device.

    The executable is resolved through `shutil.which`, not handed to the OS as
    the bare word: on Windows CreateProcess appends only `.exe` to a bare name,
    so an adb reached through a `.cmd`/`.bat` wrapper on PATH (PATHEXT) would be
    "file not found" although a shell finds it. The bare name stays the
    fallback so a missing adb still fails loudly at the first call.
    """
    argv = [shutil.which("adb") or "adb"]
    if adb_server:
        argv += ["-L", adb_server]
    if serial:
        argv += ["-s", serial]
    return argv


def probe_device(serial: str | None, adb_server: str | None, timeout: float = 5) -> bool:
    """Is this run's device attached and usable, on this run's adb server?

    False -- never an exception -- for every way the answer can fail to arrive:
    no adb, a server that cannot be reached (a venue run whose tunnel is down),
    a client that does not answer inside `timeout`. The harness then reports
    android unavailable, which is the truth, rather than red.

    `-s` is deliberately not passed: `adb devices` lists every device, and the
    serial is matched against that list by `device_present`.
    """
    try:
        result = subprocess.run(
            [*adb_argv(None, adb_server), "devices"],
            capture_output=True, text=True, timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    return device_present(result.stdout, serial)


# ── Run-level state, set once by conftest's pytest_configure ────────────────
# `_android_available()` and `_build_app_config()` are both module-level and are
# handed no pytest `config`, so the run's serial and adb server live here and
# conftest pushes them in -- the same shape `nest_mode._RUN_MODE` uses, for the
# same reason.

_DEVICE_SERIAL: str | None = None
_ADB_SERVER: str | None = None


def set_device_serial(serial: str | None) -> None:
    """Called by conftest for `--device-serial`. Not for test code."""
    global _DEVICE_SERIAL
    _DEVICE_SERIAL = serial


def device_serial() -> str | None:
    """This run's device serial, or None for "whichever device is attached"."""
    return _DEVICE_SERIAL


def set_adb_server(spec: str | None) -> None:
    """Called by conftest for `--adb-server`. Not for test code."""
    global _ADB_SERVER
    _ADB_SERVER = spec


def adb_server() -> str | None:
    """This run's adb server socket spec, or None for a local server."""
    return _ADB_SERVER


#: The instrumentation APK carrying the on-device bridge, repo-relative -- the
#: `app_path` APK's sibling, built by the same `just android-debug`.
TEST_APK = "apps/fauna-android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"


def run_launch_facts(repo_root) -> dict:
    """The run facts every android launch config carries beside its `app_path`:
    the bridge APK to install, this run's device and this run's adb server. One
    home for both launchers -- conftest's `app` config and the launch harness
    (`common.launch_harness`) -- so neither can drop one."""
    from pathlib import Path

    return {
        "test_apk": str(Path(repo_root) / TEST_APK),
        "device_serial": device_serial(),
        "adb_server": adb_server(),
    }
