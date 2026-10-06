"""Minimal in-process fake clamd (ClamAV daemon) for the T1.4 content-scan
tier_3 tests.

The MTA bridge's scan gate (`bins/fauna-bridges/internal/mta/scan_gate.go`)
dials clamd over a socket and runs an INSTREAM scan: it writes ``zINSTREAM\\0``,
then a sequence of ``<uint32-BE length><chunk>`` frames terminated by a
zero-length frame, and reads a null-terminated reply line. This fake speaks
exactly that wire and replies ``stream: <sig> FOUND`` when the streamed body
contains the infection marker, else ``stream: OK``.

Like ``helpers/stub_mx.StubMX``, this is not a fake of a fauna binary — it is a
real external daemon the bridge talks to over a socket, which is why the scan
test stays tier_3 (real nest, real bridge, real wire; only clamd/rspamd, the
non-fauna external daemons, are stubbed). A later tier_3 variant against a real
co-located clamd would use the genuine EICAR test string; this fake uses a
plain marker so no antivirus on the dev box quarantines the test source.
"""

from __future__ import annotations

import socket
import struct
import threading

# Body marker that triggers an "infected" verdict. The reject test sends a
# message whose body contains this; every other message scans clean. (A real
# clamd would key on the EICAR signature; the fake keys on this marker so the
# literal EICAR string never appears in the test sources.)
INFECTED_MARKER = b"fauna-scan-test-infected"

# The signature name the fake reports for a marker hit — surfaces in the
# bridge's ``554 5.7.1 Message contains malware: <sig>`` reply.
INFECTED_SIGNATURE = "Fauna-Test-Eicar-Signature"


class FakeClamd:
    """A loopback clamd that answers a single INSTREAM scan per connection."""

    def __init__(self, host: str = "127.0.0.1", port: int = 0) -> None:
        # `host`/`port` are the bind interface. Process-level tests keep the
        # default loopback + ephemeral port. The docker round-trip runs this as a
        # sidecar *container* on a user-defined network (the nest container can't
        # reach host listeners on this docker setup), binding "0.0.0.0" on a fixed
        # port the nest dials by container name (e.g. `clamd:3310`).
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind((host, port))
        self._sock.listen(8)
        self.host, self.port = self._sock.getsockname()[:2]
        self._stopped = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)

    @property
    def addr(self) -> str:
        """`host:port` for the operator-hatch ``clamd_addr`` value."""
        return f"{self.host}:{self.port}"

    def start(self) -> "FakeClamd":
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stopped.set()
        try:
            self._sock.close()
        except OSError:
            pass

    # ── internals ─────────────────────────────────────────────────────

    def _serve(self) -> None:
        while not self._stopped.is_set():
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return  # socket closed by stop()
            threading.Thread(target=self._handle, args=(conn,), daemon=True).start()

    def _handle(self, conn: socket.socket) -> None:
        with conn:
            conn.settimeout(10.0)
            try:
                self._scan(conn)
            except (OSError, struct.error):
                return

    def _scan(self, conn: socket.socket) -> None:
        buf = bytearray()

        def read_exactly(n: int) -> bytes:
            while len(buf) < n:
                chunk = conn.recv(4096)
                if not chunk:
                    raise OSError("clamd client closed mid-stream")
                buf.extend(chunk)
            out = bytes(buf[:n])
            del buf[:n]
            return out

        def read_until_nul() -> None:
            # Consume the "zINSTREAM\0" command line.
            while b"\x00" not in buf:
                chunk = conn.recv(4096)
                if not chunk:
                    raise OSError("clamd client closed before command")
                buf.extend(chunk)
            idx = buf.index(b"\x00")
            del buf[: idx + 1]

        read_until_nul()
        body = bytearray()
        while True:
            length = struct.unpack(">I", read_exactly(4))[0]
            if length == 0:
                break
            body.extend(read_exactly(length))

        if INFECTED_MARKER in bytes(body):
            reply = f"stream: {INFECTED_SIGNATURE} FOUND\x00"
        else:
            reply = "stream: OK\x00"
        conn.sendall(reply.encode())
