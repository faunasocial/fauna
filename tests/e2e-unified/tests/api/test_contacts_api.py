"""E2E API test: Contacts, knocks, inbox mode, and email filters.

Drives the user-facing connection-management + email-filter WS-RPC kinds
directly (no browser). The HTTP twins were deleted by the WS-RPC-everywhere
migration:

* ``GET|PUT /api/v1/inbox-mode/{aid}``        → ``fauna.inbox.mode.{get,set}``
* ``GET /api/v1/knocks/{aid}``                → ``fauna.knocks.list``
* ``GET /api/v1/contacts/{aid}``              → ``fauna.contacts.list``
* ``POST /api/v1/contacts/{aid}/confirm``     → ``fauna.contacts.confirm``
* ``GET|POST|DELETE /api/v1/email/filters*``  → ``fauna.email.filters.*``

All these kinds are ``User``-class (``bridge_method_allowlist.rs``); the
connection actor replaces the old ``{aid}`` path param, so the kinds carry no
actor argument. Reply shapes verified against
``libs/fauna-protocol/src/{contacts,email}.rs`` +
``bins/fauna-nest/src/{contacts,email}_handlers.rs`` (2026-05-24):

* ``fauna.inbox.mode.get`` → ``{"mode": str}`` (default ``"allow_knock"``)
* ``fauna.inbox.mode.set`` → ``{}`` (empty ack; bad mode → RpcCallError)
* ``fauna.knocks.list``    → ``{"knocks": [KnockItem]}``
* ``fauna.contacts.list``  → ``{"contacts": [ContactItem]}`` where
  ``ContactItem`` is ``{peer_id, status, accepted_at?, created_at}``
* ``fauna.knocks.accept``  → ``{}`` (upserts an ``accepted`` contact row)
* ``fauna.contacts.confirm`` → ``{}`` (promotes ``accepted`` → ``confirmed``)
* ``fauna.email.filters.create`` → ``{"id": i64}``
* ``fauna.email.filters.list``   → ``{"filters": [EmailFilter]}`` where
  ``EmailFilter`` is ``{id, name, rules, combination, action, priority,
  created_at}``
* ``fauna.email.filters.delete`` → ``{"ok": true}`` (unknown id → RpcCallError
  ``fauna.email.not_found``)
"""

from common import (
    build_email_inbox_payload,
    create_actor_and_register,
    port_base_url,
)

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _ws_client(port: int, actor: dict) -> WsRpcAdminClient:
    """Open a User-class WS-RPC client for ``actor`` against the nest at ``port``.

    ``actor`` is a ``common.create_actor_and_register`` dict carrying
    ``actor_id_bytes`` (32-byte Ed25519 pubkey) and ``signing_key`` (PyNaCl
    ``SigningKey``). The returned client is a context manager — use ``with``.
    """
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


@pytest.mark.feature("contacts")
@pytest.mark.feature("privacy-settings")
def test_inbox_mode_api(two_nodes):
    """Set and get inbox mode via the ``fauna.inbox.mode.*`` WS-RPC kinds.

    Verifies:
    1. Default inbox mode is retrievable (``allow_knock``)
    2. Setting to "contacts_only" persists
    3. Setting to "closed" persists
    4. Setting back to "open" persists
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    with _ws_client(port, actor) as client:
        # Default mode — a fresh actor has no inbox_modes row, so the
        # handler returns the documented default.
        reply = client.call("fauna.inbox.mode.get", {})
        assert reply.get("mode") == "allow_knock", f"Expected allow_knock default, got {reply}"
        print(f"Default inbox mode: {reply['mode']}")

        for mode in ("contacts_only", "closed", "open"):
            client.call("fauna.inbox.mode.set", {"mode": mode})
            reply = client.call("fauna.inbox.mode.get", {})
            assert reply.get("mode") == mode, f"Expected {mode}, got {reply}"
            print(f"Set to {mode}: OK")


@pytest.mark.feature("contacts")
def test_contacts_list_empty(two_nodes):
    """New user has empty knocks and contacts lists.

    Verifies:
    1. ``fauna.knocks.list`` returns ``{"knocks": []}``
    2. ``fauna.contacts.list`` returns ``{"contacts": []}``
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    with _ws_client(port, actor) as client:
        knocks = client.call("fauna.knocks.list", {})
        assert isinstance(knocks, dict), f"Expected dict reply, got {type(knocks)}"
        knock_list = knocks.get("knocks", [])
        assert isinstance(knock_list, list), f"Expected list, got {type(knock_list)}"
        assert len(knock_list) == 0, f"Expected 0 knocks, got {len(knock_list)}"
        print("Knocks list empty: OK")

        contacts = client.call("fauna.contacts.list", {})
        assert isinstance(contacts, dict), f"Expected dict reply, got {type(contacts)}"
        contact_list = contacts.get("contacts", [])
        assert isinstance(contact_list, list), f"Expected list, got {type(contact_list)}"
        assert len(contact_list) == 0, f"Expected 0 contacts, got {len(contact_list)}"
        print("Contacts list empty: OK")


@pytest.mark.feature("contacts")
def test_contacts_accept_knock(two_nodes):
    """Exercise the real contact lifecycle over the WS-RPC kinds.

    The knock-creation path (``push_knock``) is server-side only — it fires
    from inbound mail delivery (``DeliveryDecision::Knock`` in
    ``inbox_routes.rs::handle_deliver``), not from any client API. But
    ``fauna.knocks.accept`` calls ``CacheDb::accept_contact``, which upserts an
    ``accepted`` contact row *unconditionally* (no pre-existing knock required —
    verified in ``db/contacts.rs::accept_contact`` /
    ``contacts_handlers.rs::accept_contact_core``). ``fauna.contacts.confirm``
    then promotes that ``accepted`` row to ``confirmed``
    (``promote_to_confirmed`` only updates rows whose status is ``accepted``).

    So the API-reachable lifecycle is accept → confirm → list, which this test
    drives end-to-end:
    1. Alice accepts Bob → Bob appears in Alice's contacts as ``accepted``.
    2. Alice confirms Bob → Bob's status transitions to ``confirmed``.
    """
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]

    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob_id = bob["actor_id_hex"]

    with _ws_client(port, alice) as client:
        # Accept Bob — upserts an `accepted` contact row (the post-knock
        # acceptance write; works without a prior knock).
        client.call("fauna.knocks.accept", {"peer_id": bob_id})

        contacts = client.call("fauna.contacts.list", {}).get("contacts", [])
        by_peer = {c["peer_id"]: c for c in contacts}
        assert bob_id in by_peer, f"Bob {bob_id[:16]} not in contacts after accept: {contacts}"
        assert by_peer[bob_id]["status"] == "accepted", (
            f"Expected accepted, got {by_peer[bob_id]}"
        )
        print("Alice accepted Bob (status=accepted): OK")

        # Confirm Bob — promotes accepted → confirmed.
        client.call("fauna.contacts.confirm", {"peer_id": bob_id})

        contacts = client.call("fauna.contacts.list", {}).get("contacts", [])
        by_peer = {c["peer_id"]: c for c in contacts}
        assert bob_id in by_peer, f"Bob {bob_id[:16]} missing after confirm: {contacts}"
        assert by_peer[bob_id]["status"] == "confirmed", (
            f"Expected confirmed, got {by_peer[bob_id]}"
        )
        print("Alice confirmed Bob (status=confirmed): OK")


@pytest.mark.feature("contacts")
def test_delivered_message_confirms_an_accepted_contact_with_no_confirm_call(two_nodes):
    """A message that actually REACHES the inbox promotes ``accepted`` →
    ``confirmed`` on its own — nobody clicks confirm.

    ``docs/goal/ui/contacts.md`` § Where logic lives → *Contact confirm*: the
    ``fauna.contacts.confirm`` button is one route to ``confirmed``; "the same
    promotion also happens automatically on a live inbox arrival", via
    ``deliver_to_inbox``'s ``promote_contact`` flag calling the identical
    ``promote_to_confirmed``. ``test_contacts_accept_knock`` above drives the
    BUTTON route; this drives the arrival route, and it is the only test that
    reaches ``confirmed`` without calling ``fauna.contacts.confirm`` at all.

    Deliberately left on the DEFAULT ``allow_knock`` inbox mode rather than
    opening the inbox: ``allow_knock`` delivers precisely because the sender is
    already an ``accepted`` contact (``routes.rs::deliver_inbox_payload_core``),
    so the journey is the real one — you accepted their knock, they wrote to
    you, the relationship settles itself. An ``open`` inbox would deliver for a
    reason that has nothing to do with the contact edge.

    Latency-independent (convention 14): the promotion runs inside
    ``deliver_to_inbox`` *before* ``fauna.inbox.send`` replies, so the
    post-send read is ordered by the RPC itself — no wait, no poll.
    """
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    node_url = port_base_url(port)

    alice = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob_id = bob["actor_id_hex"]

    def alice_status_for_bob() -> str | None:
        with _ws_client(port, alice) as c:
            rows = c.call("fauna.contacts.list", {}).get("contacts", [])
        return next((r["status"] for r in rows if r["peer_id"] == bob_id), None)

    # Alice accepts Bob's knock. This is the whole of her deliberate action.
    with _ws_client(port, alice) as c:
        c.call("fauna.knocks.accept", {"peer_id": bob_id})
        assert c.call("fauna.inbox.mode.get", {})["mode"] == "allow_knock", (
            "the journey needs the DEFAULT inbox mode — an opened inbox would "
            "deliver for a reason unrelated to the contact edge"
        )
    assert alice_status_for_bob() == "accepted", (
        f"sanity: accept must leave the edge at `accepted`, got "
        f"{alice_status_for_bob()!r} — nothing below would mean anything otherwise"
    )

    # Bob writes to Alice. Nothing here names the contact edge.
    payload, _post_id = build_email_inbox_payload(
        bob["signing_key"],
        alice["actor_id_hex"],
        "About tomorrow",
        "A real message, not a knock.",
        node_url=node_url,
    )
    with _ws_client(port, bob) as c:
        reply = c.call(
            "fauna.inbox.send",
            {
                "recipient_actor_id": alice["actor_id_hex"],
                "recipient_nest_url": None,
                "payload_bytes": payload,
            },
        )
    assert reply.get("inbox_id") is not None, (
        "the message must actually REACH the inbox for the promotion to be due "
        f"— an accepted contact delivers under `allow_knock`; got {reply!r} "
        "(inbox_id=None means it was held as a knock instead)"
    )

    # The edge settled itself. No `fauna.contacts.confirm` was called anywhere
    # in this test.
    status = alice_status_for_bob()
    assert status == "confirmed", (
        f"a delivered message from an accepted contact must promote the edge to "
        f"`confirmed` with no confirm call (contacts.md § Where logic lives); "
        f"Alice's edge for Bob reads {status!r}"
    )


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_api(two_nodes):
    """Create, list, and delete email filters via the ``fauna.email.filters.*`` kinds.

    The ``/api/v1/email/filters*`` HTTP routes were deleted by the WS-RPC
    migration; the kinds replace them (``email_handlers.rs`` +
    ``libs/fauna-protocol/src/email.rs``).

    Wire-shape notes (serde externally-tagged enums on the CBOR wire):
    * ``EmailFilterRule::SenderIs { address }`` → ``{"SenderIs": {"address": ...}}``
    * ``EmailFilterAction::Discard`` (unit variant) → the string ``"Discard"``

    Verifies:
    1. Create returns a numeric filter ``id``
    2. List includes the filter (with the round-tripped rule + action)
    3. Delete returns ``{"ok": true}`` and the filter is gone
    4. Deleting again raises ``fauna.email.not_found``
    """
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    with _ws_client(port, actor) as client:
        create_reply = client.call(
            "fauna.email.filters.create",
            {
                "name": "Block spammer",
                "rules": [{"SenderIs": {"address": "spammer@evil.com"}}],
                "combination": "all",
                "action": "Discard",
                "priority": 0,
            },
        )
        filter_id = create_reply["id"]
        assert isinstance(filter_id, int), f"Expected numeric id, got: {create_reply}"
        print(f"Created filter: {filter_id}")

        filters = client.call("fauna.email.filters.list", {}).get("filters", [])
        by_id = {f["id"]: f for f in filters}
        assert filter_id in by_id, f"Filter {filter_id} not in list: {filters}"
        created = by_id[filter_id]
        assert created["name"] == "Block spammer", f"Name round-trip failed: {created}"
        assert created["rules"] == [{"SenderIs": {"address": "spammer@evil.com"}}], (
            f"Rule round-trip failed: {created['rules']}"
        )
        assert created["action"] == "Discard", f"Action round-trip failed: {created['action']}"
        print(f"Filter in list ({len(filters)} total)")

        delete_reply = client.call("fauna.email.filters.delete", {"id": filter_id})
        assert delete_reply.get("ok") is True, f"Delete failed: {delete_reply}"

        filters_after = client.call("fauna.email.filters.list", {}).get("filters", [])
        ids_after = [f["id"] for f in filters_after]
        assert filter_id not in ids_after, f"Filter {filter_id} still present after delete"
        print("Filter deleted and verified gone")

        # Deleting an unknown id surfaces the typed not-found error (the WS-RPC
        # analogue of the old 404).
        with pytest.raises(RpcCallError) as exc:
            client.call("fauna.email.filters.delete", {"id": filter_id})
        assert exc.value.code == "fauna.email.not_found", (
            f"Expected fauna.email.not_found, got {exc.value.code}"
        )
        print("Re-delete raised fauna.email.not_found: OK")
