"""tier_3 e2e for the admin external-forwarder write path
(``fauna.bridges.{create,list,delete}_forwarder``
§ AF; ``docs/goal/behavior/mail-aliases.md`` § Kind 7).

An admin external forwarder is an ``account_aliases`` ``kind='forwarder'`` row:
an address with no local mailbox whose inbound is forwarded to an external
``forward_target``, attributed to the managing admin actor. These three RPCs
are **Admin-class** (distinct from the User ``*_account_alias`` CRUD) and the
forwarder is excluded from the owner-scoped ``list_account_aliases``.

This file proves the Admin CRUD + validation wire path over the real socket:
create → list (carries ``forward_target``) → delete; plus the create-time
guards (non-admin denied, target-on-a-hosted-domain refused, unhosted
``local_domain`` refused, reserved local-part refused). The ``Forward``
resolver outcome itself is MTA-class (``resolve_recipient``, bridge→nest, not
yet wired to the Go MTA — the ``validate_recipient``→``resolve_recipient``
RCPT-TO cutover is pending) and is covered by the in-process Rust handler test
``create_forwarder_resolves_to_forward_then_deletes``, mirroring how
``test_mail_alias_policy.py`` defers the resolver gates to the Rust tests.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

DOMAIN = "forwarder-e2e.test"


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


def _ensure_domain(admin):
    """Idempotently add the test domain (re-add returns ``skipped=true``)."""
    admin.call(
        "fauna.bridges.add_local_domain",
        {
            "domain": DOMAIN,
            "mta_sts_cert_mode": "expand_primary",
        },
    )


@pytest.mark.feature("admin-forwarders")
def test_admin_creates_lists_deletes_forwarder(nest_instance):
    """create → list (with ``forward_target``) → delete, over the real socket.

    Pattern is randomized so the session-scoped nest's other forwarder rows
    can't perturb the assertions; we filter ``list_forwarders`` to our row.
    """
    admin = _admin_client(nest_instance)
    pattern = "info" + secrets.token_hex(3)
    target = f"{pattern}@external.example"
    with admin:
        _ensure_domain(admin)
        created = admin.call(
            "fauna.bridges.create_forwarder",
            {"local_domain": DOMAIN, "pattern": pattern, "forward_target": target},
        )
        alias_id = created["alias_id"]
        assert len(alias_id) == 16, f"expected a 16-byte alias id: {created!r}"

        listed = admin.call("fauna.bridges.list_forwarders", {})["forwarders"]
        mine = [r for r in listed if r["alias_id"] == alias_id]
        assert len(mine) == 1, f"created forwarder must be listed: {listed!r}"
        row = mine[0]
        assert row["kind"] == "forwarder"
        assert row["pattern"] == pattern
        assert row["forward_target"] == target

        deleted = admin.call("fauna.bridges.delete_forwarder", {"alias_id": alias_id})
        assert deleted.get("ok") is True

        after = admin.call("fauna.bridges.list_forwarders", {})["forwarders"]
        assert all(r["alias_id"] != alias_id for r in after), "delete must remove it"


@pytest.mark.feature("admin-forwarders")
def test_non_admin_denied_on_create_forwarder(nest_instance):
    """A User-class actor is rejected by the allowlist before any DB work."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call(
                "fauna.bridges.create_forwarder",
                {
                    "local_domain": DOMAIN,
                    "pattern": "sales" + secrets.token_hex(3),
                    "forward_target": "team@external.example",
                },
            )
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User-class actor on an Admin kind must be denied; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-forwarders")
def test_create_forwarder_rejects_local_domain_target(nest_instance):
    """A target on a domain we host is refused — that's an alias, not a forward
    (``mail-forwarding.md:244``)."""
    admin = _admin_client(nest_instance)
    with admin:
        _ensure_domain(admin)
        with pytest.raises(RpcCallError) as excinfo:
            admin.call(
                "fauna.bridges.create_forwarder",
                {
                    "local_domain": DOMAIN,
                    "pattern": "info" + secrets.token_hex(3),
                    "forward_target": f"bob@{DOMAIN}",
                },
            )
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"local-domain target must be refused; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-forwarders")
def test_create_forwarder_rejects_unhosted_domain(nest_instance):
    """A forwarder on a domain the deployment does not host is refused."""
    admin = _admin_client(nest_instance)
    with admin:
        with pytest.raises(RpcCallError) as excinfo:
            admin.call(
                "fauna.bridges.create_forwarder",
                {
                    "local_domain": "not-hosted-" + secrets.token_hex(3) + ".test",
                    "pattern": "info",
                    "forward_target": "team@external.example",
                },
            )
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"unhosted local_domain must be refused; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-forwarders")
def test_create_forwarder_rejects_reserved_local_part(nest_instance):
    """A reserved role local-part (``postmaster``) can't be a forwarder either —
    it routes to the admin mailbox via role-address routing."""
    admin = _admin_client(nest_instance)
    with admin:
        _ensure_domain(admin)
        with pytest.raises(RpcCallError) as excinfo:
            admin.call(
                "fauna.bridges.create_forwarder",
                {
                    "local_domain": DOMAIN,
                    "pattern": "postmaster",
                    "forward_target": "team@external.example",
                },
            )
    assert excinfo.value.code == "fauna.bridges.reserved_local_part", (
        f"reserved local-part must be refused; got {excinfo.value.code!r}"
    )
