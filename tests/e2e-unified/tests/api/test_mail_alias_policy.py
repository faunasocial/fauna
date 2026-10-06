"""tier_3 e2e for the alias-policy admin write path
(``fauna.bridges.put_alias_policy``).

Unlike the five ``put_<substruct>_policy`` kinds (A3 Bucket B), these four
knobs (``exact_aliases_max``, ``reserved_local_parts``,
``subaddressing_enabled``, ``wildcard_prefix_enabled``) are **not**
projected to the bridge via ``fetch_config`` — they are read nest-side by
the alias resolver + alias CRUD. So this file proves both the Admin write
wire path *and* a functional read-back: after the admin sets
``exact_aliases_max=1`` over the socket, a User's second ``create_account_
alias`` is rejected by the now-tunable cap (default would be 20). The
resolver gates (``subaddressing_enabled`` / ``wildcard_prefix_enabled``)
are read by the MTA-class ``resolve_recipient`` (covered by the in-process
Rust handler test ``subaddressing_disabled_via_policy_falls_through``).
"""

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

DOMAIN = "alias-policy-e2e.test"


def _admin_client(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _create_exact(client, pattern):
    return client.call(
        "fauna.bridges.create_account_alias",
        {
            "kind": "exact",
            "local_domain": DOMAIN,
            "pattern": pattern,
            "controls": {"label": ""},
        },
    )


def test_admin_puts_alias_policy_over_wire(nest_instance):
    """The Admin write path round-trips ``{ ok: true }`` over the real socket."""
    client = _admin_client(nest_instance)
    with client:
        reply = client.call(
            "fauna.bridges.put_alias_policy",
            {
                "exact_aliases_max": 50,
                "reserved_local_parts": ["postmaster", "abuse", "sales"],
                "subaddressing_enabled": False,
                "wildcard_prefix_enabled": True,
            },
        )
    assert reply.get("ok") is True, f"put_alias_policy must return ok=true: {reply!r}"


def test_non_admin_denied_on_put_alias_policy(nest_instance):
    """A User-class actor is rejected by the allowlist before any DB work."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.put_alias_policy", {"exact_aliases_max": 5})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User-class actor on an Admin kind must be denied; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-mail-policy", "mail-aliases")
def test_exact_alias_cap_enforced_from_put_alias_policy(nest_instance):
    """Admin lowers ``exact_aliases_max`` to 1 → a User's second exact-alias
    create is rejected by the now-tunable cap (the read-back the handler
    does on every create). Resets the deployment-wide policy afterward so
    the session-scoped nest's other alias tests see the default cap again.
    """
    admin = _admin_client(nest_instance)
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with admin:
        admin.call("fauna.bridges.put_alias_policy", {"exact_aliases_max": 1})
        try:
            with _user_client(nest_instance, user) as uclient:
                # First exact alias under the cap of 1 succeeds…
                first = _create_exact(uclient, "alice")
                assert len(first["alias_id"]) == 16
                # …the second hits the now-tunable cap.
                with pytest.raises(RpcCallError) as excinfo:
                    _create_exact(uclient, "alice.smith")
            assert excinfo.value.code == "fauna.bridges.alias_cap_exceeded", (
                f"second create over cap=1 must be rejected; got {excinfo.value.code!r}"
            )
        finally:
            # Restore the catalog default (all-None ⇒ const defaults).
            admin.call("fauna.bridges.put_alias_policy", {})
