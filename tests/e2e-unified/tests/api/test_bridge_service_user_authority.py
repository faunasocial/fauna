"""tier_3: what a mail helper's key is allowed to do, before approval and after
a revoke, over the real socket (``docs/goal/behavior/mail-bridge-lifecycle.md``
§ Service-user re-keying step 4 and § Don't do these).

- **admin-bridges 12** — the moment the admin rotates or revokes a helper's key
  (the app's rotate button IS ``fauna.bridges.revoke_service_user``, § Service-
  user re-keying steps 1–3), the connection that key already holds is closed
  ``4401``, the bearer it signed in with stops opening sockets, and a fresh
  sign-in with the old key gets no bridge-class answer.
- **admin-bridges 14** — a helper still waiting for approval serves no mail: the
  nest answers none of its bridge-class calls (``fetch_config`` above all, the
  snapshot every listener is armed from), and the admin's approval alone —
  nothing else changed — is what makes the same key's same calls answer.

The helper here is the harness acting with a helper's key over the same
challenge-response sign-in every caller class uses (``WsRpcAdminClient`` is a
generic actor client); the real binary's own reaction to the ``4401`` — shut
down, re-key, come back — is ``test_mail_bridge_rekey.py``. Each test runs on
``dedicated_no_mail_nest`` so a stray approved helper never shows on the shared
nest's mail health.
"""

from __future__ import annotations

import struct

import pytest
import websocket
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.bridge_enrollment import approve_bridge, enroll_and_approve_bridge, enroll_bridge

pytestmark = pytest.mark.tier_3

#: `transport.md` § Revocation teardown — the close a revoked actor's socket gets.
CLOSE_AUTH_REVOKED = 4401


def _as_key(nest, sk: SigningKey) -> WsRpcAdminClient:
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _admin(nest) -> WsRpcAdminClient:
    return _as_key(nest, nest["admin"]["signing_key"])


def _close_code(client: WsRpcAdminClient) -> int:
    """Read the held socket until the nest closes it; its close code."""
    raw = client._ws
    assert raw is not None, "the helper's connection was never open"
    while True:
        opcode, data = raw.recv_data(control_frame=True)
        if opcode == websocket.ABNF.OPCODE_CLOSE:
            return struct.unpack("!H", data[:2])[0] if len(data) >= 2 else 1005


def _refused(nest, sk: SigningKey, kind: str, payload: dict) -> str:
    """Sign in with ``sk`` and call ``kind``; the nest must refuse — at the
    sign-in's RPCs, at the socket upgrade, or at the call, whichever it reaches
    first. Returns where and how it refused."""
    with pytest.raises((RpcCallError, websocket.WebSocketBadStatusException)) as ei:
        with _as_key(nest, sk) as ws:
            ws.call(kind, payload)
    refusal = ei.value
    if isinstance(refusal, RpcCallError):
        assert refusal.code, refusal
        return f"rpc {refusal.code}"
    assert refusal.status_code in (401, 403), refusal
    return f"upgrade {refusal.status_code}"


@pytest.mark.feature("admin-bridges")
def test_revoking_a_helper_closes_its_connection_and_refuses_its_key(dedicated_no_mail_nest):
    """admin-bridges 12: an approved MTA key holds a live connection whose
    ``whoami`` answers; the admin revokes it; that connection is closed ``4401``,
    the bearer it held is refused at the upgrade (``401``), and a fresh sign-in
    with the old key gets no ``whoami`` answer."""
    nest = dedicated_no_mail_nest
    sk = SigningKey.generate()
    pk = bytes(sk.verify_key)
    enroll_and_approve_bridge(nest["url"], nest["admin"]["signing_key"], pk, "mta")

    held = _as_key(nest, sk)
    with held:
        me = held.call("fauna.bridges.whoami", {})
        assert me["role"] == "mta" and me["status"] == "approved", me
        old_bearer = held._bearer.token

        with _admin(nest) as admin:
            reply = admin.call("fauna.bridges.revoke_service_user", {"bridge_actor_id": pk})
        assert reply.get("ok") is True, reply

        assert _close_code(held) == CLOSE_AUTH_REVOKED, (
            "the revoke must close the connection the helper's key already held"
        )

    with pytest.raises(websocket.WebSocketBadStatusException) as dial:
        websocket.create_connection(
            held._build_ws_url(),
            subprotocols=["fauna.v1", f"bearer.{old_bearer}"],
            timeout=10.0,
            sslopt=held._ssl_opt(),
        ).close()
    assert dial.value.status_code == 401, (
        f"the revoked key's bearer must be refused at the upgrade, got {dial.value.status_code}"
    )

    # A fresh sign-in with the revoked key gets no whoami answer (`_refused`
    # raises if it does).
    _refused(nest, sk, "fauna.bridges.whoami", {})


@pytest.mark.feature("admin-bridges")
def test_a_pending_helper_is_given_nothing_to_serve_until_approved(dedicated_no_mail_nest):
    """admin-bridges 14: a freshly enrolled MTA key waits pending (mail never
    on); signed in with that key, ``fetch_config`` — the snapshot its listeners
    are armed from — and ``whoami`` are both refused. The admin approves the row
    and changes nothing else; the same key's same calls now answer, so the
    refusals were the pending state's doing."""
    nest = dedicated_no_mail_nest
    sk = SigningKey.generate()
    pk = bytes(sk.verify_key)
    assert enroll_bridge(nest["url"], pk, "mta") == "pending", "mail never on → the MTA waits"

    for kind, payload in (
        ("fauna.bridges.fetch_config", {"scope": "all"}),
        ("fauna.bridges.whoami", {}),
    ):
        _refused(nest, sk, kind, payload)

    approve_bridge(nest["url"], nest["admin"]["signing_key"], pk, "mta")

    with _as_key(nest, sk) as ws:
        me = ws.call("fauna.bridges.whoami", {})
        assert me["status"] == "approved", me
        config = ws.call("fauna.bridges.fetch_config", {"scope": "all"})
    assert isinstance(config, dict) and config, (
        f"the approved helper's fetch_config must answer with a snapshot, got {config!r}"
    )
