"""tier_3: mail read state rides the mailbox's own IMAP ``\\Seen`` flag
(``docs/goal/behavior/conversation-read-state.md`` § Mail: ``\\Seen`` is the
marker; the wire is ``mail-app-surface.md`` § Read state).

Two journeys, every binary real — a real inbound mail through the real MTA,
the real shared receive loop, the real nest flag write:

* **Unread at launch.** A delivered mail the user never opened is still unread
  after the app restarts. Before the mailbox flag was the marker, a restart's
  launch floor made every message stamped before it read — history — so a mail
  that arrived while the app was closed showed no indicator at all.
* **Read on one device, read on the other.** Two devices of one account both
  show the mail unread; opening it on the first sets ``\\Seen`` on the nest (one
  batched ``fauna.email.inbox.mark_seen``), the nest's ``fauna.mail.flags_changed``
  wake reaches the second, whose cursored ``flag_changes`` drain clears it —
  without a relaunch. The second device then restarts and the mail is still
  read, because the flag is on the nest, not in either app.

Both assert state (``unread_count`` off the published thread rows) under a named
budget, never a timing (convention 14). Threads are found by a per-test nonce in
the subject, so mail other tests leave in the session's shared inbox never
matches — and each device's own ``thread_id`` is looked up on that device, since
a thread id is minted per app run.

Test taxonomy:
- ``tier_3`` (mocking depth): every binary real, real SMTP wire, real seal →
  fetch → open → ingest, and the real flag write, wake and delta fetch.
"""

import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import (
    deliver_inbound,
    plain_message,
    route_inbound_mail_to_app,
    thread_with_nonce,
    wait_for_thread_with_nonce,
)
from helpers.waiting import wait_until

# tui leads (the default app set); linux shares the same session loop and the
# second-device fixture, and is listed so its sweep carries the witness too.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.real_conversations,
]

SENDER = "reader@external.test"


def _deliver(mail_bridge_mta, recipient_addr: str, nonce: str) -> None:
    deliver_inbound(
        mail_bridge_mta.mx_port,
        mail_bridge_mta.domain,
        SENDER,
        recipient_addr,
        plain_message(SENDER, recipient_addr, nonce),
        time.monotonic() + 40.0,
    )


def _unread(app, nonce: str):
    """The nonce thread's ``unread_count`` on ``app``, or ``None`` when it is not
    in the list (yet)."""
    thread = thread_with_nonce(app, nonce)
    return None if thread is None else thread.unread_count


def _wait_unread(app, nonce: str, expected: int, *, what: str, bridge_log) -> None:
    wait_until(
        lambda: _unread(app, nonce) == expected,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"{what}: the mail tagged {nonce!r} should read unread_count == "
            f"{expected}; it reads {_unread(app, nonce)!r}.\n"
            f"  threads: {[(t.label, t.unread_count, t.rail) for t in app.conversations.list_threads()]}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {bridge_log}"
        ),
    )


@pytest.mark.web
@pytest.mark.feature("email-in-conversations")
def test_a_mail_nobody_opened_is_still_unread_after_a_restart(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    app = logged_in_app
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "readstate-restart"
    )
    nonce = f"unreadrestart{int(time.time() * 1000)}qx"
    _deliver(mail_bridge_mta, recipient, nonce)
    wait_for_thread_with_nonce(
        app, nonce, what="before the restart", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )
    _wait_unread(app, nonce, 1, what="arrived while running",
                 bridge_log=mail_bridge_mta.log_file)

    # The restart re-drains the mailbox from the nest. Every record is stamped
    # before this run began, so the launch floor would call it history; the
    # mail rail reads the missing `\Seen` instead.
    app.driver.hard_reload()
    wait_for_thread_with_nonce(
        app, nonce, what="after the restart", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )
    _wait_unread(app, nonce, 1, what="after the restart, never opened",
                 bridge_log=mail_bridge_mta.log_file)


@pytest.mark.feature("email-in-conversations")
def test_a_mail_read_on_one_device_reads_on_the_other_and_stays_read(
    request, logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    device_a = logged_in_app
    recipient = route_inbound_mail_to_app(
        device_a, mail_bridge_mta, nest_instance, test_user, "readstate-seats"
    )
    nonce = f"readseats{int(time.time() * 1000)}qx"
    _deliver(mail_bridge_mta, recipient, nonce)
    log = mail_bridge_mta.log_file

    wait_for_thread_with_nonce(device_a, nonce, what="device A", budget_s=MLS_HANDSHAKE_S,
                               bridge_log=log)
    _wait_unread(device_a, nonce, 1, what="device A before any read", bridge_log=log)

    # The same account's second device: its own launch, its own receive loop,
    # the same mailbox.
    device_b, _driver_b = request.getfixturevalue("alice_second_device")
    wait_for_thread_with_nonce(device_b, nonce, what="device B", budget_s=MLS_HANDSHAKE_S,
                               bridge_log=log)
    _wait_unread(device_b, nonce, 1, what="device B before any read", bridge_log=log)

    # Device A opens the thread — the read, and the `\Seen` write it owes.
    device_a.conversations.open_thread_by_id(thread_with_nonce(device_a, nonce).thread_id)
    _wait_unread(device_a, nonce, 0, what="device A after opening it", bridge_log=log)

    # Device B hears of it with no relaunch: the nest's flag-change wake, then
    # one cursored flag_changes drain.
    _wait_unread(device_b, nonce, 0,
                 what="device B, after device A read the mail (no relaunch)",
                 bridge_log=log)

    # And it is the nest's flag, not device B's memory: a restart re-drains the
    # mailbox and still finds the mail read.
    device_b.driver.hard_reload()
    wait_for_thread_with_nonce(device_b, nonce, what="device B after its restart",
                               budget_s=MLS_HANDSHAKE_S, bridge_log=log)
    _wait_unread(device_b, nonce, 0, what="device B after its restart", bridge_log=log)
