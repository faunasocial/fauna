"""tier_3 API e2e: the **profile arm** and the **notifications arms** over a
real principal session (``docs/goal/behavior/authorization-server.md`` § Scope
grammar → *The profile arm*, *The notifications arms*).

The arm-absent refusals and the read-cannot-mark / write-cannot-list splits are
pinned in process
(``principal_handlers::tests::the_notifications_arms_read_and_mark_the_accounts_inbox``
and the profile arm's sibling); the cli journey (``test_cli_mcp.py``) polls and
marks through the catalog, whose consent always asks for every arm. What this
journey holds against a live nest, as a third party consenting all three
strings at once:

- ``fauna.profile.get`` serves the account's own public profile and another
  member's, each exactly the record the owner's own read serves — the public
  reading, any actor this nest holds;
- a like from a second member rings the account's inbox: ``list`` shows it and
  ``count`` counts it, as the owner's own reads do;
- ``mark_read`` marks it read: the principal's ``count`` and the owner's own
  ``count`` both read 0, and the row lists ``is_read``;
- ``dismiss`` and ``clear`` are refused to the principal — dismissal is the
  owner's affordance — and the row is still in the owner's inbox afterwards.
"""

import pytest

import fauna_ffi
from actions.api_actor import ApiActor
from helpers.admin_wire import admit_adult
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.principal_session import (
    PrincipalSession,
    alice_client,
    ceremony,
    refusal_code,
    upgrade,
)
from helpers.succession import publish_signed_profile

pytestmark = pytest.mark.tier_3

ARMS = "fauna:profile:read fauna:notifications:read fauna:notifications:write"
#: The loopback client's declared set (its ``client_id`` carries it).
DECLARED = f"{ARMS} openid"
REDIRECT_URI = "http://127.0.0.1:17803/callback"
NOTIF_REFUSED = "fauna.notifications.permission_denied"


def _ok(session, kind, payload):
    ok, reply = session.call(kind, payload)
    assert ok, f"{kind}: {reply!r}"
    return reply


@pytest.mark.feature("connected-apps")
def test_a_principal_reads_profiles_and_the_inbox_and_marks_it_read_never_dismisses(
    consent_nest, consent_bridge
):
    nest = consent_nest
    alice = nest["user"]
    alice_secret = bytes(alice["signing_key"])
    bob = admit_adult(nest, "bobreader", domain=HANDLE_DOMAIN, token=True)

    # ── Fixture setup over the members' own connections (convention 8 (b)). ──
    for member, name in ((alice, "Alice, in public"), (bob, "Bob, in public")):
        publish_signed_profile(
            nest["url"],
            actor_id_hex=member["actor_id_hex"],
            identity_seed_hex=bytes(member["signing_key"]).hex(),
            display_name=name,
            bio=f"{name}'s bio",
        )
    with alice_client(nest) as ws:
        own_profiles = {
            m["actor_id_hex"]: bytes(ws.call("fauna.profile.get", {"actor_id": m["actor_id_hex"]})["body"])
            for m in (alice, bob)
        }
        post_id = ws.call(
            "fauna.posts.create", {"body": fauna_ffi.build_post(alice_secret, "a post to like")}
        )["post_id"]
    ApiActor(nest["url"], bob["token"], bob["actor_id_hex"], bytes(bob["signing_key"])).like_post(
        post_id
    )
    with alice_client(nest) as ws:
        owners = ws.call("fauna.notifications.list", {})["notifications"]
        owner_unread = ws.call("fauna.notifications.count", {})["count"]
    liked = [n for n in owners if n.get("sender_id") == bob["actor_id_hex"]]
    assert len(liked) == 1 and not liked[0]["is_read"], (
        f"the control: the like rings the owner's inbox once, unread: {owners!r}"
    )
    assert owner_unread >= 1, owner_unread

    # ── The ceremony: all three strings at once. ──
    client, tokens = ceremony(nest, DECLARED, redirect_uri=REDIRECT_URI, declared=DECLARED)
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))

    # ── The profile arm: the account's own profile and another member's. ──
    for actor_hex, served_by_owner in own_profiles.items():
        reply = _ok(session, "fauna.profile.get", {"actor_id": actor_hex})
        assert bytes(reply["body"]) == served_by_owner, (
            f"the principal reads {actor_hex}'s public profile exactly as the owner's read serves it"
        )

    # ── The read arm: the inbox and its count, as the owner reads them. ──
    listed = _ok(session, "fauna.notifications.list", {})["notifications"]
    assert sorted(n["id"] for n in listed) == sorted(n["id"] for n in owners), (
        f"the principal lists the account's inbox: {listed!r} vs the owner's {owners!r}"
    )
    assert _ok(session, "fauna.notifications.count", {})["count"] == owner_unread

    # ── dismiss and clear: the owner's affordance, refused to the principal. ──
    for kind, payload in (
        ("fauna.notifications.dismiss", {"id": liked[0]["id"]}),
        ("fauna.notifications.clear", {}),
    ):
        ok, reply = session.call(kind, payload)
        assert not ok and refusal_code(reply) == NOTIF_REFUSED, f"{kind}: {reply!r}"

    # ── The write arm: mark read clears the unread state everywhere. ──
    marked = _ok(session, "fauna.notifications.mark_read", {})
    assert marked["marked_read"] == owner_unread, marked
    assert _ok(session, "fauna.notifications.count", {})["count"] == 0
    with alice_client(nest) as ws:
        assert ws.call("fauna.notifications.count", {})["count"] == 0, (
            "the mark is the account's own read state, on every device of the owner's"
        )
        after = ws.call("fauna.notifications.list", {})["notifications"]
    still = [n for n in after if n["id"] == liked[0]["id"]]
    assert len(still) == 1 and still[0]["is_read"], (
        f"the row survives the refused dismiss and clear, now read: {after!r}"
    )
