"""Python IPC helper for the per-user fauna-sync agent's unix-domain-socket
protocol — the linux/macOS/tui sibling of `sync_agent_ipc.py`'s Win32
named-pipe transport.

Wire protocol: identical to the Windows transport — [u32 LE payload
length][canonical dag-cbor payload]. Only the transport differs (a plain
`socket.AF_UNIX` stream, no ctypes); the request builders and the dag-cbor
codec are NOT duplicated here — they are reused directly from
`sync_agent_ipc`, which pins byte-for-byte against the Rust golden
(`test_sync_agent_ipc_conformance.py`) and is safe to import on any platform
(its ctypes usage is windows-only and lives inside function bodies, never at
module scope).

Socket path: `fauna_ipc::unix_transport::default_socket_path()` —
    macOS: ``~/Library/Application Support/Fauna/sync-agent.sock``
    linux (+ other unix): ``$XDG_RUNTIME_DIR/fauna/sync-agent.sock``
An isolated e2e launch derives its own per-launch path under a throwaway
HOME/XDG_RUNTIME_DIR (see the macOS driver's ``app_support_dir`` property);
callers resolve and pass in the path, this module never guesses one.

Process safety: this module spawns nothing but a read-only `ps` (on a platform
with no `/proc`) and signals nothing. All IPC is a client connect to an
already-listening unix socket; even `shutdown_agent` stops the agent through
the agent's own `Shutdown` verb.
"""

from __future__ import annotations

import os
import socket
from contextlib import contextmanager
import struct
import subprocess
import sys
import time

import cbor2

from helpers.sync_agent_ipc import (
    _MAX_EVENTS_BEFORE_REPLY,
    encode_frame,
    get_service_status_request,
    get_sync_status_request,
    provision_capability_request,
    reconcile_account_runtime_request,
    unwrap_ok,
)

__all__ = [
    "wait_for_socket",
    "send_request",
    "provision",
    "sync_status",
    "reconcile_account_runtime",
    "server_pid",
    "process_has_exited",
    "shutdown_agent",
    "held_request",
    "attach_once",
    "attached_app",
    "push_arm_tally",
    "SyncAgentClient",
]


def wait_for_socket(socket_path: str, timeout: float = 10.0) -> None:
    """Poll until ``socket_path`` accepts a connection, or raise TimeoutError.

    Mirrors ``sync_agent_ipc.wait_for_pipe``: a bare path-exists check is not
    enough — `unix_transport::serve` creates the socket FILE at `bind()` but
    the listener isn't accepting until the following `listen()`/loop tick, so
    this connects-and-closes rather than stat()s.
    """
    deadline = time.monotonic() + timeout
    last_err: OSError | None = None
    while time.monotonic() < deadline:
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
                s.settimeout(1.0)
                s.connect(socket_path)
            return
        except OSError as e:
            last_err = e
            time.sleep(0.1)
    raise TimeoutError(
        f"socket {socket_path!r} not accepting connections within {timeout}s "
        f"(last error: {last_err})"
    )


def _recv_exact(sock: socket.socket, n: int) -> bytes:
    """Read exactly ``n`` bytes. A short `recv()` on a stream socket is a
    partial read, not EOF or an error — must loop until `n` bytes are in."""
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise OSError(f"socket closed after {len(buf)} of {n} expected bytes")
        buf += chunk
    return bytes(buf)


def _read_reply(sock: socket.socket, socket_path: str) -> dict:
    """Read frames off ``sock`` until the reply. The agent pushes its events to
    every open connection, a request's own included, so an event frame can
    precede the reply — the Windows transport's `_exchange` skips them the same
    way, bounded."""
    for _ in range(_MAX_EVENTS_BEFORE_REPLY):
        resp_len = struct.unpack("<I", _recv_exact(sock, 4))[0]
        decoded = cbor2.loads(_recv_exact(sock, resp_len))
        if isinstance(decoded, dict) and "event" in decoded and "result" not in decoded:
            continue
        return decoded
    raise OSError(
        f"{socket_path!r} pushed {_MAX_EVENTS_BEFORE_REPLY} event frames and no reply"
    )


def send_request(socket_path: str, frame: bytes, timeout: float = 10.0) -> dict:
    """Send a framed request over the unix socket and return the decoded Response.

    Opens a fresh connection per call — same one-shot-per-call shape as
    `sync_agent_ipc.send_request` — writes the frame, reads back a
    length-prefixed dag-cbor response, decodes it, and returns the Response
    dict: ``{"id": <int>, "result": {"Ok": ...} | {"Err": str}}``.

    Raises:
        OSError: On any socket failure (missing socket, refused connection,
            short read, timeout).
    """
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(socket_path)
        sock.sendall(frame)
        return _read_reply(sock, socket_path)


@contextmanager
def held_request(socket_path: str, frame: bytes, timeout: float = 10.0):
    """Send a framed request and keep its connection open past the reply — the
    unix twin of ``sync_agent_ipc.held_request``.

    Yields the decoded Response; the connection closes when the ``with`` body
    ends. For a request whose effect lasts exactly as long as the connection
    that carried it — ``AttachApp``'s lease, which the agent ends when the app's
    connection closes (a crash included).
    """
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(socket_path)
        sock.sendall(frame)
        yield _read_reply(sock, socket_path)


# ---------------------------------------------------------------------------
# High-level convenience functions (mirrors sync_agent_ipc's shape)
# ---------------------------------------------------------------------------

def provision(socket_path: str, req_id: int, *, timeout: float = 10.0, **caps) -> None:
    """Send a ProvisionCapability request and assert the response is Empty.

    Args:
        socket_path: Path to the agent's unix socket.
        req_id: Request correlation ID.
        **caps: Keyword arguments forwarded to provision_capability_request
                (backup_key, actor_id, nest_url, device_id, bearer_token,
                bearer_expires_at).

    Raises:
        RuntimeError: If the agent returns an Err response.
        AssertionError: If the Ok payload is not "Empty".
    """
    req = provision_capability_request(req_id, **caps)
    resp = send_request(socket_path, encode_frame(req), timeout=timeout)
    payload = unwrap_ok(resp)
    if payload != "Empty":
        raise AssertionError(
            f"ProvisionCapability expected 'Empty' payload, got {payload!r}"
        )


def sync_status(socket_path: str, req_id: int, *, timeout: float = 10.0) -> dict:
    """Send a GetSyncStatus request and return the SyncStatusInfo dict.

    Returns:
        {"connected": bool, "syncing": bool, "files_pending": int,
         "bytes_pending": int, "last_sync": int | None}
    """
    req = get_sync_status_request(req_id)
    resp = send_request(socket_path, encode_frame(req), timeout=timeout)
    ok_payload = unwrap_ok(resp)
    return ok_payload["SyncStatus"]


def reconcile_account_runtime(socket_path: str, req_id: int, *, timeout: float = 20.0) -> None:
    """Send the account-pass nudge and wait for its reply — which the agent
    sends once its `reconcile_now` finished, or after its 10 s cap
    (`pipe_server.rs::RECONCILE_NUDGE_TIMEOUT`), hence the wider socket timeout.

    Raises:
        RuntimeError: If the agent returns an Err response.
        AssertionError: If the Ok payload is not "Empty".
    """
    req = reconcile_account_runtime_request(req_id)
    payload = unwrap_ok(send_request(socket_path, encode_frame(req), timeout=timeout))
    if payload != "Empty":
        raise AssertionError(
            f"ReconcileAccountRuntime expected 'Empty' payload, got {payload!r}"
        )


def server_pid(socket_path: str, timeout: float = 10.0) -> int:
    """The pid of the process serving ``socket_path``, asked of the kernel —
    the unix twin of ``sync_agent_ipc.pipe_server_pid``.

    Linux answers ``SO_PEERCRED`` (``struct ucred``: pid, uid, gid); macOS
    answers ``LOCAL_PEERPID`` at level ``SOL_LOCAL``. Either way the answer is
    the peer's credential at ``connect()`` — the agent's own pid, never a guess
    from a process listing (which would also match a sibling launch's agent).
    """
    wait_for_socket(socket_path, timeout=timeout)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(timeout)
        s.connect(socket_path)
        if sys.platform == "darwin":
            sol_local, local_peerpid = 0, 0x002  # <sys/un.h>
            return struct.unpack("i", s.getsockopt(sol_local, local_peerpid, 4))[0]
        creds = s.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
        return struct.unpack("3i", creds)[0]


def process_has_exited(pid: int) -> bool:
    """Whether ``pid`` has exited — a zombie included: its descriptors (and so
    every flock it held, ``agent.lock`` and ``engine.lock`` among them) are
    closed once it is a zombie, whoever reaps it later.

    Read from ``/proc/<pid>/stat`` where there is one, else ``ps``; never
    ``os.kill(pid, 0)`` — a probe that spells a kill is one typo away from one,
    against a process another session may own (the e2e process-safety rule).
    """
    stat = f"/proc/{pid}/stat"
    if os.path.isdir("/proc/self"):
        try:
            with open(stat) as f:
                # `pid (comm) state …` — comm may hold spaces and parens, so the
                # state is the first field after the LAST ')'.
                return f.read().rsplit(")", 1)[1].split()[0] in ("Z", "X")
        except (OSError, IndexError):
            return True
    out = subprocess.run(
        ["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True,
    ).stdout.strip()
    return not out or out.startswith("Z")


def shutdown_agent(socket_path: str, *, timeout: float = 60.0) -> int:
    """Stop the agent serving ``socket_path`` through its own ``Shutdown``
    request and wait until its PROCESS has exited — not merely until the socket
    stops answering: a caller asserting what happens once the agent is GONE
    (its kernel locks released) needs the process down. Returns the pid that
    exited. The unix twin of ``windows_sync_agent.shutdown_agent``.

    ⚠ It stops the agent; nothing here keeps it stopped. An app whose
    provisioning loop finds the socket absent spawns a fresh agent on its next
    convergence tick (``fauna_client_sync::agent::ensure_agent_running``) — a
    caller that reads state after the stop must tell the two agents apart.
    """
    pid = server_pid(socket_path)
    unwrap_ok(send_request(socket_path, encode_frame({"id": 1, "method": "Shutdown"})))
    deadline = time.monotonic() + timeout
    while not process_has_exited(pid):
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"the agent (pid {pid}) serving {socket_path} accepted Shutdown but "
                f"was still running {timeout:.0f}s later"
            )
        time.sleep(0.2)  # sleep-ok: poll cadence of a deadline poll, not a settle wait
    return pid


def _attach_app_request(app: str) -> dict:
    # No `notification_identity`: the linux sink needs none (`RequestMethod::AttachApp`).
    return {"id": 1, "method": {"AttachApp": {"app": app}}}


def attach_once(socket_path: str, app: str, *, timeout: float = 10.0) -> None:
    """Attach as ``app`` (``"tui"``, ``"linux"``) and let go at once — the agent
    keeps the app's name for a tapped banner; the lease ends as the connection
    closes. The unix twin of ``windows_sync_agent.attach_once``."""
    unwrap_ok(send_request(socket_path, encode_frame(_attach_app_request(app)), timeout=timeout))


@contextmanager
def attached_app(socket_path: str, app: str, *, timeout: float = 10.0):
    """Hold ``app``'s attachment lease for the ``with`` body — an open app, as
    the agent's push arm sees one (`RequestMethod::AttachApp`).

    The agent takes the lease before it replies, so the body runs attached; the
    lease ends when the connection closes on exit. The unix twin of
    ``windows_sync_agent.attached_app``."""
    frame = encode_frame(_attach_app_request(app))
    with held_request(socket_path, frame, timeout=timeout) as reply:
        unwrap_ok(reply)
        yield


def push_arm_tally(socket_path: str, *, timeout: float = 10.0) -> dict:
    """The agent's push-arm decisions so far (test-only `GetPushArmTally`):
    ``{attached_apps, withheld, no_sink, posted, failed}``."""
    frame = encode_frame({"id": 1, "method": "GetPushArmTally"})
    return unwrap_ok(send_request(socket_path, frame, timeout=timeout))["PushArmTally"]


# ---------------------------------------------------------------------------
# Optional ergonomics: thin client class (mirrors sync_agent_ipc.SyncAgentClient)
# ---------------------------------------------------------------------------

class SyncAgentClient:
    """Thin wrapper around the module-level functions with auto-incrementing IDs.

    Usage::

        client = SyncAgentClient(socket_path)
        client.wait_for_pipe()  # named to match the Windows client's API
        client.provision(backup_key=..., actor_id=..., ...)
        status = client.service_status()
    """

    def __init__(self, socket_path: str, timeout: float = 10.0):
        self.socket_path = socket_path
        self.timeout = timeout
        self._next_id = 1

    def _req_id(self) -> int:
        rid = self._next_id
        self._next_id += 1
        return rid

    def wait_for_pipe(self) -> None:
        """Named to match `sync_agent_ipc.SyncAgentClient.wait_for_pipe` — a
        test written against either client can call the same method name."""
        wait_for_socket(self.socket_path, timeout=self.timeout)

    def provision(self, **caps) -> None:
        provision(self.socket_path, self._req_id(), timeout=self.timeout, **caps)

    def sync_status(self) -> dict:
        return sync_status(self.socket_path, self._req_id(), timeout=self.timeout)

    def service_status(self) -> dict:
        req = get_service_status_request(self._req_id())
        resp = send_request(self.socket_path, encode_frame(req), timeout=self.timeout)
        ok_payload = unwrap_ok(resp)
        return ok_payload["ServiceStatus"]
