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

Process safety: this module spawns nothing. All IPC is a client connect to
an already-listening unix socket.
"""

from __future__ import annotations

import socket
import struct
import time

import cbor2

from helpers.sync_agent_ipc import (
    encode_frame,
    get_service_status_request,
    get_sync_status_request,
    provision_capability_request,
    unwrap_ok,
)

__all__ = [
    "wait_for_socket",
    "send_request",
    "provision",
    "sync_status",
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
        resp_len = struct.unpack("<I", _recv_exact(sock, 4))[0]
        payload = _recv_exact(sock, resp_len)
    return cbor2.loads(payload)


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
