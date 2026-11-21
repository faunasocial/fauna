"""The user's main use cases no other test walked, walked by gestures alone —
`add-a-contact` and `message-someone` (`docs/goal/behavior/main-use-cases.md`
§ The journeys; the steps are catalogued in `use-cases.yaml`).

Each journey starts where a user is after signing in: on the feed. It reaches
the page by its tab, acts with the page's own controls, and reads the outcome
where a person would see it. Then `assert_use_case_walked` checks that the
driver's gesture record walked the catalogued steps (convention 8: the mutation
under test is a UI gesture; an API call only arranges the world).

* `add-a-contact` — the world holds a second account on the same nest, with a
  handle. The user types `handle@<the nest's domain>` into the lookup, looks it
  up, and presses Add on the result. Adding sends a knock, so the outcome is
  what the sender sees, the Add button reading Sent, and what the knock did,
  the person's knock queue holding it (read over the API, an observation, not a
  gesture). The sender's own roster gains no row until the person accepts: the
  pending edge is the recipient's (`contacts.md` § Persistence, the mutual-edge
  ruling: the requester learns of the accept from an accepted row).
* `message-someone` — the world holds bob, a second real-engine seat on the
  same nest who has accepted alice (so her Welcome is delivered, not knocked).
  alice opens a new conversation, types bob's handle and her message, and
  presses Send without committing the recipient chip first: Send flushes the
  typed recipient (`ConversationsManager::send_new_thread`), which is what a
  person does and what the catalogued steps say. The outcome is the message on
  both seats — alice's bubble in the thread, bob's thread carrying it once his
  receive loop has looked (bob's push arm is suppressed and his ticker muted, so
  the look is one explicit poke, convention 14's causal anchor).

tier_3: a real nest and real apps, nothing stubbed. tui is the lead app; the
other apps run it through the background tester.
"""

from __future__ import annotations

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, port_base_url
from conftest import MAIL_PRIMARY_DOMAIN
from helpers import budgets
from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.contacts_knocks import read_racy
from helpers.nav_tab_click import click_nav_tab
from helpers.use_case_walk import assert_use_case_walked, setup_gestures
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


@pytest.mark.use_case("add-a-contact")
@pytest.mark.feature("contacts")
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_a_user_adds_a_person_on_the_same_nest_by_handle(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    d = app.driver
    target = create_actor_and_register(
        nest_instance["port"], base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    assert target["handle"], "control: the person to add needs a handle"
    address = f"{target['handle']}@{MAIL_PRIMARY_DOMAIN}"

    # Where a signed-in user starts: the feed (a state write, before step 1).
    app.feed.navigate()

    click_nav_tab(d, "contacts-tab", canary="contact-actor-id-field")
    # The People/Address Book segment is sticky across navigation; an earlier
    # test's Address Book visit is this test's world, not a step of the case.
    if d.is_visible("contacts-segment-people"):
        with setup_gestures(d):
            app.contacts.switch_to_people()
            d.wait_for("contact-actor-id-field", timeout=budgets.UI_SETTLE_S)

    app.contacts.find_by_handle(address)
    found = wait_until(
        lambda: app.contacts.actor_id_result_text() or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"looking up {address}, an account on this nest, found nobody: "
            f"find-error={app.contacts.find_error_text()!r} "
            f"{d.diagnose('contact-actor-id-result')}; error={app.error_text()!r}"
        ),
    )
    assert target["actor_id_hex"] in found, (
        f"{address} must resolve to the account that holds the handle, "
        f"{target['actor_id_hex']}; the result reads {found!r}"
    )

    app.contacts.add_contact_no_settle()

    # The outcome where the user sees it: the Add button reads Sent.
    sent = S.contacts.sent
    wait_until(
        lambda: read_racy(lambda: d.get_text("contacts-add-button")) == sent,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"after adding {address}, contacts-add-button should read {sent!r}; "
            f"got {read_racy(lambda: d.get_text('contacts-add-button'))!r}; "
            f"error={app.error_text()!r}"
        ),
    )
    assert not app.has_error(), (
        f"adding {address} surfaced an error banner: {app.error_text()!r}"
    )
    # What the knock did: it waits in the person's own queue.
    with WsRpcAdminClient(
        port_base_url(nest_instance["port"]),
        actor_id=target["actor_id_bytes"],
        signing_key=bytes(target["signing_key"]),
    ) as target_client:
        knocks = target_client.call("fauna.knocks.list", {}).get("knocks", [])
    assert test_user["actor_id_hex"] in [k["sender"] for k in knocks], (
        f"{address} should hold a knock from {test_user['actor_id_hex']}; "
        f"knocks={knocks}"
    )
    assert_use_case_walked(d, "add-a-contact")


@pytest.mark.use_case("message-someone")
@pytest.mark.feature("conversations")
@pytest.mark.real_conversations
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_a_user_starts_a_conversation_and_sends_a_message(
    real_faunamls_app, second_real_faunamls_app
):
    alice = second_real_faunamls_app.owner_app
    bob_app, bob = second_real_faunamls_app
    d = alice.driver
    assert bob["handle"], "control: bob needs a handle to be addressed by it"
    address = f"{bob['handle']}@{MAIL_PRIMARY_DOMAIN}"
    body = f"hello {secrets.token_hex(4)}"

    alice.feed.navigate()

    click_nav_tab(d, "conversations-tab", canary="new-conversation-button")
    d.click("new-conversation-button")
    alice.conversations._type_recipient(address)
    state = d.get_attr("recipient-resolve-status", "state")
    assert state == "resolved", (
        f"typing {address}, an account on this nest, left the picker at "
        f"{state!r}: {d.diagnose('recipient-resolve-status')}; "
        f"error={alice.error_text()!r}"
    )
    d.type_text("dm-text-field", body)
    d.click("dm-send-button")

    # alice's seat: her message in the thread.
    wait_until(
        lambda: any(
            body in d.get_text("dm-message-text", index=i)
            for i in range(d.count("dm-message-text"))
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"alice's message {body!r} never rendered in the thread: "
            f"{d.diagnose('dm-message-text')}; error={alice.error_text()!r}"
        ),
    )

    # bob's seat: the thread carrying it, once his receive loop has looked.
    def _bob_has_it():
        return any(
            body in t.snippet for t in bob_app.conversations.list_threads()
        )

    deadline_cycles = 3
    for _ in range(deadline_cycles):
        if _bob_has_it():
            break
        cycles = conv_receive_cycles(bob_app.driver)
        poke_receive_cycle(bob_app.driver)
        await_receive_cycle_after(
            bob_app.driver,
            cycles[0] if cycles else None,
            budget_s=RECEIVE_CYCLE_S,
            what="alice's message",
        )
    assert _bob_has_it(), (
        f"alice's message {body!r} never reached bob's conversations after "
        f"{deadline_cycles} receive cycles that began after her send; bob's "
        f"threads: {bob_app.conversations.list_threads()}"
    )
    assert_use_case_walked(d, "message-someone")
