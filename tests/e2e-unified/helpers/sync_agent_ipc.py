"""Python IPC helper for the per-user fauna-sync agent named-pipe protocol.

Wire protocol: [u32 LE payload length][canonical dag-cbor payload]
The payload is canonical IPLD dag-cbor (cbor2.dumps with canonical=True),
matching the Rust fauna_cbor::encode_canonical (CTAP2-canonical) ordering.

The conformance test pins the Python encoder byte-for-byte against the Rust
golden hex from fauna-ipc/src/sync.rs::print_wire_format_for_provision_capability.

Process safety: this module spawns nothing. All IPC is via Win32 named pipes.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes
import functools
import struct
import time
from contextlib import contextmanager

import cbor2


# ---------------------------------------------------------------------------
# Encoding helpers
# ---------------------------------------------------------------------------

def encode_payload(obj: dict) -> bytes:
    """Encode obj as canonical dag-cbor (NO length prefix).

    cbor2 canonical=True uses CTAP2-canonical ordering (length-first then
    bytewise for map keys), matching Rust fauna_cbor::encode_canonical.
    """
    return cbor2.dumps(obj, canonical=True)


def encode_frame(obj: dict) -> bytes:
    """Encode obj as a length-prefixed canonical dag-cbor frame.

    Frame layout: [u32 LE payload length][canonical dag-cbor payload]
    """
    payload = encode_payload(obj)
    return struct.pack("<I", len(payload)) + payload


# ---------------------------------------------------------------------------
# Request constructors
# ---------------------------------------------------------------------------

def provision_capability_request(
    req_id: int,
    *,
    backup_key: bytes,
    actor_id: bytes,
    nest_url: str,
    device_id: str,
    bearer_token: str,
    bearer_expires_at: int | None,
    provisioned_at_ms: int | None = None,
) -> dict:
    """Build a ProvisionCapability Request dict.

    Returns:
        {"id": req_id, "method": {"ProvisionCapability": <SyncCapability map>}}

    The map is every field `SyncCapability` serializes for an identity that
    never succeeded — the shape `SyncCapability::new` encodes: the two
    predecessor lists empty, `provisioned_at_ms` stamped. `provisioned_at_ms` is
    REQUIRED by the agent's decoder (a capability without it "is not one this
    software wrote", `fauna-ipc/src/sync.rs`) — omitted here, it is stamped now,
    as the app's builder stamps it at mint.

    Field ordering is irrelevant — cbor2 canonical=True handles sorting.
    backup_key and actor_id are CBOR byte strings (major type 2); pass raw bytes.
    """
    if provisioned_at_ms is None:
        provisioned_at_ms = int(time.time() * 1000)
    return {
        "id": req_id,
        "method": {
            "ProvisionCapability": {
                "backup_key": backup_key,
                "actor_id": actor_id,
                "nest_url": nest_url,
                "device_id": device_id,
                "bearer": {
                    "token": bearer_token,
                    "expires_at": bearer_expires_at,
                },
                "predecessor_backup_keys": b"",
                "predecessor_actor_ids": b"",
                "provisioned_at_ms": provisioned_at_ms,
            }
        },
    }


def get_sync_status_request(req_id: int) -> dict:
    """Build a GetSyncStatus Request dict.

    Unit variant encodes as the bare variant-name string.
    """
    return {"id": req_id, "method": "GetSyncStatus"}


def get_service_status_request(req_id: int) -> dict:
    """Build a GetServiceStatus Request dict.

    Unit variant encodes as the bare variant-name string.
    """
    return {"id": req_id, "method": "GetServiceStatus"}


def reconcile_account_runtime_request(req_id: int) -> dict:
    """Build a ReconcileAccountRuntime Request dict — the account-pass nudge.

    The production verb an app runtime sends its co-located agent when it
    yields the engine role (`fauna-ipc/src/sync.rs`); the agent runs
    `reconcile_now` on its mounted store whether or not it holds the role and
    replies `Empty` once the pass finished (or after its 10 s reply cap).
    Unit variant encodes as the bare variant-name string.
    """
    return {"id": req_id, "method": "ReconcileAccountRuntime"}


# ---------------------------------------------------------------------------
# Win32 named-pipe I/O
# ---------------------------------------------------------------------------

_GENERIC_READ = 0x80000000
_GENERIC_WRITE = 0x40000000
_OPEN_EXISTING = 3
_ERROR_FILE_NOT_FOUND = 2
_ERROR_PIPE_BUSY = 231


@functools.lru_cache(maxsize=1)
def _kernel32():
    """kernel32 loaded with ``use_last_error=True``, its pipe calls typed.

    ``ctypes.get_last_error()`` reads only a ``use_last_error`` handle's private
    copy of the thread's last error: after a call through ``ctypes.windll`` it
    reads 0, so a failed open used to report "Win32 error 0" whatever the cause
    (convention 6 — a failure names its own cause). Typed so a ``HANDLE`` is
    passed and returned at full pointer width. Lazy, because the unix twin
    (``sync_agent_ipc_unix``) imports this module where there is no kernel32.
    """
    w = ctypes.wintypes
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.CreateFileW.restype = w.HANDLE
    k32.CreateFileW.argtypes = [
        w.LPCWSTR, w.DWORD, w.DWORD, ctypes.c_void_p, w.DWORD, w.DWORD, w.HANDLE,
    ]
    k32.ReadFile.restype = w.BOOL
    k32.ReadFile.argtypes = [w.HANDLE, ctypes.c_void_p, w.DWORD, ctypes.POINTER(w.DWORD), ctypes.c_void_p]
    k32.WriteFile.restype = w.BOOL
    k32.WriteFile.argtypes = [w.HANDLE, ctypes.c_void_p, w.DWORD, ctypes.POINTER(w.DWORD), ctypes.c_void_p]
    k32.CloseHandle.restype = w.BOOL
    k32.CloseHandle.argtypes = [w.HANDLE]
    k32.WaitNamedPipeW.restype = w.BOOL
    k32.WaitNamedPipeW.argtypes = [w.LPCWSTR, w.DWORD]
    return k32


def wait_for_pipe(pipe_path: str, timeout: float = 10.0) -> None:
    """Poll WaitNamedPipeW until the pipe is ready or timeout expires.

    Args:
        pipe_path: Full Win32 pipe path, e.g. ``r'\\\\.\\pipe\\fauna-sync-test-a'``.
                   The agent's run_pipe_server uses the name verbatim — no
                   ``\\\\.\\pipe\\`` prefix is added for you.
        timeout: Seconds to wait before raising TimeoutError.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = _kernel32().WaitNamedPipeW(pipe_path, 100)
        if result:
            return
        time.sleep(0.1)
    raise TimeoutError(f"Pipe {pipe_path!r} not ready within {timeout}s")


def pipe_server_pid(pipe_path: str, timeout: float = 10.0) -> int:
    """The pid of the process serving ``pipe_path`` right now.

    "The pipe answers" never says WHICH process answers it: an agent spawned onto
    a pipe another agent already serves exits as a duplicate, while the pipe goes
    on answering. ``GetNamedPipeServerProcessId`` on a client handle is the OS's
    own answer — no wire verb, no guess. Opening the handle takes one server
    instance for an instant and sends nothing; the agent's accept loop sees a
    client that hung up.

    Raises:
        TimeoutError: nothing served the pipe (or every instance stayed busy)
            within ``timeout``.
        OSError: any other Win32 failure.
    """
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateFileW.restype = ctypes.wintypes.HANDLE
    kernel32.CreateFileW.argtypes = [
        ctypes.wintypes.LPCWSTR, ctypes.wintypes.DWORD, ctypes.wintypes.DWORD,
        ctypes.c_void_p, ctypes.wintypes.DWORD, ctypes.wintypes.DWORD,
        ctypes.wintypes.HANDLE,
    ]
    kernel32.GetNamedPipeServerProcessId.argtypes = [
        ctypes.wintypes.HANDLE, ctypes.POINTER(ctypes.wintypes.ULONG),
    ]
    kernel32.CloseHandle.argtypes = [ctypes.wintypes.HANDLE]
    kernel32.WaitNamedPipeW.argtypes = [ctypes.wintypes.LPCWSTR, ctypes.wintypes.DWORD]
    invalid = ctypes.wintypes.HANDLE(-1).value
    deadline = time.monotonic() + timeout
    while True:
        handle = kernel32.CreateFileW(
            pipe_path, _GENERIC_READ | _GENERIC_WRITE, 0, None, _OPEN_EXISTING, 0, None
        )
        if handle is not None and handle != invalid:
            break
        err = ctypes.get_last_error()
        if time.monotonic() >= deadline:
            raise TimeoutError(
                f"no process could be reached serving {pipe_path!r} within {timeout}s "
                f"(last Win32 error {err})"
            )
        if err == _ERROR_PIPE_BUSY:
            kernel32.WaitNamedPipeW(pipe_path, 100)
        else:
            time.sleep(0.1)
    try:
        pid = ctypes.wintypes.ULONG()
        if not kernel32.GetNamedPipeServerProcessId(handle, ctypes.byref(pid)):
            raise OSError(
                f"GetNamedPipeServerProcessId failed on {pipe_path!r}: "
                f"Win32 error {ctypes.get_last_error()}"
            )
        return pid.value
    finally:
        kernel32.CloseHandle(handle)


# How many pushed event frames `send_request` skips before giving up on a reply.
_MAX_EVENTS_BEFORE_REPLY = 1024


def _read_frame(handle, pipe_path: str) -> bytes:
    """Read one length-prefixed (u32 LE) frame's payload off ``handle``."""
    len_buf = (ctypes.c_char * 4)()
    read_count = ctypes.wintypes.DWORD()
    k32 = _kernel32()
    ok = k32.ReadFile(handle, len_buf, 4, ctypes.byref(read_count), None)
    if not ok or read_count.value != 4:
        err = ctypes.get_last_error()
        raise OSError(
            f"Failed to read response length from {pipe_path!r}: "
            f"got {read_count.value} bytes, Win32 error {err}"
        )
    length = struct.unpack("<I", bytes(len_buf))[0]
    buf = (ctypes.c_char * length)()
    ok = k32.ReadFile(handle, buf, length, ctypes.byref(read_count), None)
    if not ok or read_count.value != length:
        err = ctypes.get_last_error()
        raise OSError(
            f"Failed to read response payload from {pipe_path!r}: "
            f"expected {length} bytes, got {read_count.value}, Win32 error {err}"
        )
    return bytes(buf)


def _open_pipe(pipe_path: str, timeout: float = 10.0):
    """Open one client connection to ``pipe_path``, retrying inside ``timeout``
    while every server instance is busy or none is listening yet.

    The agent's accept loop mints each next server instance asynchronously, so an
    open that races it — right after :func:`pipe_server_pid` took an instance for
    an instant, or beside the app's own status polls — finds every instance busy
    (``ERROR_PIPE_BUSY``), or, before the agent's first instance stands, no pipe at
    all (``ERROR_FILE_NOT_FOUND``). Both are waited out here, the one retry every
    request shares (convention 14: the open asserts the agent answers, not that
    it won a race). Any other error, or either one past the deadline, raises.

    Raises:
        OSError: carrying the open's real Win32 error number.
    """
    k32 = _kernel32()
    invalid = ctypes.wintypes.HANDLE(-1).value
    deadline = time.monotonic() + timeout
    while True:
        handle = k32.CreateFileW(
            pipe_path, _GENERIC_READ | _GENERIC_WRITE, 0, None, _OPEN_EXISTING, 0, None
        )
        if handle is not None and handle != invalid:
            return handle
        err = ctypes.get_last_error()
        if err not in (_ERROR_PIPE_BUSY, _ERROR_FILE_NOT_FOUND):
            raise OSError(err, f"Cannot open pipe {pipe_path!r}: Win32 error {err}")
        if time.monotonic() >= deadline:
            raise OSError(
                err,
                f"Cannot open pipe {pipe_path!r} within {timeout}s: Win32 error {err} "
                f"({'every server instance stayed busy' if err == _ERROR_PIPE_BUSY else 'no server instance was listening'})",
            )
        if err == _ERROR_PIPE_BUSY:
            k32.WaitNamedPipeW(pipe_path, 100)
        else:
            time.sleep(0.1)  # sleep-ok: poll cadence of a deadline poll, not a settle wait


def _exchange(handle, pipe_path: str, frame: bytes) -> dict:
    """Write ``frame`` on an open connection and return its decoded Response."""
    written = ctypes.wintypes.DWORD()
    ok = _kernel32().WriteFile(handle, frame, len(frame), ctypes.byref(written), None)
    if not ok:
        err = ctypes.get_last_error()
        raise OSError(f"WriteFile failed on {pipe_path!r}: Win32 error {err}")

    # The agent broadcasts its events (`SyncProgress`, `FileStatusChanged`, …)
    # to every open connection, a request's own included, so a pushed event
    # frame can arrive before the reply (measured on Windows 2026-09-29: a
    # `GetFileStatus` read a `SyncProgress` event and failed `KeyError:
    # 'result'`). Skip event frames until the Response; bounded, so a
    # connection that only ever pushes events fails loudly instead of hanging.
    for _ in range(_MAX_EVENTS_BEFORE_REPLY):
        decoded = cbor2.loads(_read_frame(handle, pipe_path))
        if isinstance(decoded, dict) and "event" in decoded and "result" not in decoded:
            continue
        return decoded
    raise OSError(
        f"{pipe_path!r} pushed {_MAX_EVENTS_BEFORE_REPLY} event frames and no reply"
    )


def send_request(pipe_path: str, frame: bytes, timeout: float = 10.0) -> dict:
    """Send a framed request to the named pipe and return the decoded Response.

    Opens the pipe, writes the frame, reads back a length-prefixed dag-cbor
    response, decodes it, and returns the Response dict.

    Args:
        pipe_path: Full Win32 pipe path, e.g. ``r'\\\\.\\pipe\\fauna-sync-test-a'``.
        frame: A pre-built length-prefixed frame (from encode_frame).
        timeout: Seconds the open may wait out a busy or not-yet-listening
                 pipe (:func:`_open_pipe`); the exchange itself is synchronous.

    Returns:
        Decoded Response dict: {"id": <int>, "result": {"Ok": ...} | {"Err": str}}

    Raises:
        OSError: On any Win32 failure (open, WriteFile, ReadFile), carrying its
            real Win32 error number.
    """
    handle = _open_pipe(pipe_path, timeout)
    try:
        return _exchange(handle, pipe_path, frame)
    finally:
        _kernel32().CloseHandle(handle)


@contextmanager
def held_request(pipe_path: str, frame: bytes, timeout: float = 10.0):
    """Send a framed request and keep its connection open past the reply.

    Yields the decoded Response; the connection closes when the ``with`` body
    ends. For a request whose effect lasts exactly as long as the connection
    that carried it — ``AttachApp``'s lease, which the agent ends when the app's
    connection closes (a crash included).

    Raises:
        OSError: On any Win32 failure opening, writing or reading. The open waits
            out a busy or not-yet-listening pipe inside ``timeout``, as
            :func:`send_request`'s does.
    """
    handle = _open_pipe(pipe_path, timeout)
    try:
        yield _exchange(handle, pipe_path, frame)
    finally:
        _kernel32().CloseHandle(handle)


# ---------------------------------------------------------------------------
# Response decoding helpers
# ---------------------------------------------------------------------------

def unwrap_ok(resp: dict) -> object:
    """Extract the Ok payload from a Response dict.

    Args:
        resp: Decoded Response dict from send_request.

    Returns:
        The value under result["Ok"].

    Raises:
        RuntimeError: If result contains "Err".
        KeyError: If resp doesn't have the expected structure.
    """
    result = resp["result"]
    if "Err" in result:
        raise RuntimeError(result["Err"])
    return result["Ok"]


# ---------------------------------------------------------------------------
# High-level convenience functions (T3 imports these by name)
# ---------------------------------------------------------------------------

def provision(pipe_path: str, req_id: int, **caps) -> None:
    """Send a ProvisionCapability request and assert the response is Empty.

    Args:
        pipe_path: Full Win32 pipe path.
        req_id: Request correlation ID.
        **caps: Keyword arguments forwarded to provision_capability_request
                (backup_key, actor_id, nest_url, device_id, bearer_token,
                bearer_expires_at).

    Raises:
        RuntimeError: If the agent returns an Err response.
        AssertionError: If the Ok payload is not "Empty".
    """
    req = provision_capability_request(req_id, **caps)
    resp = send_request(pipe_path, encode_frame(req))
    payload = unwrap_ok(resp)
    if payload != "Empty":
        raise AssertionError(
            f"ProvisionCapability expected 'Empty' payload, got {payload!r}"
        )


def sync_status(pipe_path: str, req_id: int) -> dict:
    """Send a GetSyncStatus request and return the SyncStatusInfo dict.

    Args:
        pipe_path: Full Win32 pipe path.
        req_id: Request correlation ID.

    Returns:
        The inner SyncStatusInfo dict:
        {"connected": bool, "syncing": bool, "files_pending": int,
         "bytes_pending": int, "last_sync": int | None}

    Raises:
        RuntimeError: If the agent returns an Err response.
        KeyError: If the Ok payload doesn't contain "SyncStatus".
    """
    req = get_sync_status_request(req_id)
    resp = send_request(pipe_path, encode_frame(req))
    ok_payload = unwrap_ok(resp)
    return ok_payload["SyncStatus"]


# ---------------------------------------------------------------------------
# Optional ergonomics: thin client class (not required by T3, but convenient)
# ---------------------------------------------------------------------------

class SyncAgentClient:
    """Thin wrapper around the module-level functions with auto-incrementing IDs.

    Usage (pipe_path is the full Win32 path — fauna-sync named pipes live under
    the Win32 device namespace, so the path string starts with four backslashes
    followed by a dot, then ``pipe``, then the name)::

        client = SyncAgentClient(pipe_path)
        client.provision(backup_key=..., actor_id=..., ...)
        status = client.sync_status()
    """

    def __init__(self, pipe_path: str, timeout: float = 10.0):
        self.pipe_path = pipe_path
        self.timeout = timeout
        self._next_id = 1

    def _req_id(self) -> int:
        rid = self._next_id
        self._next_id += 1
        return rid

    def wait_for_pipe(self) -> None:
        wait_for_pipe(self.pipe_path, timeout=self.timeout)

    def provision(self, **caps) -> None:
        provision(self.pipe_path, self._req_id(), **caps)

    def sync_status(self) -> dict:
        return sync_status(self.pipe_path, self._req_id())

    def service_status(self) -> dict:
        req = get_service_status_request(self._req_id())
        resp = send_request(self.pipe_path, encode_frame(req), timeout=self.timeout)
        ok_payload = unwrap_ok(resp)
        return ok_payload["ServiceStatus"]

    # -- the peer-leg probe arm (test-only verbs, `fauna_ipc::sync`) --------

    def call(self, method) -> object:
        """One request whose ``Ok`` payload is returned as-is: ``method`` is
        the serde form of a ``RequestMethod`` — a bare name for a unit verb,
        ``{name: argument}`` for a verb that carries one."""
        frame = encode_frame({"id": self._req_id(), "method": method})
        return unwrap_ok(send_request(self.pipe_path, frame, timeout=self.timeout))

    def share_serve_info(self) -> list:
        """Each bound folder's ``{folder, folder_id, watch_dir, db_path}``
        (``GetShareServeInfo``) — ``folder_id`` is the folder's ``FolderRef``
        wire string."""
        return self.call("GetShareServeInfo")["ShareServeInfo"]

    def peer_leg_dial_target(self) -> dict:
        """Where this agent's same-account peer leg listens:
        ``{node_id, lan_endpoints}`` (test-only ``GetPeerLegDialTarget``)."""
        return self.call("GetPeerLegDialTarget")["PeerLegDialTarget"]

    def probe_peer_leg(self, target: dict, folder: str, *, relative_path=None,
                       store_keys=None) -> dict:
        """Dial ``target``'s peer leg as this agent's device and report what it
        served (test-only ``ProbePeerLeg``;
        ``fauna_ipc::sync::PeerLegProbeReport``)."""
        return self.call({"ProbePeerLeg": {
            "target": target,
            "folder": folder,
            "relative_path": relative_path,
            "store_keys": list(store_keys or []),
        }})["PeerLegProbe"]

    def arm_peer_serve_fault(self, bodies: int) -> None:
        """Make this agent's same-account serve door forge the next ``bodies``
        distinct chunk bodies it serves a sibling, one byte flipped in each
        (test-only ``ArmPeerServeFault``)."""
        self.call({"ArmPeerServeFault": {"bodies": int(bodies)}})
