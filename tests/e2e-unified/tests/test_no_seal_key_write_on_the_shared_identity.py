"""A harness write of a shared identity's recipient seal key is refused at the call.

The rule is ``e2e-conventions.md`` convention 10, *A sixth form*. A seal key the
harness writes for the shared ``test_user`` or the session nest's admin is one
that account's app never holds. Whatever is sealed to it can never open there.
The app counts it on the conversations page's unopenable-mail notice for the rest
of the run, and every later "page shows no error" assertion fails on it. Two
harness writers did exactly that before this fence existed: a backup seed's
stand-in key on ``test_user``, and ``mail_bridge_mta``'s random key for the
session admin.

The fence is ``helpers/shared_identity.refuse_seal_key_write_on_shared_identity``,
called by the WS-RPC client's ``call`` and ``call_with_key`` before anything is
sent, so it covers every harness caller with no per-test discipline. These tests
drive it on a client that never connects: a refused write raises
``SharedIdentityKeyWrite``, and anything the fence lets through reaches the
client's own "no open socket" error instead.

The second half of the fence is registration: the session nest's admin is
recorded as shared where ``nest_instance`` creates it, not only when ``test_user``
is requested. Otherwise a fixture that runs first — ``mail_bridge_mta`` was one —
meets an empty registry and the fence stays silent.

tier_1: no binary, no driver, no nest.
"""

from __future__ import annotations

import ast
import pathlib

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import shared_identity

pytestmark = [pytest.mark.tier_1]

_CONFTEST = pathlib.Path(__file__).resolve().parent.parent / "conftest.py"

_SHARED = bytes(range(32))
_OWN = bytes(range(32, 64))


@pytest.fixture
def registry(monkeypatch):
    """A registry holding exactly one shared actor, for this test only — the
    run's real registry is swapped out, never cleared."""
    monkeypatch.setattr(shared_identity, "_SHARED_ACTORS", set())
    shared_identity.remember_shared_actor(_SHARED.hex())
    return shared_identity


def _offline_client() -> WsRpcAdminClient:
    return WsRpcAdminClient("http://127.0.0.1:1", actor_id=_OWN, signing_key=bytes(32))


def _seal_key_write(target: bytes) -> dict:
    return {"actor_id": target, "mls_pubkey": b"\x09" * 32}


def test_a_seal_key_write_for_a_shared_actor_is_refused_before_it_is_sent(registry):
    client = _offline_client()
    with pytest.raises(shared_identity.SharedIdentityKeyWrite, match="A sixth form"):
        client.call(shared_identity.SEAL_KEY_WRITE_KIND, _seal_key_write(_SHARED))
    with pytest.raises(shared_identity.SharedIdentityKeyWrite):
        client.call_with_key(
            shared_identity.SEAL_KEY_WRITE_KIND, _seal_key_write(_SHARED), bytes(16)
        )


def test_the_fence_lets_an_actor_of_the_tests_own_through(registry):
    """An actor a fixture minted is the harness's to key, and the call must
    reach the client's own send path, whose offline answer is "no open socket"."""
    client = _offline_client()
    with pytest.raises(RuntimeError, match="no open socket"):
        client.call(shared_identity.SEAL_KEY_WRITE_KIND, _seal_key_write(_OWN))


def test_the_fence_is_about_the_seal_key_only(registry):
    """Every other kind aimed at a shared actor is ordinary accumulation, which
    the shared identity exists for."""
    client = _offline_client()
    with pytest.raises(RuntimeError, match="no open socket"):
        client.call("fauna.bridges.provision_wrapped_mls_blob", {"actor_id": _SHARED})


def test_the_session_nest_admin_is_registered_where_the_nest_is_created():
    """``nest_instance`` itself records its admin as shared, so no fixture can
    reach the admin before the fences can see it."""
    tree = ast.parse(_CONFTEST.read_text(encoding="utf-8"))
    fixture = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "nest_instance"
    )
    calls = {
        node.func.attr
        for node in ast.walk(fixture)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
    }
    assert "remember_shared_actor" in calls, (
        "nest_instance no longer registers its admin as a shared actor; the "
        "seal-key fence is then silent for any fixture that runs before test_user"
    )
