"""The harness's process-wide bearer cache spends one refusal per sign-in lost,
never one per client instance.

``WsRpcAdminClient`` keeps one bearer per (nest, actor) for the whole pytest
process (``clients/ws_rpc_admin_client.py::_BEARERS``), and many instances of
it live at once — the ``_authed_call`` cache, the ``tests/api`` per-actor
caches, a fixture's own client. A nest restart forgets every bearer, so each
live instance's next dial is refused ``401`` and re-minted once
(``transport-connection.md`` § Connection lifecycle → *Upgrade-time auth
rejection*). Those refusals spend the nest's failed-credential bucket for the
actor on loopback (``transport-connection.md`` § Abuse posture → *The
failed-credential throttle*: 10 a minute), and once it is full the next
stale dial is answered ``429``, which no re-mint follows. A whole-module
``test_family.py`` run on macos measured exactly that: about ten admin
re-mints in the 1.5 s after a ``restart_nest``, then three tests red on
``Handshake status 429``.

So the second instance must take the bearer the first one re-minted rather
than present its own copy of the dead one — and must not throw the fresh one
away when its own dead copy is refused. No nest: the dial and the mint are
stand-ins, which is all the property is about.
"""

from __future__ import annotations

import time

import pytest
import websocket

from clients import ws_rpc_admin_client as admin_client_mod
from clients.ws_rpc_admin_client import WsRpcAdminClient, _BearerToken

pytestmark = pytest.mark.tier_1

_NEST = "http://127.0.0.1:9"


class _Nest:
    """The bearers a stand-in nest knows, and what each client did to it."""

    def __init__(self) -> None:
        self.known: set[str] = set()
        self.refusals = 0
        self.mints = 0

    def restart(self) -> None:
        self.known.clear()


class _Client(WsRpcAdminClient):
    def __init__(self, nest: _Nest) -> None:
        super().__init__(_NEST, bytes(32), bytes(32))
        self._nest = nest

    def _dial(self, token: _BearerToken):
        if token.token not in self._nest.known:
            self._nest.refusals += 1
            raise websocket.WebSocketBadStatusException(
                "Handshake status %d %s", 401, "Unauthorized"
            )
        return object()

    def _mint_bearer(self) -> _BearerToken:
        self._nest.mints += 1
        token = f"bearer-{self._nest.mints}"
        self._nest.known.add(token)
        return _BearerToken(token=token, expires_at=time.time() + 3600)


@pytest.fixture(autouse=True)
def _no_shared_bearers():
    with admin_client_mod._BEARERS_LOCK:
        saved = dict(admin_client_mod._BEARERS)
        admin_client_mod._BEARERS.clear()
    yield
    with admin_client_mod._BEARERS_LOCK:
        admin_client_mod._BEARERS.clear()
        admin_client_mod._BEARERS.update(saved)


def test_two_clients_spend_one_refusal_and_one_mint_after_a_nest_restart():
    nest = _Nest()
    first, second = _Client(nest), _Client(nest)
    first._connect()
    second._connect()
    assert (nest.mints, nest.refusals) == (1, 0), "both share the one sign-in"

    nest.restart()
    first._connect()
    second._connect()

    assert nest.refusals == 1, (
        f"{nest.refusals} refusals for one lost sign-in: a client presented its "
        "own copy of the dead bearer after a sibling had already re-minted"
    )
    assert nest.mints == 2, (
        f"{nest.mints - 1} re-mints for one lost sign-in: a refused stale copy "
        "threw away the bearer a sibling had just minted"
    )


def test_a_refused_stale_copy_keeps_the_siblings_fresh_bearer():
    """The late client's own dead token is refused (it dialled before it could
    know); its recovery must adopt the sibling's re-mint, not discard it."""
    nest = _Nest()
    first, second = _Client(nest), _Client(nest)
    first._connect()
    second._connect()
    nest.restart()
    first._connect()
    fresh = admin_client_mod._BEARERS[first._bearer_key()].token

    # `second` dials with the bearer it held before the restart, as a client
    # that raced its sibling would.
    stale = second._bearer
    second._acquire_bearer = lambda: stale  # type: ignore[method-assign]
    second._connect()
    del second._acquire_bearer

    assert admin_client_mod._BEARERS[second._bearer_key()].token == fresh
    assert nest.mints == 2
