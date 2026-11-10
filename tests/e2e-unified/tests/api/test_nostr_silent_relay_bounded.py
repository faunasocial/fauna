"""A relay that accepts the nest's connection and then never answers costs the
nest one bound, is sat out, and is tried again later — while publishing and
refresh go on through the user's other relay. Witness of `docs/features/nostr.md`
outcome 29, its never-answers arm.

`nest/network-exposure.md` § Rulings F8 owns the ruling: no relay the nest
dials may hold a dialer past a bound — `RelayClient::connect` gives resolve,
TCP connect and the WebSocket handshake together `RELAY_CONNECT_TIMEOUT`
(10 s, `libs/fauna-bridge-nostr/src/relay_client.rs`) — and the sync worker's
pool (`bins/fauna-nest/src/nostr/relay_pool.rs`) dials and publishes to every
relay of a list at once, then skips a relay whose dial failed until its
backoff lapses (`RELAY_BACKOFF_INITIAL`, 30 s, doubling to 15 min). The sync
worker is one loop for every account on the nest, so before the fix one silent
relay on one user's list stalled outbound publish and inbound refresh for
everyone.

The silent relay is a loopback listener inside this test: the kernel completes
the TCP handshake, the test reads the nest's WebSocket upgrade request and never
answers it. What it observes is all the nest does to it — each connection
accepted, the bytes the nest sent, and the nest hanging up. The healthy relay
beside it on the same list is a :class:`~helpers.nostr_relay.FakeRelay`. Four
observations, none timed against the bound (convention 14 — the bound only sizes
the hang guards):

1. a post goes out to the healthy relay although the silent one never answered —
   so the dial to it was given up on (and the nest is seen hanging up on it);
2. the next post goes out with no new connection to the silent relay — it is
   sat out (anchored on that post's arrival: a pass dials before it publishes,
   so a redial would have been accepted first);
3. a followed author's note served by the healthy relay reaches the feed on the
   inbound tick — refresh keeps going with the silent relay among the author's
   relay hints;
4. once its backoff has lapsed, the nest dials the silent relay again, and gives
   up on it again.

The stops-reading and never-pauses arms of the outcome (F8 (a′)) are pinned by
the Rust tests the ruling's status line names; a Python fake cannot shrink its
own receive buffer the way `a_relay_that_stops_reading_is_given_up_on_within_the_send_bound` does.

tier_3: a locally built `fauna-nest` (``nostr_dialing_nest`` — a loopback relay
is dialable only under `test-hooks` + `FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`, F7 (c)),
driven over its production doors; no app.
"""

from __future__ import annotations

import os
import secrets
import socket
import threading
import time

import pytest

import fauna_ffi
from helpers.budgets import NOSTR_INBOUND_TICK_S, RPC_ROUNDTRIP_S
from helpers.nostr_relay import FakeRelay, await_received
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.test_nostr_nest import (  # noqa: F401 — fixture
    _link_generated,
    _post,
    _settings,
    _user,
    nostr_dialing_nest,
)

pytestmark = pytest.mark.tier_3

#: `fauna_bridge_nostr::relay_client::RELAY_CONNECT_TIMEOUT`
#: (`libs/fauna-bridge-nostr/src/relay_client.rs:110`) — the bound on a dial.
RELAY_CONNECT_TIMEOUT_S = 10.0
#: `nostr::relay_pool::RELAY_BACKOFF_INITIAL`
#: (`bins/fauna-nest/src/nostr/relay_pool.rs:33`) — how long a relay whose dial
#: failed first sits out.
RELAY_BACKOFF_INITIAL_S = 30.0


class SilentRelay:
    """A loopback listener that accepts every connection, reads what the nest
    sends, and never answers — the tarpit F8 names."""

    def __init__(self) -> None:
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(16)
        self.port = self._sock.getsockname()[1]
        self._lock = threading.Lock()
        self._conns: list[socket.socket] = []
        #: One entry per accepted connection: ``{"sent": bytes, "hung_up_at":
        #: monotonic seconds or None}``.
        self.connections: list[dict] = []
        self._closed = threading.Event()
        threading.Thread(target=self._accept_loop, daemon=True).start()

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.port}"

    def snapshot(self) -> list[dict]:
        with self._lock:
            return [dict(c) for c in self.connections]

    def close(self) -> None:
        self._closed.set()
        for s in [self._sock, *self._conns]:
            try:
                s.close()
            except OSError:
                pass

    def _accept_loop(self) -> None:
        while not self._closed.is_set():
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return
            record = {"sent": b"", "hung_up_at": None}
            with self._lock:
                self._conns.append(conn)
                self.connections.append(record)
            threading.Thread(target=self._listen, args=(conn, record), daemon=True).start()

    def _listen(self, conn: socket.socket, record: dict) -> None:
        """Read until the nest hangs up; answer nothing."""
        while True:
            try:
                chunk = conn.recv(4096)
            except OSError:
                chunk = b""
            with self._lock:
                if not chunk:
                    if not self._closed.is_set():
                        record["hung_up_at"] = time.monotonic()
                    return
                record["sent"] += chunk


@pytest.fixture
def silent_relay():
    relay = SilentRelay()
    yield relay
    relay.close()


@pytest.fixture
def healthy_relay():
    relay = FakeRelay()
    yield relay
    relay.close()


def _describe(connections: list[dict]) -> str:
    return repr([
        {"sent": c["sent"][:60], "hung_up": c["hung_up_at"] is not None} for c in connections
    ])


def _await_hang_up(silent: SilentRelay, index: int, budget_s: float, what: str) -> dict:
    return wait_until(
        lambda: (lambda cs: cs[index] if len(cs) > index and cs[index]["hung_up_at"] else None)(
            silent.snapshot()
        ),
        budget_s,
        diagnose=lambda: (
            f"{what}: the nest still holds the silent relay's connection #{index + 1} "
            f"open past the bound; connections: {_describe(silent.snapshot())}"
        ),
    )


@pytest.mark.feature("nostr")
def test_a_relay_that_never_answers_is_given_up_on_sat_out_and_tried_again_later(
    nostr_dialing_nest, silent_relay, healthy_relay,
):
    """Outcome 29, the never-answers arm (`nest/network-exposure.md` § Rulings F8)."""
    nest = nostr_dialing_nest
    user = _user(nest)
    pubkey = _link_generated(nest, user)
    _settings(
        nest, user, relay_list=[silent_relay.url, healthy_relay.url], auto_publish=True,
    )

    def _published(text: str, budget_s: float) -> dict:
        return await_received(
            healthy_relay,
            lambda e: e["pubkey"] == pubkey and e["kind"] == 1 and text in e["content"],
            budget_s,
            f"the note {text!r} (silent relay connections: {_describe(silent_relay.snapshot())})",
        )

    # 1. Given up on within the bound: the healthy relay gets the post, and the
    #    nest hangs up on the relay that never answered its upgrade.
    first = f"past a silent relay {secrets.token_hex(4)}"
    _post(nest, user, first)
    _published(first, RELAY_CONNECT_TIMEOUT_S + RPC_ROUNDTRIP_S)
    given_up = _await_hang_up(silent_relay, 0, RPC_ROUNDTRIP_S, "the first dial")
    assert given_up["sent"].startswith(b"GET "), (
        "the nest reached the silent relay and asked it for the WebSocket upgrade "
        f"it never got: {_describe(silent_relay.snapshot())}"
    )
    dialed_after_first = len(silent_relay.snapshot())

    # 2. Sat out: the next post goes out with no new dial to the silent relay.
    second = f"while the silent relay sits out {secrets.token_hex(4)}"
    _post(nest, user, second)
    _published(second, RELAY_CONNECT_TIMEOUT_S + RPC_ROUNDTRIP_S)
    since_hang_up = time.monotonic() - given_up["hung_up_at"]
    assert len(silent_relay.snapshot()) == dialed_after_first, (
        "a relay whose dial just failed is skipped until its backoff lapses "
        f"({RELAY_BACKOFF_INITIAL_S:.0f}s; the second post arrived {since_hang_up:.1f}s "
        f"after the hang-up); connections: {_describe(silent_relay.snapshot())}"
    )

    # 3 + 4. Refresh keeps going, and the silent relay is tried again later: a
    #    followed author's relay hints name both relays; the healthy one serves
    #    the author's note into the feed on the inbound tick, and a tick after
    #    the backoff lapsed dials the silent relay again.
    feed_id = ws_api.create_feed(nest["port"], user, "Everything", rules=[])
    author_secret = os.urandom(32)
    author = fauna_ffi.nostr_keys(author_secret)
    note = fauna_ffi.nostr_sign_event(author_secret, {
        "kind": 1, "created_at": int(time.time()) - 30, "tags": [],
        "content": f"served beside a silent relay {secrets.token_hex(4)}",
    })
    healthy_relay.seed(note)
    ws_api.bridge_add_follow(
        nest["port"], user, "nostr", author["npub"],
        extra={"relay_hints": [silent_relay.url, healthy_relay.url]},
    )

    def _bodies() -> set[str]:
        return {p["body"] for p in ws_api.feed_posts(nest["port"], user, feed_id)}

    wait_until(
        lambda: note["content"] in _bodies() and len(silent_relay.snapshot()) > dialed_after_first,
        NOSTR_INBOUND_TICK_S + RELAY_CONNECT_TIMEOUT_S,
        diagnose=lambda: (
            f"note in feed: {note['content'] in _bodies()}; the healthy relay saw REQs "
            f"{healthy_relay.requests!r}; silent relay connections (want more than "
            f"{dialed_after_first}): {_describe(silent_relay.snapshot())}"
        ),
    )
    _await_hang_up(
        silent_relay, dialed_after_first, RELAY_CONNECT_TIMEOUT_S + RPC_ROUNDTRIP_S,
        "the redial after the backoff",
    )
