"""Running the real linux `fauna-sync-agent` as a test's own child, in a world of
its own — the linux counterpart of ``helpers/windows_sync_agent.running_agent``.

On linux the agent's every location is derived from the environment, not from a
flag: its unix socket is ``$XDG_RUNTIME_DIR/fauna/sync-agent.sock``
(`fauna_ipc::unix_transport::default_socket_path`, which ignores
``--pipe-name``), its account store and sync state live under the XDG config
root (`fauna_account_store::root`), and its keyring and banner sink are
whatever its session bus offers. So isolation (convention 10) is one private
world: a throwaway HOME / XDG tree, a private session bus the caller owns
(``helpers/notification_bus.NotificationBus`` for a push-arm test), and a
private credential store (``FAUNA_E2E_CREDENTIAL_DIR``) — never the developer's
desktop session, keyring or the machine's per-user agent.

Talk to it with ``helpers/sync_agent_ipc_unix`` on the socket path
:func:`running_agent` yields.

Process safety: the agent is this test's own child (group leader +
die-with-parent), confirmed by the kernel's peer credential to be the process
serving the socket, and stopped by handle on exit — never by name.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from contextlib import contextmanager
from pathlib import Path

from drivers.port_util import popen_group_kwargs, track_process, untrack_process
from helpers.sync_agent_ipc_unix import server_pid, wait_for_socket

#: `sockaddr_un.sun_path` is 108 bytes on linux, NUL included.
_SUN_PATH_MAX = 107


def _world(root: Path) -> dict:
    """The agent's private XDG world under ``root``; returns its env overlay."""
    dirs = {
        "HOME": root / "home",
        "XDG_CONFIG_HOME": root / "home" / ".config",
        "XDG_DATA_HOME": root / "home" / ".local" / "share",
        "XDG_STATE_HOME": root / "home" / ".local" / "state",
        "XDG_CACHE_HOME": root / "home" / ".cache",
        "XDG_RUNTIME_DIR": root / "run",
        "FAUNA_E2E_CREDENTIAL_DIR": root / "credentials",
    }
    for d in dirs.values():
        d.mkdir(parents=True, exist_ok=True)
    # The agent refuses a runtime dir another account could reach.
    os.chmod(dirs["XDG_RUNTIME_DIR"], 0o700)
    return {k: str(v) for k, v in dirs.items()}


@contextmanager
def running_agent(exe, data_dir, log_path, *, bus_address: str):
    """Popen a fauna-sync agent in a private world, confirm IT serves its
    socket, and guarantee teardown. Yields the socket path.

    Args:
        exe: Path to the ``fauna-sync-agent`` debug binary (the sibling `just
            tui-debug` builds next to ``fauna-tui``).
        data_dir: The agent's ``--data-dir`` (fresh per test).
        log_path: Where stdout+stderr are captured.
        bus_address: The private session bus the agent gets — the caller owns
            and stops it.
    """
    # The socket lives in a short temp dir of its own: a pytest `tmp_path` named
    # after a long test overruns `sun_path`.
    root = Path(tempfile.mkdtemp(prefix="fauna-agent-"))
    try:
        env = dict(os.environ)
        # Nothing of the developer's desktop: no display, and the bus is ours.
        for var in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"):
            env.pop(var, None)
        env.update(_world(root))
        env["DBUS_SESSION_BUS_ADDRESS"] = bus_address
        socket_path = os.path.join(env["XDG_RUNTIME_DIR"], "fauna", "sync-agent.sock")
        if len(socket_path.encode()) > _SUN_PATH_MAX:
            raise RuntimeError(
                f"agent socket path {socket_path!r} overruns sun_path ({_SUN_PATH_MAX} bytes): "
                "point TMPDIR at a shorter directory"
            )
        with open(log_path, "w") as log:
            proc = subprocess.Popen(
                [str(exe), "--foreground", "--data-dir", str(data_dir)],
                stdout=log,
                stderr=subprocess.STDOUT,
                env=env,
                **popen_group_kwargs(),
            )
            track_process(proc)
            try:
                wait_for_socket(socket_path, timeout=20)
                served_by = server_pid(socket_path, timeout=20)
                if served_by != proc.pid:
                    raise RuntimeError(
                        f"{socket_path} is served by process {served_by}, not by the "
                        f"agent this spawn started ({proc.pid}); its log: {log_path}"
                    )
                yield socket_path
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
                untrack_process(proc)
    finally:
        shutil.rmtree(root, ignore_errors=True)
