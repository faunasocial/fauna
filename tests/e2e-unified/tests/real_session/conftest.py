"""Real-ambient-session category (opt-in: ``--real-session``).

Tests in this directory deliberately run against — and mutate — the box's REAL
desktop session: the real ``systemd --user`` manager, the real session D-Bus,
the real ``~/.config``. That is the exact machine-global surface testing.md
§ point 10 bans the standard suite from touching, and the ban stays correct
there: concurrent checkouts, CI, and machine-wide sweeps all need the
private-bus/private-XDG isolation. This category exists for the mechanisms
that isolation *by construction* cannot exercise — e.g. the sync-agent's
systemd-unit-write + D-Bus-drive path, which every e2e launch skips because
``SystemdAgentSpawner`` branches to a direct child spawn under e2e
(apps/fauna-linux/src/sync_agent.rs). A test here is the codified form of a
supervised manual pass: exactly as safe as asking a human to run the same
steps on their desktop, and far more repeatable.

Three gates (all mandatory, in this order):

1. **Collection**: the root conftest's ``pytest_ignore_collect`` prunes this
   directory unless ``--real-session`` is passed — no default sweep, tier
   filter, or ``just`` recipe can even import these files.
2. **Ambient-session guard** (``real_ambient_session`` below): the run must
   actually be on the real desktop session — real ``/run/user/<uid>`` runtime
   dir, real session bus, a reachable ``systemd --user`` manager, no
   ``FAUNA_E2E_*`` isolation in the environment. Fails loudly otherwise: a
   green run inside driver-style isolation would silently test nothing.
3. **Exclusivity**: a machine-wide loud+bounded flock (the live_box_lock
   convention, keyed ``real-desktop-session``) held for the whole session —
   the desktop session is a singleton resource, so two runs serialize instead
   of interleaving unit writes and bus traffic.

**The macOS arm (2026-09-26).** On macOS the ambient resource is not a desktop
session's units and bus but the user's **System Photo Library**, which
``photolibraryd`` serves daemon-side and no launch isolation can redirect — the
photo-backup witnesses (`test_photo_backup_macos.py`) run against it because
nothing else can drive real PhotoKit on a Mac. Gate 2 there asserts a real GUI
(Aqua) session with ``photolibraryd`` in it, **and that the box is a virtual
machine** — the user approved the VM's own disposable library, and the guard is
what keeps a run on a real person's Mac from ever writing into theirs. Gate 3 is
keyed ``real-photos-library``, the resource actually held.

Contract for tests here: **borrow and give back**. Back up any real state you
touch (an existing ``fauna-sync-agent.service``, a running agent), restore it
on teardown, and remove state your run created. Debris that is inert-by-key
(data scoped to a throwaway test actor) may remain; state a real component
would act on (agent config entries, unit files) must not.
"""

import os
import subprocess
import sys

import pytest

from helpers import live_box_lock

# The singleton resource key: this box's desktop session. One owner at a time,
# machine-wide, regardless of which test module wants it. On macOS the ambient
# resource is the user's System Photo Library instead (the macOS arm above).
REAL_SESSION_LOCK_KEY = (
    "real-photos-library" if sys.platform == "darwin" else "real-desktop-session"
)


def _fail(msg: str):
    pytest.fail(f"real_session ambient-session guard: {msg}", pytrace=False)


@pytest.fixture(scope="session", autouse=True)
def real_ambient_session():
    """Assert this run owns the box's real desktop session, then hold the
    machine-wide desktop-session lock for the whole pytest session.

    The *contract* of this category (opt-in, guarded, exclusive, borrow-and-give-
    back) is platform-neutral; only the probes that establish "this really is the
    ambient session" are OS-specific, so they branch below. The isolation-env check
    is common: it is the one that would let a green run silently test nothing.
    """
    for var in ("FAUNA_E2E_BRIDGE", "FAUNA_E2E_AGENT_PORT"):
        if var in os.environ:
            _fail(f"{var} is set — e2e mode would make the app skip the very "
                  "production paths this category exists to exercise")

    if sys.platform == "win32":
        _guard_windows_ambient_session()
    elif sys.platform == "darwin":
        _guard_macos_ambient_session()
    else:
        _guard_posix_ambient_session()

    fd = live_box_lock.acquire(REAL_SESSION_LOCK_KEY)
    try:
        yield
    finally:
        live_box_lock.release(fd)


def _guard_windows_ambient_session():
    """Windows: the ambient resource is the real USER's package registration state.

    There is no XDG/D-Bus/systemd triad here. What makes a Windows run "ambient" is
    that `Add-AppxPackage` will register into this logged-on user's own package
    catalogue — machine-global state a driver-isolated launch never touches. Two
    things must hold or the assertions below mean nothing.
    """
    # Developer Mode: without it an unsigned/self-signed package cannot register
    # per-user, and every registration assertion would fail for an environmental
    # reason rather than a product one.
    probe = subprocess.run(
        ["reg", "query",
         r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock",
         "/v", "AllowDevelopmentWithoutDevLicense"],
        capture_output=True, text=True,
    )
    if "0x1" not in (probe.stdout or ""):
        _fail(
            "Developer Mode is off (AllowDevelopmentWithoutDevLicense != 1), so a "
            "self-signed package cannot be registered per-user. Enable it in "
            "Settings > System > For developers, then re-run."
        )
    # The appx stack is serviced by the AppX Deployment Service; if it is not
    # runnable, Add-AppxPackage fails in ways that look like manifest bugs.
    svc = subprocess.run(
        ["powershell", "-NoProfile", "-Command",
         "(Get-Service AppXSvc).Status"],
        capture_output=True, text=True,
    )
    if "Running" not in (svc.stdout or "") and "Stopped" not in (svc.stdout or ""):
        _fail(f"AppXSvc is not queryable ({(svc.stdout or svc.stderr).strip()!r}) — "
              "the package deployment stack is not healthy on this box")


def _guard_macos_ambient_session():
    """macOS: the ambient resource is this user's System Photo Library.

    Three things must hold or a run is either meaningless or dangerous:

    - **A virtual machine.** The user approved running against the macOS VM's own
      disposable library (tagged test photos stay behind in it). On bare metal the
      library is a real person's, so the guard refuses outright rather than trusting
      whoever typed `--real-session`.
    - **A GUI (Aqua) session.** The venue launches the app through Launch Services
      so TCC charges the Photos grant to the app, and `photolibraryd` is a GUI-domain
      agent; from an SSH/background session neither is reachable, and every
      assertion would fail for an environmental reason.
    - **`photolibraryd` in that session** — the daemon that actually serves the
      library the engine reads.
    """
    vm = subprocess.run(["sysctl", "-n", "kern.hv_vmm_present"],
                        capture_output=True, text=True)
    if (vm.stdout or "").strip() != "1":
        _fail(
            "this Mac is not a virtual machine (kern.hv_vmm_present != 1). The "
            "photo-library venue writes test photos into the box's REAL System Photo "
            "Library and was approved only for the dev VM's own disposable library — "
            "it must never run where the library is a person's"
        )
    manager = subprocess.run(["launchctl", "managername"],
                             capture_output=True, text=True)
    if (manager.stdout or "").strip() != "Aqua":
        _fail(
            f"launchctl managername is {(manager.stdout or manager.stderr).strip()!r}, "
            "not 'Aqua' — this run is outside the logged-in GUI session, so neither "
            "Launch Services nor photolibraryd is reachable from it"
        )
    uid = os.getuid()
    daemon = subprocess.run(
        ["launchctl", "print", f"gui/{uid}/com.apple.photolibraryd"],
        capture_output=True, text=True,
    )
    if daemon.returncode != 0:
        _fail(
            f"com.apple.photolibraryd is not loaded in gui/{uid} "
            f"({(daemon.stderr or daemon.stdout).strip()[:200]!r}) — nothing would "
            "serve the System Photo Library the engine reads"
        )


def _guard_posix_ambient_session():
    uid = os.getuid()
    expected_runtime = f"/run/user/{uid}"
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR", "")
    if runtime_dir != expected_runtime:
        _fail(
            f"XDG_RUNTIME_DIR is {runtime_dir!r}, expected {expected_runtime!r} — "
            "this environment looks driver-isolated, not the real session"
        )
    bus = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    if bus and f"{expected_runtime}/bus" not in bus:
        _fail(
            f"DBUS_SESSION_BUS_ADDRESS ({bus!r}) does not point at the real session "
            f"bus {expected_runtime}/bus — a private harness bus would make every "
            "systemd/D-Bus assertion here meaningless"
        )
    # (the FAUNA_E2E_* isolation check is common to both platforms and now lives
    #  in real_ambient_session, above)
    if not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        _fail("no DISPLAY/WAYLAND_DISPLAY — not a desktop session")
    probe = subprocess.run(
        ["systemctl", "--user", "is-system-running"],
        capture_output=True, text=True,
    )
    state = (probe.stdout or "").strip()
    if state not in {"running", "degraded"}:
        _fail(
            "the real `systemd --user` manager is not reachable/healthy "
            f"(is-system-running -> {state or probe.stderr.strip()!r})"
        )
    # `systemctl` talks to the manager over its PRIVATE socket, so the check
    # above can pass while the manager's SESSION-BUS name is dead — the exact
    # channel a sandboxed app must use. Observed 2026-07-20: an experiment
    # spawning a second `systemd --user` left `org.freedesktop.systemd1`
    # unowned on the session bus, every call hanging in D-Bus activation until
    # timeout, and the Flatpak seam silently falling back to app-child
    # residency. Probe the bus name directly; any healthy manager answers in
    # milliseconds (an error reply like FileNotFound still counts as alive).
    try:
        bus_probe = subprocess.run(
            ["gdbus", "call", "--session",
             "--dest", "org.freedesktop.systemd1",
             "--object-path", "/org/freedesktop/systemd1",
             "--method", "org.freedesktop.systemd1.Manager.GetUnitFileState",
             "fauna-sync-agent.service"],
            capture_output=True, text=True, timeout=10,
        )
        wedged = "Timeout was reached" in (bus_probe.stderr or "")
    except subprocess.TimeoutExpired:
        wedged = True
    if wedged:
        _fail(
            "`org.freedesktop.systemd1` is unresponsive on the SESSION BUS "
            "(while the private `systemctl` socket works) — the user manager "
            "lost its bus name. Remedy: `systemctl --user daemon-reexec`, "
            "then re-run."
        )
