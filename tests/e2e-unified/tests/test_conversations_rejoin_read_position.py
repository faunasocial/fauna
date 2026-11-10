"""A group you are removed from and brought back into opens at the read
position you had before — the app-level witness of a re-join resuming.

``docs/features/group-conversations.md`` § Coverage contract, outcome 26, and
the kept-device arm of ``docs/features/conversations.md`` outcome 26 (its
later-enrolled arm is ``test_conversations_rejoin_later_enrolled_device.py``).
Authority: ``docs/goal/architecture/delegable-scope-reclamation.md``
§ Delegable-scope reclamation, parts (6) and (7) — a departed scope's entries
stay on the devices, and "a re-joined conversation resumes at the old position
wherever a device kept it"; the read marker itself is
``docs/goal/behavior/conversation-read-state.md``. The shared-Rust proof on
the real handlers is ``conformance_account_runtime.rs::
a_left_conversations_rows_leave_the_nest_and_a_re_join_writes_them_back``;
this is the same promise through a real app, a real nest and a real MLS
removal and re-admission.

**Who departs.** The departure is a removal by the other member, not the
user's own leave: a plain group offers no leave gesture (``room-leave-button``
is a governed room's), and the re-adder has to be an engine that stayed in the
group. The sender is a one-shot ``mls-group-gen --rejoin`` engine that mints
the whole stream at once — the Welcome, the messages, the Remove commit, the
Add commit with a second Welcome from the user's second key package, and a
message in the re-joined epoch — and the test posts each piece at its own
moment. Arranging the world is setup (convention 8 carve-out (b)); the user's
own acts — opening the thread, going back to the list, opening it again — go
through the app.

The sequence, and why each step is there:

1. The sender seats the user and posts message one; the user opens the thread
   (the read raises the marker through message one) and goes back to the list,
   which closes the thread on tui (``show_list`` clears the selection).
   **Durability barrier**: ``read_marker_state`` reports the raise.
2. Message two arrives while the thread is closed: one unread. This is the
   position the re-join must resume at — read through one, two unread-able.
3. The sender removes the user (the Remove commit) and re-adds them (the Add
   commit and the second Welcome, acked by the app's inbox drain), then posts
   message three in the re-joined epoch.
4. The witness: the thread reads **two** unread — messages two and three, the
   ones above the kept marker. A re-join that lost the position reads three
   (nothing read) or one (a launch floor or a reset marker calling message two
   history), so only the kept position gives two. Then opening the thread reads
   both and raises the marker past message three.
"""

from __future__ import annotations

import json

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import await_account_runtime_assembled, wait_until
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

FIRST_BODY = "re-join witness: read before the removal"
SECOND_BODY = "re-join witness: arrived unread before the removal"
THIRD_BODY = "re-join witness: sent after the re-join"


def _read_marker(app, channel_hex: str) -> dict | None:
    raw = app.driver.call_command("read_marker_state", {"channel_id_hex": channel_hex})
    if raw is None:
        return None
    return json.loads(raw) if isinstance(raw, str) else raw


def _thread(app, channel_hex: str):
    return next(
        (
            t
            for t in app.conversations.list_threads()
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex
        ),
        None,
    )


def _describe(app, channel_hex: str) -> str:
    t = _thread(app, channel_hex)
    if t is None:
        return "the thread is not listed"
    return (
        f"messages={t.message_count} unread={t.unread_count} "
        f"participants={t.participant_count}; marker={_read_marker(app, channel_hex)!r}"
    )


@pytest.mark.feature("group-conversations")
@pytest.mark.feature("conversations")
def test_a_group_left_and_re_joined_opens_at_the_old_read_position(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    # Two key packages: the re-admission cannot reuse the first one's init key.
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) >= 2,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to publish two key packages",
    )
    first_kp = conv_api.keypackage_fetch(port, sender, me)
    second_kp = conv_api.keypackage_fetch(port, sender, me)
    assert first_kp and second_kp and first_kp != second_kp, (
        "the app's user should have two distinct fetchable key packages"
    )
    stream = conv_api.mint_group_rejoin_stream(
        bytes(sender["signing_key"]),
        first_kp,
        second_kp,
        [FIRST_BODY, SECOND_BODY],
        [THIRD_BODY],
    )
    channel_hex = stream.channel_id_hex
    first, second = stream.before
    (third,) = stream.after

    # ── 1. Joined; message one is read; back to the list. ──
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, stream.welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, first)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to decrypt message one",
    )
    assert _read_marker(app, channel_hex) is not None, (
        "this app does not answer the `read_marker_state` reader; every "
        "runtime-hosting app registers it over the shared "
        "fauna_account_plane::account_driver::e2e_readers::read_marker_state"
    )
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"opening the thread should read message one: {_describe(app, channel_hex)}",
    )
    wait_until(
        lambda: (_read_marker(app, channel_hex) or {}).get("found") is True,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the read never raised the marker: {_describe(app, channel_hex)}",
    )
    read_through = _read_marker(app, channel_hex)["through"]
    app.conversations.navigate()

    # ── 2. Message two arrives with the thread closed. ──
    conv_api.channel_send(port, sender, channel_hex, second)
    wait_until(
        lambda: _thread(app, channel_hex).message_count >= 2
        and _thread(app, channel_hex).unread_count == 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "message two should arrive unread with the thread closed: "
        f"{_describe(app, channel_hex)}",
    )

    # ── 3. Removed, then brought back in; message three in the new epoch. ──
    conv_api.channel_send(port, sender, channel_hex, stream.remove_commit)
    conv_api.channel_send(port, sender, channel_hex, stream.add_commit)
    rejoin_inbox = conv_api.welcome_deliver(
        port, sender, me, channel_hex, stream.second_welcome
    )
    conv_api.await_welcome_applied(port, test_user, rejoin_inbox, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, third)

    # ── 4. The witness: the old position, so two unread. ──
    wait_until(
        lambda: _thread(app, channel_hex).message_count >= 3,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the re-joined app should decrypt message three: "
        f"{_describe(app, channel_hex)}",
    )
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 2,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the re-joined thread should open at the old position "
        f"(read through {read_through}: messages two and three unread); "
        f"{_describe(app, channel_hex)} — 3 means the position was lost, 1 "
        "that message two was called history",
    )
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 0
        and ((_read_marker(app, channel_hex) or {}).get("through") or 0) > read_through,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "opening the re-joined thread should read both and raise the "
        f"marker past {read_through}: {_describe(app, channel_hex)}",
    )
