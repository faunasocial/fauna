"""fake_cloud's HTTP fixture must not wedge under ordinary idle-connection load
(e2e-conventions.md point 9 "the harness is self-terminating" and point 6
"failures must diagnose themselves").

Nothing has to misbehave to trigger this: a browser's preconnect, an HTTP
client's connection pool, or a leftover socket from a prior test are all
completely ordinary. Before the fix, even ONE idle bare connection (no
request ever sent on it) made the server stop answering every OTHER client
entirely -- the failure then presents as "the app is hanging", with the fake
looking healthy and logging nothing, which cost `test_bundled_provider.py`
seven runs of misdiagnosis across three sessions before the stall was traced
to this fixture rather than the app.
"""
from __future__ import annotations

import socket
from urllib.parse import urlparse

import pytest
import requests
from pytest_httpserver import HTTPServer

pytestmark = pytest.mark.tier_1


def test_an_ordinary_request_is_served_promptly_under_idle_squatting_connections(
    httpserver: HTTPServer,
) -> None:
    httpserver.expect_request("/ping").respond_with_data("pong")
    parsed = urlparse(httpserver.url_for("/ping"))

    # N well above any plausible real client (a browser's preconnect pool, an
    # HTTP client's connection pool, a stray curl left open) — convention 14:
    # a generous fixed ceiling, never a measured duration used as the verdict.
    squatters = [
        socket.create_connection((parsed.hostname, parsed.port), timeout=5)
        for _ in range(32)
    ]
    try:
        resp = requests.get(parsed.geturl(), timeout=5)
    finally:
        for s in squatters:
            s.close()
    assert resp.status_code == 200
    assert resp.text == "pong"
