"""The web bridge's port choice is collision-proof (convention 10, bounded
distinct-port retry; `docs/goal/architecture/e2e-launch-isolation.md`).

`http.server.HTTPServer` defaults `allow_reuse_address = True`, which on
Windows lets a second server co-bind a port a first one holds with no error —
a silent dual listener nothing can retry on. The bridge's server class turns
that off so a held port is always an `OSError`, and `bind_fresh_port` retries
on it with a fresh draw. Pure in-process: real sockets on loopback, no
bridge process, no app.
"""

import importlib.util
import pathlib
import socket

import pytest

pytestmark = pytest.mark.tier_1


def _bridge():
    server_path = (
        pathlib.Path(__file__).resolve().parents[1] / "web-bridge" / "server.py"
    )
    spec = importlib.util.spec_from_file_location("fauna_e2e_web_bridge_port", server_path)
    bridge = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bridge)
    return bridge


def _held_socket():
    """A listener standing in for another bridge: bound, listening, port held."""
    holder = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    holder.bind(("127.0.0.1", 0))
    holder.listen(1)
    return holder, holder.getsockname()[1]


def _free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_the_bridge_server_never_reuses_an_address():
    bridge = _bridge()
    assert bridge.BridgeServer.allow_reuse_address is False, (
        "SO_REUSEADDR lets a second HTTPServer co-bind silently on Windows; "
        "a collision must always be an OSError"
    )


def test_a_held_port_is_a_detectable_bind_error():
    bridge = _bridge()
    holder, port = _held_socket()
    try:
        with pytest.raises(OSError):
            bridge.BridgeServer(("127.0.0.1", port), bridge.BridgeHandler)
    finally:
        holder.close()


def test_a_taken_draw_is_skipped_for_the_next_free_one():
    bridge = _bridge()
    holder, taken = _held_socket()
    free = _free_port()
    draws = iter([taken, free])
    try:
        server, port = bridge.bind_fresh_port(lambda: next(draws))
        try:
            assert port == free, "must land on the free port, not the held one"
            assert server.server_address[1] == free
        finally:
            server.server_close()
    finally:
        holder.close()


def test_every_draw_taken_fails_loudly_after_the_bound():
    bridge = _bridge()
    holder, taken = _held_socket()
    calls = []

    def draw():
        calls.append(1)
        return taken

    try:
        with pytest.raises(RuntimeError):
            bridge.bind_fresh_port(draw)
    finally:
        holder.close()
    assert len(calls) == bridge.PORT_MAX_ATTEMPTS == 20
