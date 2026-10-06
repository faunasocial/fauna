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
import struct
import time

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
) -> dict:
    """Build a ProvisionCapability Request dict.

    Returns:
        {"id": req_id, "method": {"ProvisionCapability": <SyncCapability map>}}

    Field ordering is irrelevant — cbor2 canonical=True handles sorting.
    backup_key and actor_id are CBOR byte strings (major type 2); pass raw bytes.
    """
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


# ---------------------------------------------------------------------------
# Win32 named-pipe I/O
# ---------------------------------------------------------------------------

_GENERIC_READ = 0x80000000
_GENERIC_WRITE = 0x40000000
_OPEN_EXISTING = 3
_INVALID_HANDLE_VALUES = (-1, 0xFFFFFFFFFFFFFFFF)


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
        result = ctypes.windll.kernel32.WaitNamedPipeW(pipe_path, 100)
        if result:
            return
        time.sleep(0.1)
    raise TimeoutError(f"Pipe {pipe_path!r} not ready within {timeout}s")


_ERROR_PIPE_BUSY = 231


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
    ok = ctypes.windll.kernel32.ReadFile(handle, len_buf, 4, ctypes.byref(read_count), None)
    if not ok or read_count.value != 4:
        err = ctypes.get_last_error()
        raise OSError(
            f"Failed to read response length from {pipe_path!r}: "
            f"got {read_count.value} bytes, Win32 error {err}"
        )
    length = struct.unpack("<I", bytes(len_buf))[0]
    buf = (ctypes.c_char * length)()
    ok = ctypes.windll.kernel32.ReadFile(handle, buf, length, ctypes.byref(read_count), None)
    if not ok or read_count.value != length:
        err = ctypes.get_last_error()
        raise OSError(
            f"Failed to read response payload from {pipe_path!r}: "
            f"expected {length} bytes, got {read_count.value}, Win32 error {err}"
        )
    return bytes(buf)


def send_request(pipe_path: str, frame: bytes, timeout: float = 10.0) -> dict:
    """Send a framed request to the named pipe and return the decoded Response.

    Opens the pipe, writes the frame, reads back a length-prefixed dag-cbor
    response, decodes it, and returns the Response dict.

    Args:
        pipe_path: Full Win32 pipe path, e.g. ``r'\\\\.\\pipe\\fauna-sync-test-a'``.
        frame: A pre-built length-prefixed frame (from encode_frame).
        timeout: Not used for the Win32 synchronous call (kept for API parity
                 with the T3 test harness signature).

    Returns:
        Decoded Response dict: {"id": <int>, "result": {"Ok": ...} | {"Err": str}}

    Raises:
        OSError: On any Win32 failure (bad handle, WriteFile, ReadFile).
    """
    handle = ctypes.windll.kernel32.CreateFileW(
        pipe_path,
        _GENERIC_READ | _GENERIC_WRITE,
        0,
        None,
        _OPEN_EXISTING,
        0,
        None,
    )
    if handle in _INVALID_HANDLE_VALUES:
        err = ctypes.get_last_error()
        raise OSError(f"Cannot open pipe {pipe_path!r}: Win32 error {err}")

    try:
        # Write request frame
        written = ctypes.wintypes.DWORD()
        ok = ctypes.windll.kernel32.WriteFile(
            handle, frame, len(frame), ctypes.byref(written), None
        )
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
    finally:
        ctypes.windll.kernel32.CloseHandle(handle)


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
