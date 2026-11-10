"""tier_3 API e2e: the **admin arms** over a real principal session
(``docs/goal/behavior/authorization-server.md`` § Scope grammar — three
families → *The admin arms*; the sovereign-act boundary,
``docs/goal/architecture/apps/cli.md`` § The sovereign-act boundary).

``docs/features/connected-apps.md`` outcome 53. The per-kind matrix and the
per-RPC authority read are pinned in process
(``principal_handlers::tests::the_users_page_reads_under_either_verb_and_writes_under_rw``,
``the_admin_plane_is_served_only_while_the_account_is_an_admin``, and
``bridge_method_allowlist::tests::the_users_pages_sovereign_acts_reach_no_principal``);
what this journey holds against a live nest is the outside-observed twin. The
nest's admin approves an outside app for ``fauna:admin:read:dashboard`` and
``fauna:admin:rw:users`` over the admin's own connection, the way the admin's
app answers the card, and then, on the app's principal session:

- it reads the dashboard (``fauna.admin.stats``, ``fauna.admin.status``) and
  the users page (``fauna.admin.users.list``), as the admin's own reads serve
  them;
- it acts on the users page — creates an invite code — and the code is in the
  admin's own list afterwards;
- the acts only a sovereign admin may take never reach it: deleting a member
  and seating an admin are refused, and the member is still there with no
  pending deletion scheduled against them.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.oauth_client import OAuthClient
from helpers.principal_session import PrincipalSession, redeem, refusal_code, upgrade

pytestmark = pytest.mark.tier_3

ARMS = "fauna:admin:read:dashboard fauna:admin:rw:users"
#: The loopback client's declared set (its ``client_id`` carries it).
DECLARED = f"{ARMS} openid"
REDIRECT_URI = "http://127.0.0.1:17812/callback"


def _admin_client(nest):
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _admin_ceremony(nest):
    """One browser-start ceremony, approved over the ADMIN's own connection —
    the admin's app answering the card. Returns the client and its tokens."""
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=DECLARED,
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with _admin_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        matches = [c for c in pending if c["code"] == browser_code]
        assert matches, f"no pending consent carries {browser_code!r}: {pending!r}"
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": matches[0]["consent_id"], "approved": True},
        )
        assert resolved["resolved"], f"the admin's approval resolves the card: {resolved!r}"
    return client, redeem(client, flow_token)


def _ok(session, kind, payload):
    ok, reply = session.call(kind, payload)
    assert ok, f"{kind}: {reply!r}"
    return reply


@pytest.mark.feature("connected-apps")
def test_an_admin_approved_app_reads_and_acts_on_the_admin_pages_never_a_sovereign_act(
    consent_nest, consent_bridge
):
    nest = consent_nest
    member = nest["user"]
    member_id = member["actor_id_bytes"]

    client, tokens = _admin_ceremony(nest)
    for arm in ARMS.split():
        assert arm in tokens.get("scope", "").split(), (
            f"the grant carries the admin string the admin approved: {tokens!r}"
        )
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))

    # ── Read: the dashboard and the users page. ──
    stats = _ok(session, "fauna.admin.stats", {})
    _ok(session, "fauna.admin.status", {})
    with _admin_client(nest) as ws:
        own_stats = ws.call("fauna.admin.stats", {})
    # The member count, not the whole reply: the connection count is live and
    # differs between two reads made over two different sockets.
    assert stats["total_users"] == own_stats["total_users"] >= 2, (
        f"the app reads the dashboard the admin's own read serves: {stats!r} vs {own_stats!r}"
    )
    listed = _ok(session, "fauna.admin.users.list", {"limit": 50, "offset": 0})
    assert any(bytes(u["actor_id"]) == member_id for u in listed["users"]), (
        f"the app reads the member list: {listed!r}"
    )

    # ── Act: a non-sovereign write of the users page. ──
    created = _ok(session, "fauna.admin.invite_codes.create", {"tier": "free", "uses": 1})
    code = created.get("code")
    assert code, f"the app creates an invite code: {created!r}"
    with _admin_client(nest) as ws:
        codes = ws.call("fauna.admin.invite_codes.list", {})
    assert code in repr(codes), f"the app's invite code is in the admin's own list: {codes!r}"

    # ── Sovereign: deleting a member and seating an admin never reach it. ──
    for kind, payload in (
        ("fauna.admin.users.delete", {"actor_id": member_id}),
        ("fauna.admin.admins.add", {"actor_id": member_id}),
    ):
        ok, reply = session.call(kind, payload)
        assert not ok and refusal_code(reply).endswith("permission_denied"), (
            f"{kind} is a sovereign act and must not reach the app: {reply!r}"
        )
    with _admin_client(nest) as ws:
        after = ws.call("fauna.admin.users.list", {"limit": 50, "offset": 0})
        admins = ws.call("fauna.admin.admins.list", {})
        pending = ws.call("fauna.pending_actions.list", {})
    assert any(bytes(u["actor_id"]) == member_id for u in after["users"]), (
        f"the member is still there: {after!r}"
    )
    assert member_id not in {bytes(a["actor_id"]) for a in admins["admins"]}, (
        f"the member was not seated as an admin: {admins!r}"
    )
    against = [
        a for a in pending.get("actions", [])
        if a.get("action_type") in ("admin.delete_user", "admin.add")
    ]
    assert not against, f"nothing sovereign was scheduled through the app: {against!r}"
    session.ws.close()
