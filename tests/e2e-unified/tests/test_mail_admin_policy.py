"""tier_3 e2e for the mail-policy admin write path
(``fauna.bridges.put_{spam,auth,submission,imap,outbound}_policy`` — A3
Bucket B, tracked internally).

The mission these kinds serve: the mail bridge is configurable
entirely from a Fauna app, over WS-RPC, with no HTTP. This file proves
the full wire path for an Admin caller across all five per-sub-struct
policy kinds — HTTP challenge/verify → bearer → WS subprotocol negotiation
→ canonical DAG-CBOR Request encode → kind-routed dispatch → handler →
``mail_<substruct>_policy`` table → Reply decode — plus the spam-threshold
ordering guard and the caller-class gate.

The ``put → fetch_config`` overlay *projection* (each ``Some`` field
reaching the bridge) is covered in isolation by the Rust conformance test
``bins/fauna-nest/tests/conformance_mail_policy.rs`` (real router dispatch,
which also exercises the overlay) — observing it over the socket would
require a bridge service-user to call the MTA/MDA-class ``fetch_config``,
disproportionate here. This file owns the over-the-wire integration of the
Admin write kinds; same scoping rationale as ``test_mail_admin_local_domains``.
"""

import pytest

from conftest import _start_dedicated_nest

pytestmark = [pytest.mark.tier_3]


@pytest.fixture(scope="module")
def policy_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest for the puts that LAND. Each ``put_*_policy`` replaces
    its whole sub-struct row (``db/mail_policy.rs`` ``write_singleton_json``),
    and there is no admin read-back to restore it from. On the shared
    ``nest_instance`` these puts wiped the posture ``mail_bridge_mta`` set for
    every later mail test: ``max_conn_per_min`` fell back to the catalog 10/min,
    so the suite's loopback SMTP drew ``421 Connection rate limit exceeded``, and
    ``delete_nonempty: allowed`` let a non-empty IMAP folder be deleted. The
    refused puts below write nothing and stay on the shared nest."""
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory,
                                          "mail-admin-policy")
    yield nest
    cleanup()


def _admin_client(nest_instance):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


# One representative payload per sub-struct kind. Every field is optional
# (``None`` ⇒ catalog default); the values here are arbitrary valid
# overrides chosen to round-trip over the wire.
_PUT_CASES = [
    (
        "fauna.bridges.put_spam_policy",
        {
            "baseline_standing_publish": False,
            "dnsbl_servers": [],
            "greylist_enabled": False,
            "max_score_before_reject": 20,
            "max_message_bytes": 100_000_000,
            "fcrdns_mode": "off",
        },
    ),
    (
        "fauna.bridges.put_auth_policy",
        {"enforce_dkim": True, "log_only": True, "max_auth_failures_per_minute": 15},
    ),
    (
        "fauna.bridges.put_submission_policy",
        {"max_per_day": 500, "max_recipients_per_message": 50},
    ),
    (
        "fauna.bridges.put_imap_policy",
        {"delete_nonempty": "allowed", "storage_bytes_default": 2 << 30},
    ),
    (
        "fauna.bridges.put_outbound_policy",
        {"retry_schedule_seconds": [0, 600, 3600], "ipv6_enabled": False},
    ),
]


@pytest.mark.parametrize("kind,payload", _PUT_CASES, ids=[k for k, _ in _PUT_CASES])
@pytest.mark.feature("admin-mail-policy")
def test_admin_puts_policy_substruct_over_wire(policy_nest, kind, payload):
    """Each per-sub-struct policy kind round-trips for an Admin caller.

    Proves the kind is registered, the allowlist permits Admin, and the
    DAG-CBOR Request/``PutPolicyReply`` round-trips over the real socket.
    """
    client = _admin_client(policy_nest)
    with client:
        reply = client.call(kind, payload)
    assert reply.get("ok") is True, f"{kind} must return ok=true: {reply!r}"


@pytest.mark.feature("admin-mail-policy")
def test_put_spam_policy_rejects_threshold_ordering_violation_over_wire(nest_instance):
    """An override whose effective spam thresholds violate
    ``spam_folder < reject`` is rejected at the handler with
    ``fauna.protocol.malformed`` (the bridge would otherwise debug-assert).
    """
    from clients.ws_rpc_admin_client import RpcCallError

    client = _admin_client(nest_instance)
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            # Effective {folder=5 (the default), reject=3} violates the
            # ordering of the two tiers: folder=5 must be < reject=3 (a
            # disabled 0 tier would be skipped).
            client.call(
                "fauna.bridges.put_spam_policy", {"baseline_standing_publish": False, "max_score_before_reject": 3}
            )
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"out-of-order spam thresholds must be rejected; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_put_policy_kind(nest_instance, test_user):
    """A User-class actor is rejected by the allowlist before any DB work."""
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.put_spam_policy", {"baseline_standing_publish": False, "greylist_enabled": False})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User-class actor on an Admin kind must be denied; got {excinfo.value.code!r}"
    )
