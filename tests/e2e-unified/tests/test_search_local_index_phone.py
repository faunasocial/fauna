"""tier_3: a mail that just arrived is found on a phone by a phrase from its body,
through the index a desktop seat of the same account built and synced.

`test_search_local_index.py`'s journey in the phone's real shape
(`docs/goal/behavior/content-index.md` § Build vs. query: "a desktop app builds
and syncs; phones receive the segments and query the synced copy"). A phone holds
no index builder, so a single seat could never find its own mail, and the hit
here is the whole split working: real SMTP into the mailbox, the desktop seat
opening the mail and publishing a sealed segment to `__index`, and the phone's
local arm querying the replica it synced.

**Why a hit can only come from the local index:** the needle is in a private
mail's body alone, and the nest's `content_fts` never holds private mail. The
needle stays out of the subject and the sender, so the hit is a body hit too.

**The desktop launches after the delivery.** Its fresh state root has nothing to
read but the mailbox it drains at login, so nothing has to be timed, and only a
declared `alice_builder_seats` lets the prebuild hoist build the macos app.

tier_3: every binary real, real SMTP wire, two real app processes.
"""

import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, UI_SETTLE_S
from helpers.mail_client_ui import (
    deliver_inbound,
    route_inbound_mail_to_app,
    wait_for_thread_with_nonce,
)
from helpers.search_journeys import LOCAL_INDEX_S
from helpers.waiting import wait_until

# ios and android pair with a builder seat (`alice_builder_seats`: macos, tui).
# android's witness is NOT YET RUN on a device (no venue).
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.ios,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

SENDER = "sender@external.test"


def _message(recipient: str, subject_tag: str, needle: str) -> bytes:
    """A mail whose subject carries `subject_tag`, for the delivery barrier, and
    whose body alone carries `needle`."""
    return (
        "\r\n".join(
            [
                f"From: External Sender <{SENDER}>",
                f"To: {recipient}",
                f"Subject: Quarterly report {subject_tag}",
                f"Message-ID: <{needle}@external.test>",
                "Date: Mon, 25 May 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"The {needle} report is attached for your review.",
            ]
        )
        + "\r\n"
    ).encode()


@pytest.mark.feature("private-search-index")
def test_a_phone_finds_a_received_mail_in_the_index_a_desktop_seat_built(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, alice_builder_seats
):
    phone = logged_in_app
    stamp = int(time.time() * 1000)
    subject_tag = f"phonetag{stamp}"
    needle = f"phonebody{stamp}qx"
    log = mail_bridge_mta.log_file

    recipient = route_inbound_mail_to_app(
        phone, mail_bridge_mta, nest_instance, test_user, "e2esearchphone"
    )
    deliver_inbound(
        mail_bridge_mta.mx_port, mail_bridge_mta.domain, SENDER, recipient,
        _message(recipient, subject_tag, needle), time.monotonic() + 40.0,
    )
    # The mail arrived on the phone, so a search miss below is never a delivery
    # failure.
    wait_for_thread_with_nonce(
        phone, subject_tag, what="the phone", budget_s=MLS_HANDSHAKE_S, bridge_log=log
    )

    # The builder: it drains the same mailbox, opens the mail and publishes. Its
    # own hit is the proof the segment holds the body, before the phone is asked.
    desktop, _ = alice_builder_seats.launch()
    desktop.search.navigate()

    def desktop_finds():
        desktop.search.query(needle)
        return desktop.search.result_count() >= 1

    wait_until(
        desktop_finds,
        LOCAL_INDEX_S,
        diagnose=lambda: (
            f"the desktop seat never indexed the mail body {needle!r}: "
            f"{desktop.driver.diagnose('search-result-item')} "
            f"error={desktop.error_text()!r}; bridge log: {log}"
        ),
    )

    # The phone: no builder of its own, so this hit is the synced replica.
    phone.search.navigate()

    def phone_finds():
        phone.search.query(needle)
        return phone.search.result_count() >= 1

    wait_until(
        phone_finds,
        LOCAL_INDEX_S,
        diagnose=lambda: (
            f"the phone never found {needle!r} in the index the desktop published: "
            f"{phone.driver.diagnose('search-result-item')} "
            f"error={phone.error_text()!r}"
        ),
    )
    result_text = phone.search.result_text(0)
    assert needle in result_text or subject_tag in result_text, (
        f"the phone's row must name the matched mail; got {result_text!r}"
    )

    # Activating the row opens the mail's thread on the phone, on the message the
    # row named (`SearchNav::Mail`). A single-pane stack follows its own path, so a
    # selection nothing pushes leaves the user on the list.
    phone.search.open_result(0)
    phone.conversations.driver.wait_for("thread-header", timeout=15.0)
    wait_until(
        lambda: phone.conversations.selected_message_index() is not None,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the thread opened but no message is marked, so the row's message_id "
            f"was dropped: {phone.conversations.driver.diagnose('dm-message-timestamp')}"
        ),
    )
    selected = phone.conversations.selected_message_index()
    marked = phone.conversations.driver.get_text("dm-message-text", index=selected)
    assert needle in marked, (
        f"the marked message must be the one the row named ({needle!r}); "
        f"bubble {selected} reads {marked!r}"
    )
