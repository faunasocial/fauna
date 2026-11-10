"""tier_1 gates for ``helpers/nest_link.py`` — the cuttable relay a journey puts
between a seat and its nest to hold the seat's changes off the nest causally.

Decided in-process against a local echo server: the link carries bytes both
ways while open, a cut drops what it carries AND refuses what comes after it,
and a restore carries again. No nest, no app, no clock beyond socket timeouts
that bound a broken relay (convention 9).
"""

from __future__ import annotations

import socket
import threading

import pytest

from helpers.nest_link import NestLink

pytestmark = [pytest.mark.tier_1]


@pytest.fixture
def echo_url():
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 0))
    server.listen(16)
    stop = threading.Event()

    def serve(conn):
        with conn:
            while True:
                try:
                    data = conn.recv(4096)
                except OSError:
                    return
                if not data:
                    return
                conn.sendall(data)

    def accept():
        while not stop.is_set():
            try:
                conn, _ = server.accept()
            except OSError:
                return
            threading.Thread(target=serve, args=(conn,), daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()
    yield f"http://127.0.0.1:{server.getsockname()[1]}"
    stop.set()
    server.close()


def _connect(link: NestLink) -> socket.socket:
    return socket.create_connection(("127.0.0.1", link.port), timeout=5.0)


def _round_trip(sock: socket.socket, payload: bytes) -> bytes:
    sock.sendall(payload)
    got = b""
    while len(got) < len(payload):
        chunk = sock.recv(4096)
        if not chunk:
            break
        got += chunk
    return got


def _reads_closed(sock: socket.socket) -> bool:
    """True when the peer closed: a read returns EOF or resets."""
    try:
        return sock.recv(1) == b""
    except (ConnectionResetError, BrokenPipeError):
        return True


def test_an_open_link_carries_bytes_both_ways(echo_url):
    with NestLink(echo_url) as link:
        assert link.url == f"http://127.0.0.1:{link.port}"
        with _connect(link) as sock:
            assert _round_trip(sock, b"hello nest") == b"hello nest"
        assert link.carried == 1 and link.refused == 0


def test_a_cut_drops_what_it_carries_and_refuses_what_follows(echo_url):
    with NestLink(echo_url) as link:
        live = _connect(link)
        assert _round_trip(live, b"before") == b"before"
        link.cut()
        assert link.is_cut
        assert _reads_closed(live), "a cut must close the connection it carried"
        live.close()

        with _connect(link) as refused:
            assert _reads_closed(refused), "a cut link must refuse a new connection"
        assert link.refused >= 1
        assert link.carried == 1, link.describe()


def test_a_restored_link_carries_again(echo_url):
    with NestLink(echo_url) as link:
        link.cut()
        with _connect(link) as refused:
            assert _reads_closed(refused)
        link.restore()
        assert not link.is_cut
        with _connect(link) as sock:
            assert _round_trip(sock, b"after") == b"after"
        assert link.carried == 1 and link.refused == 1, link.describe()
