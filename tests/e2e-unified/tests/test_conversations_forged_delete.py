"""tier_3 e2e: a delete someone else forged is ignored on every copy
(``message-reactions.md`` § Reactions & message delete).

A ``Delete`` tombstones its target only when the claimant is the target's
sender, and the claimant is the MLS-authenticated leaf, never the sealed body's
own ``sender`` field. No app's UI offers a delete on someone else's message
(outcome 2's ``test_peer_message_delete_button_absent``), so the forger here is
a real member engine driven by the e2e harness library
(``fauna_ffi.harness_conv_forge_delete``): bob joins alice's conversation from
her Welcome like any client, then seals a delete of her message that CLAIMS
alice sent it. The shared-Rust pin of the same shape is ``fauna-conversations``'
``forged_cross_sender_delete_does_not_tombstone_victim_message``; this is the
witness that the app on the victim's side keeps her message on screen.

Its own module because ``real_conversations`` is session-wide on the families
that read it at launch (``real_faunamls_app``'s docstring).
"""

from __future__ import annotations

import secrets
import threading

import pytest

import fauna_ffi
from common.auth import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("reactions-and-message-delete")
def test_forged_delete_of_your_message_is_ignored(
    real_faunamls_app, nest_instance, test_user, tmp_path
):
    """alice sends bob a message through the compose bar; bob — a real member
    engine — sends a delete of it claiming to be alice. After a receive cycle
    that began after the forgery, alice's copy still paints her text and shows
    no ``dm-message-deleted``. Her own delete of the same message, through the
    ⋯ menu, still tombstones it: the control that the bubble CAN show one.

    A red that names the tombstone, with the forgery sent, is a product bug in
    the claimant floor (``ConversationsManager::apply_inbound_delete_claim``)
    or in what the engine reports as the sender, never a test to soften."""
    alice = real_faunamls_app
    d = alice.driver
    body = f"keep this {secrets.token_hex(4)}"

    bob = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    # bob accepts alice, so her DM Welcome is delivered rather than knocked.
    conv_api.accept_contact(nest_instance["port"], bob, test_user["actor_id_hex"])

    ready = tmp_path / "forger-ready"
    budget_s = int(MLS_HANDSHAKE_S + 2 * RECEIVE_CYCLE_S)
    result: dict = {}

    def _forge():
        try:
            result["ok"] = fauna_ffi.harness_conv_forge_delete(
                nest_instance["url"],
                bytes(bob["signing_key"]),
                test_user["actor_id_hex"],
                body,
                ready,
                budget_s,
            )
        except Exception as e:  # surfaced in the main thread's failure
            result["error"] = e

    forger = threading.Thread(target=_forge, name="forged-delete", daemon=True)
    forger.start()
    # bob is addressable only once his key packages are up; before that
    # alice's resolve would find no chip.
    wait_until(
        lambda: ready.exists() or "error" in result,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the forging member never published its key packages",
    )
    assert "error" not in result, f"the forging member failed: {result['error']!r}"

    # alice sends — the user's gesture through the new-conversation composer.
    alice.conversations.start_new_conversation(bob["actor_id_hex"], body=body)
    wait_until(
        lambda: d.count("dm-message-text") >= 1,
        RECEIVE_CYCLE_S,
        diagnose=lambda: f"alice's sent bubble never rendered: {d.diagnose('dm-message-text')}",
    )

    forger.join(budget_s + 30)
    assert not forger.is_alive(), "the forging member never returned"
    assert "error" not in result, f"the forging member failed: {result['error']!r}"
    forged = result["ok"]
    assert forged["delete_seq"] > forged["target_seq"], forged

    # alice's receive loop has looked past the forgery (convention 14's anchor),
    # then her copy is read.
    cycles = conv_receive_cycles(d)
    poke_receive_cycle(d)
    await_receive_cycle_after(
        d,
        cycles[0] if cycles else None,
        budget_s=RECEIVE_CYCLE_S,
        what="bob's forged delete",
    )
    assert d.count("dm-message-deleted") == 0, (
        "a delete bob sent, claiming alice's authorship, tombstoned alice's "
        f"message (forged delete at seq {forged['delete_seq']} naming seq "
        f"{forged['target_seq']} on {forged['channel']})"
    )
    texts = [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))]
    assert any(body in t for t in texts), (
        f"alice's message {body!r} is no longer painted after bob's forged delete; "
        f"bubbles: {texts}"
    )

    # Control: alice's own delete still tombstones the same message.
    alice.conversations.delete_message(
        message_index=d.count("dm-message-actions-button") - 1, timeout_s=12.0
    )
    alice.conversations.wait_for_message_deleted(timeout_s=RECEIVE_CYCLE_S)
    assert d.count("dm-message-deleted") >= 1, (
        "alice's own delete did not tombstone her message — the bubble cannot "
        "show a tombstone at all, so the forgery assertion above proves nothing"
    )
