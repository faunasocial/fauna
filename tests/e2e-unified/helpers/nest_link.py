"""A seat's link to its nest that the test can cut and restore — the causal way
to keep a device's changes off the nest while the device keeps running.

A seat launched against :attr:`NestLink.url` reaches the real nest through this
in-process TCP relay. :meth:`NestLink.cut` closes every connection the relay
carries and refuses new ones (each accepted socket is closed at once, so the
app reads a dropped connection exactly as it reads a nest that went away);
:meth:`NestLink.restore` lets connections through again. Nothing here is timed:
while the link is cut no byte of the seat's can reach the nest, whatever the
seat's push cadence, which is what a "change that never reached your nest"
witness needs (convention 14 — the hold is a cause, never a race).

Pure harness code: threads in the test process, no child process, nothing
killed (the process-safety rule). The listening port comes from
``drivers/port_util.find_free_port``'s convention of binding port 0.
"""

from __future__ import annotations

import socket
import threading
from urllib.parse import urlsplit

#: Chunk size of one relayed read.
_CHUNK = 64 * 1024


class NestLink:
    """A cuttable TCP relay in front of ``target_url``'s host and port."""

    def __init__(self, target_url: str) -> None:
        parts = urlsplit(target_url)
        self._target = (parts.hostname or "127.0.0.1", parts.port or 80)
        self._scheme = parts.scheme or "http"
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(64)
        self.port = self._listener.getsockname()[1]
        self.url = f"{self._scheme}://127.0.0.1:{self.port}"
        self._lock = threading.Lock()
        self._open = True
        self._closed = False
        self._live: set[socket.socket] = set()
        #: Connections carried through, and refused while cut — the run's
        #: record of what the link did (convention 6).
        self.carried = 0
        self.refused = 0
        self._acceptor = threading.Thread(
            target=self._accept_loop, name=f"nest-link-{self.port}", daemon=True
        )
        self._acceptor.start()

    # ── the two gestures ───────────────────────────────────────────────────

    def cut(self) -> None:
        """Close every carried connection and refuse new ones until
        :meth:`restore`."""
        with self._lock:
            self._open = False
            live, self._live = self._live, set()
        for sock in live:
            _close(sock)

    def restore(self) -> None:
        """Carry connections to the nest again."""
        with self._lock:
            self._open = True

    @property
    def is_cut(self) -> bool:
        with self._lock:
            return not self._open

    def live_connections(self) -> int:
        """How many relayed socket ends are open now (two per connection)."""
        with self._lock:
            return len(self._live)

    def describe(self) -> str:
        state = "CUT" if self.is_cut else "open"
        return (
            f"nest link {self.url} -> {self._target[0]}:{self._target[1]}: {state}, "
            f"{self.carried} connection(s) carried, {self.refused} refused while cut, "
            f"{self.live_connections() // 2} open now"
        )

    # ── lifecycle ──────────────────────────────────────────────────────────

    def close(self) -> None:
        self._closed = True
        self.cut()
        _close(self._listener)

    def __enter__(self) -> "NestLink":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # ── the relay ──────────────────────────────────────────────────────────

    def _accept_loop(self) -> None:
        while not self._closed:
            try:
                client, _ = self._listener.accept()
            except OSError:
                return
            with self._lock:
                open_now = self._open
            if not open_now:
                self.refused += 1
                _close(client)
                continue
            try:
                upstream = socket.create_connection(self._target, timeout=10.0)
                upstream.settimeout(None)
            except OSError:
                _close(client)
                continue
            with self._lock:
                if not self._open:
                    # Cut between the accept and the dial: refuse it too.
                    self.refused += 1
                    _close(client)
                    _close(upstream)
                    continue
                self._live.update((client, upstream))
                self.carried += 1
            for src, dst in ((client, upstream), (upstream, client)):
                threading.Thread(
                    target=self._pump, args=(src, dst), daemon=True,
                    name=f"nest-link-{self.port}-pump",
                ).start()

    def _pump(self, src: socket.socket, dst: socket.socket) -> None:
        try:
            while True:
                data = src.recv(_CHUNK)
                if not data:
                    break
                dst.sendall(data)
        except OSError:
            pass
        finally:
            with self._lock:
                self._live.discard(src)
                self._live.discard(dst)
            _close(src)
            _close(dst)


def _close(sock: socket.socket) -> None:
    try:
        sock.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass
    try:
        sock.close()
    except OSError:
        pass
