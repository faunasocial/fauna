"""tier_3 e2e: web ``contacts-add-button`` sends a real knock over ``fauna.inbox.send``.

Until 2026-07-09 web's ``contacts-add-button`` was a literal no-op
(``onclick={() => { /* send knock to add contact */ }}``) — the last residual of
already-completed internal tracking for the knock-send federation work.
linux/windows/android already
composed the signed ``(ContactRequest, Post)`` tuple via the shared
``fauna_client_core::email::build_knock_payload`` builder and sent it over the
``fauna.inbox.send`` kind. This pins web's leg onto that same path
(``docs/goal/architecture/api-layers.md`` § Contacts & Knocks — the authority;
``docs/goal/ui/contacts.md`` § User actions).

What makes this worth an e2e rather than a unit test: web's leg crosses a **new wasm
boundary** (the ``buildKnockPayload`` free fn + the ``inboxSend`` ``WsRpcClient``
method). A ``deno check`` proves the TS types line up; only a real nest proves the
payload actually verifies server-side and routes to a knock.

The flow this drives:

1. Register a second actor (``bob``) on the shared nest over the admin API. He is
   never logged into a client — he only needs to exist and be addressable.
2. In the browser, logged in as the ``test_user``, look Bob up by his 64-hex actor id.
   A raw actor-id resolves **offline** (``classifyRecipient`` → ``kind == 'actor_id'``
   short-circuits before any nest hop), so no handle/DNS resolution is involved.
3. Click ``contacts-add-button``.
4. Assert the SPA surfaced no error, and — the real assertion — that **Bob's**
   ``fauna.knocks.list`` now shows a pending knock from the test user.

Step 4 is what a "no error banner" check alone would miss: a silently-swallowed
promise rejection, or a payload the nest accepts but routes to a plain inbox row
instead of a knock, both leave the UI looking healthy.

Why the knock lands as a *knock* and not a delivered inbox row: ``InboxMode``'s
``#[default]`` is ``AllowKnock`` (``libs/fauna-core/src/data.rs``), and Bob is a fresh
actor with no contact relationship to the sender, so ``deliver_inbox_payload_core``
returns ``InboxDeliveryOutcome::KnockStored``. The nest replies ``inbox_id: null`` —
that null is *success*, not failure, which is exactly the kind of thing the web leg
could get backwards.

Red-verified 2026-07-09: with the button's ``onclick`` reverted to the old no-op, this
fails at the final assertion with ``knocks=[]`` (not at an earlier guard), so a green
run really does mean a knock crossed the wire.

tier_3: needs a real ``fauna-nest`` binary (``nest_instance``); web app only.

Widening attempted marker-less during the 2026-07-12 ``contacts-search-result`` /
``contact-actor-id-result`` ID consolidation (contacts.md § ID note), to use the
``actor_id_result_text()`` assertion below as live proof the surviving id resolves
correctly on every app. ``--client macos`` FAILED it at the time — not from the
id move (the diagnose output showed ``visible=True, count=1``, proving the id
resolves to the right leaf) but from a pre-existing, separate divergence: macOS/iOS
rendered this field via ``shortId(hex: result.actorId)`` (a truncated
"b7d805cf49a2…"), while web/android/windows/linux all show/hold the FULL raw
actor-id. **Fixed 2026-07-12**: both apple views now render
``result.actorId`` (full) with `.lineLimit`/`.truncationMode` for the ON-SCREEN
glyphs only — the registered automation value stays the full id, matching every
other app's actual `get_text()` semantics — so this is marker-less again as the
standing cross-app regression gate.

tier_3: web only needs a real ``fauna-nest`` binary (``nest_instance``); every
other app's variant of this send-a-real-knock check lives on its own client's
add-contact e2e (this file's own history predates apple's fix, so its name/module
stays web-flavored, but the marker no longer restricts it to web).
"""
from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from common.auth import port_base_url

pytestmark = [pytest.mark.tier_3]


def _ws_client(port: int, actor: dict) -> WsRpcAdminClient:
    """Open a User-class WS-RPC client for ``actor`` against the nest at ``port``."""
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


@pytest.mark.feature("contacts")
def test_web_add_contact_sends_knock_to_recipient(logged_in_app, nest_instance, test_user):
    """Clicking ``contacts-add-button`` lands a pending knock in the recipient's queue."""
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # Bob: a registered, addressable actor with the default AllowKnock inbox mode.
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    bob_id = bob["actor_id_hex"]

    with _ws_client(port, bob) as bob_client:
        before = bob_client.call("fauna.knocks.list", {}).get("knocks", [])
        assert not any(k["sender"] == test_user["actor_id_hex"] for k in before), (
            "precondition: Bob must not already hold a knock from the test user"
        )

    contacts = logged_in_app.contacts
    contacts.navigate()
    contacts.find_by_actor_id(bob_id)

    assert contacts.actor_id_result_text() == bob_id, (
        "the raw actor-id lookup should resolve offline and echo Bob's id back; "
        f"find-error={contacts.find_error_text()!r} "
        f"{logged_in_app.driver.diagnose('contact-actor-id-result')}"
    )

    contacts.add_contact()

    assert not logged_in_app.has_error(), (
        "sending a knock surfaced an error banner: "
        f"{logged_in_app.error_text()!r}"
    )

    # The assertion that actually proves the wire: Bob now holds a pending knock
    # from the sender. `inbox_id` came back null (KnockStored) — a delivered inbox
    # row instead of a knock would leave this list empty.
    with _ws_client(port, bob) as bob_client:
        after = bob_client.call("fauna.knocks.list", {}).get("knocks", [])

    senders = [k["sender"] for k in after]
    assert test_user["actor_id_hex"] in senders, (
        f"Bob should hold a pending knock from the test user "
        f"{test_user['actor_id_hex'][:16]}…; knocks={after}"
    )
