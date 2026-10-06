"""Minimal in-process fake rspamd for the T1.4 content-scan tier_3 tests.

The MTA bridge's scan gate POSTs the raw message to ``{rspamd_url}/checkv2``
and parses the JSON response (top-level ``score`` float + optional ``symbols``
map) into a scaled milli-int score. This fake answers ``/checkv2`` with a
fixed low-score JSON so rspamd never gates delivery (it does not gate in T1.4
anyway — the score is stored + header-stamped only). Like ``helpers/stub_mx``
and ``fakes/fake_clamd``, it is a real external HTTP daemon the bridge talks to
over the wire, keeping the scan test tier_3.

A message may name its own score with an ``X-Test-Rspamd-Score: <float>``
header, standing in for content the shared rules would score that high — the
one way a test makes the deployment's shared rules call a message spam. Every
other message gets the fixed benign score.
"""

from __future__ import annotations

import json
import re
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

# A benign content score (rspamd's native ~0–30 scale). At the default 0.5
# scaling this maps to a scaled ~0.75 — well below any threshold, so every
# message rspamd sees stays deliverable.
_DEFAULT_SCORE = 1.5
_DEFAULT_SYMBOLS = {"BAYES_HAM": {"score": -2.9}, "MIME_GOOD": {"score": -0.1}}
_SCORE_HEADER = re.compile(rb"^X-Test-Rspamd-Score:[ \t]*([0-9.]+)[ \t]*\r?$", re.I | re.M)


def _make_handler(score: float, symbols: dict) -> type[BaseHTTPRequestHandler]:
    class _Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802 (http.server API)
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length) if length else b""
            if not self.path.startswith("/checkv2"):
                self.send_response(404)
                self.end_headers()
                return
            head = body.split(b"\r\n\r\n", 1)[0]
            named = _SCORE_HEADER.search(head)
            payload = json.dumps({
                "score": float(named.group(1)) if named else score,
                "symbols": symbols,
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *_args: object) -> None:
            pass  # quiet — don't spam the pytest output

    return _Handler


class FakeRspamd:
    """A loopback rspamd answering /checkv2 with a fixed benign score."""

    def __init__(self, score: float = _DEFAULT_SCORE, symbols: dict | None = None,
                 host: str = "127.0.0.1", port: int = 0) -> None:
        # `host`/`port` are the bind interface. Process-level tests keep the
        # default loopback + ephemeral port. The docker round-trip runs this as a
        # sidecar *container* on a user-defined network (the nest container can't
        # reach host listeners on this docker setup), binding "0.0.0.0" on a fixed
        # port the nest reaches by container name (e.g. `http://rspamd:11333`).
        handler = _make_handler(score, symbols if symbols is not None else _DEFAULT_SYMBOLS)
        self._server = HTTPServer((host, port), handler)
        self.host, self.port = self._server.server_address[:2]
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        """Base URL for the operator-hatch ``rspamd_url`` value."""
        return f"http://{self.host}:{self.port}"

    def start(self) -> "FakeRspamd":
        self._thread.start()
        return self

    def stop(self) -> None:
        try:
            self._server.shutdown()
            self._server.server_close()
        except OSError:
            pass
