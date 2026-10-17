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
resolver outcome itself is MTA-class (``resolve_recipient``, bridge→nest; the Go MTA consumes
it on both the inbound and submission RCPT-TO paths, ``mail-forwarding.md``
§ Implementation status today) and is covered by the in-process Rust handler test
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


@pytest.mark.feature("admin-forwarders")
def test_create_forwarder_rejects_an_address_already_held(nest_instance):
    """A forwarder on an address a member's alias or another forwarder already
    holds is refused with ``conflicts_with_existing_alias``
    (``mail-aliases.md`` § Kind 7 — "must not collide with an existing exact
    alias or forwarder"). The control is a fresh pattern on the same domain,
    which the same admin creates — so the refusal is the collision, not the
    domain or the caller."""
    admin = _admin_client(nest_instance)
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    member_held = "held" + secrets.token_hex(3)
    fwd_held = "fwd" + secrets.token_hex(3)
    with admin:
        _ensure_domain(admin)
        admin.call(
            "fauna.bridges.create_forwarder",
            {"local_domain": DOMAIN, "pattern": fwd_held, "forward_target": "a@external.example"},
        )
    with _user_client(nest_instance, user) as member:
        member.call(
            "fauna.bridges.create_account_alias",
            {
                "kind": "exact",
                "local_domain": DOMAIN,
                "pattern": member_held,
                "controls": {"label": ""},
            },
        )
    with admin:
        for pattern, holder in ((member_held, "a member's alias"), (fwd_held, "a forwarder")):
            with pytest.raises(RpcCallError) as excinfo:
                admin.call(
                    "fauna.bridges.create_forwarder",
                    {
                        "local_domain": DOMAIN,
                        "pattern": pattern,
                        "forward_target": "b@external.example",
                    },
                )
            assert excinfo.value.code == "fauna.bridges.conflicts_with_existing_alias", (
                f"an address {holder} holds must be refused; got {excinfo.value.code!r}"
            )
        control = admin.call(
            "fauna.bridges.create_forwarder",
            {
                "local_domain": DOMAIN,
                "pattern": "free" + secrets.token_hex(3),
                "forward_target": "b@external.example",
            },
        )
        assert len(control["alias_id"]) == 16, "control: a free address is accepted"


@pytest.mark.feature("admin-forwarders")
def test_forwarders_never_list_among_the_admins_own_addresses(nest_instance):
    """The admin's forwarders stay out of the admin's owner-scoped
    ``list_account_aliases`` (``mail-aliases.md`` § Kind 7, ``admin.md`` § 4):
    a forwarder the admin creates shows in ``list_forwarders`` and never in the
    personal list, while a personal alias the same admin creates on the same
    domain does show there — the control that the personal read is live."""
    admin = _admin_client(nest_instance)
    fwd = "team" + secrets.token_hex(3)
    personal = "me" + secrets.token_hex(3)
    with admin:
        _ensure_domain(admin)
        created = admin.call(
            "fauna.bridges.create_forwarder",
            {"local_domain": DOMAIN, "pattern": fwd, "forward_target": "x@external.example"},
        )
        admin.call(
            "fauna.bridges.create_account_alias",
            {"kind": "exact", "local_domain": DOMAIN, "pattern": personal, "controls": {"label": ""}},
        )
        forwarders = admin.call("fauna.bridges.list_forwarders", {})["forwarders"]
        own = admin.call("fauna.bridges.list_account_aliases", {})["aliases"]
    assert any(r["alias_id"] == created["alias_id"] for r in forwarders)
    own_patterns = {r.get("pattern") for r in own}
    assert personal in own_patterns, f"control: the admin's personal alias must list: {own!r}"
    assert fwd not in own_patterns, f"a forwarder must never list as a personal address: {own!r}"
    assert all(r.get("kind") != "forwarder" for r in own), own
