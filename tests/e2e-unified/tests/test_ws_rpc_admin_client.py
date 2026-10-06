"""Tests for ``clients.ws_rpc_admin_client.WsRpcAdminClient``.

The admin client drives the existing ``provision_*`` (and follow-on admin)
WS-RPC handlers on nest — the design path that replaces the original
"HTTP admin twin routes" idea. The
client is the canonical Python entry-point that future e2e tests use to
exercise any Admin-class kind (TLS / submission tokens,
``put_<substruct>_policy`` / ``provision_recipient_mls_pubkey``).

Three tiers of assertion in this file:

* Auth + dispatch reachability for the Admin caller class — a malformed
  payload to ``provision_tls_cert_blob`` round-trips an ``ok=false`` with
  ``fauna.protocol.malformed``. That proves: HTTP challenge/verify
  succeeded, WebSocket subprotocol negotiation succeeded, the dispatcher
  routed by ``kind`` and the handler ran (without it we'd see
  ``fauna.bridges.permission_denied`` or HTTP 401).
* Caller-class enforcement — a non-Admin actor calling the same kind
  gets ``fauna.bridges.permission_denied``. Confirms the bearer-token →
  ``caller_class_for_actor`` flow is wired.
* True round-trip with ``ok=true`` against ``fauna.protocol.echo`` (open
  to any caller class). Confirms the Reply payload decode path works
  end-to-end.

The full "seal + provision + bridge reads back" round-trip is the
seal-helper's job (E.3.2) — this file only owns the client itself.
"""

import secrets

import cbor2
import pytest

pytestmark = [pytest.mark.tier_3]


def test_admin_can_open_ws_rpc_session_and_echo(nest_instance):
    """Smoke test — full round-trip with ok=true via fauna.protocol.echo.

    Drives every layer of the client: HTTP challenge/verify → bearer →
    WebSocket subprotocol handshake → canonical DAG-CBOR Request encode →
    correlation-id-matched Reply decode → reply payload returned.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        nonce = secrets.token_bytes(8)
        reply = client.call("fauna.protocol.echo", {"data": nonce})
    assert reply["data"] == nonce, f"echo did not round-trip: {reply!r}"


def test_admin_provision_tls_cert_blob_round_trips_malformed_error(nest_instance):
    """Auth + dispatch reachability for the Admin caller class.

    A malformed TLS-cert blob (random bytes that fail the canonical-CBOR
    header parse) reaches ``provision_tls_cert_blob_handler``, which
    returns an ``RpcError`` with code ``fauna.protocol.malformed``. The
    Reply round-trip carries ``ok=false`` and the decoded error code.

    The strict assertion here is: the response is *not*
    ``fauna.bridges.permission_denied`` and *not* an HTTP-layer failure —
    that confirms challenge/verify, bearer-token bind, WS subprotocol
    negotiation, and caller-class resolution to Admin all succeeded for
    the nest's admin actor. Sibling seal-helper (E.3.2) work supplies
    well-formed blobs for the ``ok=true`` round-trip later.
    """
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    # 64 random bytes are not a valid TlsCertBlob canonical-CBOR header.
    malformed_blob = secrets.token_bytes(64)
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.provision_tls_cert_blob(malformed_blob)
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"expected fauna.protocol.malformed (Admin reached the handler), "
        f"got {excinfo.value.code!r} (details={excinfo.value.details!r}). "
        f"permission_denied here would mean caller-class resolution "
        f"failed; an HTTP error would mean the challenge/verify or WS "
        f"handshake failed."
    )


@pytest.mark.feature("admin-dashboard")
def test_non_admin_actor_is_permission_denied_on_admin_kind(nest_instance, test_user):
    """Caller-class enforcement: non-Admin → fauna.bridges.permission_denied.

    Confirms the bearer's actor flows into ``caller_class_for_actor`` and
    the admin-only kind is gated. ``test_user`` is a regular User-class
    actor (registered via the admin API but not promoted via
    ``add_admin``), so the handler's ``require_class(.., Admin)`` short-
    circuits before payload decode.
    """
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    # The payload doesn't matter — the class check fires first.
    payload = cbor2.dumps(secrets.token_bytes(64))  # any bytes
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.provision_tls_cert_blob(payload)
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"expected fauna.bridges.permission_denied for a User-class "
        f"actor on an Admin kind, got {excinfo.value.code!r}"
    )


def test_generic_call_surface_routes_unknown_kind(nest_instance):
    """The ``.call(kind, payload)`` generic surface is the extension hook
    follow-on admin kinds (``put_spam_policy``,
    ``provision_recipient_mls_pubkey``,
    ``ingest_inbound_mail``) consume. Verify it surfaces dispatcher
    errors symmetrically — an unknown kind yields
    ``fauna.protocol.unknown_kind``.
    """
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.test.not_a_real_kind", {"foo": "bar"})
    assert excinfo.value.code == "fauna.protocol.unknown_kind", (
        f"unknown kinds should round-trip as fauna.protocol.unknown_kind; "
        f"got {excinfo.value.code!r}"
    )
