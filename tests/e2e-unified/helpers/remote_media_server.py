"""A remote media file on a real ``https`` hostname, for a tier_3 journey that
plays a bridged video through the nest's media proxy.

A bridged post's attachment rests as the proxy's nest-relative path
(``/api/v1/media/proxy?url=https://…``), and the proxy dials only ``https`` on
global addresses (``ssrf.rs``) — so a file served from loopback is reachable
only through the proxy's two ``test-hooks`` seams:
``FAUNA_TEST_MEDIA_PROXY_RESOLVE_JSON`` (``{host: "127.0.0.1:<port>"}``) and
``FAUNA_TEST_MEDIA_PROXY_EXTRA_CA_PEM`` (the CA's path). Both are loopback-only
and mapped-host-only on the nest side (``ssrf::test_resolve_client``); an
unmapped host still meets the full guard, and production builds no
``test-hooks``.

A single-range ``Range: bytes=a-b`` / ``bytes=a-`` / ``bytes=-n`` is answered
``206`` with its ``Content-Range``, as a real media host answers it (the proxy
relays the ``206``); anything else gets the whole body. AVFoundation refuses an
HTTP source that never answers a range (``AVErrorServerIncorrectlyConfigured``),
so a whole-body-only remote would fail every apple leg on the helper alone.
"""

from __future__ import annotations

import http.server
import json
import ssl
import threading
from pathlib import Path

from drivers.port_util import find_free_port
from helpers.client_metadata_server import _ca_and_leaf


class RemoteMediaServer:
    """Serves ``files`` (``{path: (bytes, content_type)}``) for ``host`` over
    HTTPS on loopback, under a throwaway CA. Every GET's path is recorded in
    ``requests`` — the witness that a byte load reached the remote."""

    def __init__(self, host: str, tmp: Path, files: dict[str, tuple[bytes, str]]):
        self.host = host
        self.files = files
        self.requests: list[str] = []
        self.port = find_free_port()
        self.ca_pem, cert_pem, key_pem = _ca_and_leaf([host], tmp)

        server = self

        class _Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802 — the stdlib's name
                server.requests.append(self.path)
                entry = server.files.get(self.path)
                if entry is None:
                    self.send_error(404)
                    return
                body, content_type = entry
                span = _byte_range(self.headers.get("Range"), len(body))
                if span == "unsatisfiable":
                    self.send_response(416)
                    self.send_header("Content-Range", f"bytes */{len(body)}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if span is None:
                    self.send_response(200)
                    part = body
                else:
                    start, end = span
                    part = body[start:end + 1]
                    self.send_response(206)
                    self.send_header("Content-Range", f"bytes {start}-{end}/{len(body)}")
                self.send_header("Accept-Ranges", "bytes")
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(part)))
                self.end_headers()
                self.wfile.write(part)

            def log_message(self, *_args):
                pass

        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert_pem, key_pem)
        self._httpd = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), _Handler)
        self._httpd.socket = ctx.wrap_socket(self._httpd.socket, server_side=True)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        self._thread.start()

    def url(self, path: str) -> str:
        """The remote ``https`` url a bridged post's attachment names."""
        return f"https://{self.host}{path}"

    def nest_env(self) -> dict[str, str]:
        """The two ``test-hooks`` seams the nest's media proxy reads."""
        return {
            "FAUNA_TEST_MEDIA_PROXY_RESOLVE_JSON": json.dumps(
                {self.host: f"127.0.0.1:{self.port}"}
            ),
            "FAUNA_TEST_MEDIA_PROXY_EXTRA_CA_PEM": str(self.ca_pem),
        }

    def close(self) -> None:
        self._httpd.shutdown()
        self._httpd.server_close()


def _byte_range(header: str | None, size: int):
    """``(start, end)`` inclusive for a single satisfiable ``bytes=`` range,
    ``"unsatisfiable"`` for one past the end, ``None`` for no (or an
    unparseable, or a multi-) range — which gets the whole body."""
    if not header or not header.startswith("bytes=") or "," in header:
        return None
    first, _, last = header[len("bytes="):].strip().partition("-")
    try:
        if not first:
            start, end = max(size - int(last), 0), size - 1
        else:
            start = int(first)
            end = min(int(last), size - 1) if last else size - 1
    except ValueError:
        return None
    if start >= size or start > end:
        return "unsatisfiable"
    return start, end
