"""tier_3: two refusals on the app's mail send path, each shown to the user
instead of failing silently.

The witnesses for ``docs/features/email-in-conversations.md`` outcomes 10 and 12:

- **The sending limit** (``docs/goal/behavior/mail-app-surface.md`` § Outbound
  metering). The nest caps what one account puts on the outbound queue at a
  hard-coded 100 an hour (``SEND_RATE_LIMIT_PER_HOUR``,
  ``bins/fauna-nest/src/email_handlers.rs``), counted by the authenticated
  submitter (``outbound_mail_queue.submit_actor_id``), and a send over it is
  refused with ``fauna.email.rate_limited``. The app must say so.
- **No address of its own** (§ First-party client send). Before the account
  has a handle there is no ``From:`` it may send as, and the shared send path
  refuses the message on the spot, before anything reaches the wire
  (``SmtpBackend::send``'s ``SelfAddress::usable`` pre-check,
  ``libs/fauna-conversations/src/backends/smtp.rs``), with
  ``error.email.no_handle``. The nest never produces that text, so seeing it
  is itself proof the refusal was the app's own.

Both render the one way every send failure renders (``ui/conversations.md``
§ Errors & edge cases): ``conversations.unified.error_send`` with the
localized refusal as the whole of its ``{message}`` — the same equality
``test_mail_client_send.py``'s over-size refusal asserts.

**How the limit is reached (e2e rule 8b).** Sending a hundred real messages
through the app first would make the journey the hundred sends. Instead the
precondition — this account already sent 100 messages in the last hour — is
written as 100 already-``sent`` queue rows stamped with the account as their
submitter. ``sent`` rows are never picked up by the delivery worker, and the
window count reads every row regardless of state, so the nest meets exactly
the history it would have after 100 sends. The ceiling itself is untouched:
it is a Rust constant, deliberately not a knob, and this test adds no seam to
lower it.

**Dedicated accounts.** Each test signs the app in as a fresh account: the
limit's fixture rows would otherwise refuse every later external send of the
session's shared user for an hour, and the no-address case needs an account
admitted without a handle (``create_actor_and_register(with_handle=False)``,
the shape ``public-mode.md`` § A handle-less account describes).

**Convention 14.** Each refusal is awaited as page state (``error-message``)
under a named budget, never a sleep.
"""

from __future__ import annotations

import sqlite3
import time
import uuid

import pytest

from common import create_actor_and_register
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

# Routed to the in-process stub MX by `mail_bridge_mta`, so the recipient is an
# outside address and the send takes the path the hourly ceiling meters.
EXTERNAL_RECIPIENT = "recipient@external.test"

# The shipped ceiling (`SEND_RATE_LIMIT_PER_HOUR`). Written here only to size
# the fixture history; the nest's own constant is what refuses.
SEND_RATE_LIMIT_PER_HOUR = 100


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.tui
@pytest.mark.feature("email-in-conversations")
def test_send_at_the_sending_limit_is_refused_with_a_message_saying_so(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    # ── The precondition: this account sent 100 messages in the last hour.
    _prefill_sent_history(nest_instance, user["actor_id_bytes"], SEND_RATE_LIMIT_PER_HOUR)

    # ── The journey: write to someone outside and send.
    subject = f"Over the sending limit {uuid.uuid4().hex[:8]}"
    app.conversations.start_new_conversation(
        EXTERNAL_RECIPIENT, subject=subject, body="One more than the hour allows."
    )

    error_text = _await_refusal(app, "a send over the hourly limit")
    want = S.conversations.unified.error_send(message=S.error.email.rate_limited)
    assert error_text == want, (
        "a send refused at the sending limit must say so, as the localized "
        f"rate-limit text inside error_send — got {error_text!r} (want {want!r})"
    )

    # The refusal is the whole outcome: nothing joined the queue.
    queued = _queued_by(nest_instance, user["actor_id_bytes"])
    assert queued == SEND_RATE_LIMIT_PER_HOUR, (
        f"the refused message must not be queued: the account's queue rows went "
        f"from {SEND_RATE_LIMIT_PER_HOUR} to {queued}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.tui
@pytest.mark.feature("email-in-conversations")
def test_send_before_the_account_has_an_address_is_refused_on_the_spot(
    app, nest_instance, mail_bridge_mta
):
    from helpers.e2e_session import login_as

    user = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        with_handle=False,
    )
    # `handle=""`: the session carries the nest's truth, no handle — a
    # fabricated one would give the app a `From:` to send as.
    login_as(app, nest_instance, user, handle="", device_id="test-device-no-handle")

    subject = f"No address yet {uuid.uuid4().hex[:8]}"
    app.conversations.start_new_conversation(
        EXTERNAL_RECIPIENT, subject=subject, body="Sent before choosing a handle."
    )

    error_text = _await_refusal(app, "a send from an account with no handle")
    want = S.conversations.unified.error_send(message=S.error.email.no_handle)
    assert error_text == want, (
        "a send before the account has an address must be refused with the "
        f"reason, as the localized no-handle text inside error_send — got "
        f"{error_text!r} (want {want!r})"
    )

    # On the spot: nothing reached the nest's queue.
    queued = _queued_by(nest_instance, user["actor_id_bytes"])
    assert queued == 0, f"the refused send must never reach the queue; {queued} row(s) did"


def _await_refusal(app, what: str) -> str:
    return wait_until(
        lambda: app.error_text(),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"no refusal surfaced after {what} — a swallowed refusal is the "
            "silent-drop shape e2e convention 11 forbids. "
            f"dm-send-button visible={app.driver.is_visible('dm-send-button')!r}, "
            # The state slot and the element side by side: a refusal on screen
            # while `messages.error` reads empty is the app's state mirror
            # losing it, not a swallowed send.
            f"state messages.error={app._message_from_state('error')!r}, "
            f"error-message element={app._message_from_element('error-message')!r}, "
            f"threads={[(t.label, t.snippet) for t in app.conversations.list_threads()][:5]!r}"
        ),
    )


def _prefill_sent_history(nest_instance, actor_id: bytes, count: int) -> None:
    now = int(time.time())
    tag = uuid.uuid4().hex[:8]
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        conn.executemany(
            "INSERT INTO outbound_mail_queue "
            "(original_msgid, original_sender, recipient, raw_message, "
            " next_attempt_at, status, created_at, submit_actor_id) "
            "VALUES (?1, ?2, ?3, ?4, ?5, 'sent', ?6, ?7)",
            [
                (
                    f"<limit-fixture-{tag}-{i}@external.test>",
                    "fixture-history@external.test",
                    EXTERNAL_RECIPIENT,
                    b"",
                    now,
                    now,
                    actor_id,
                )
                for i in range(count)
            ],
        )
        conn.commit()
    finally:
        conn.close()


def _queued_by(nest_instance, actor_id: bytes) -> int:
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM outbound_mail_queue WHERE submit_actor_id = ?1",
            (actor_id,),
        ).fetchone()[0]
    finally:
        conn.close()
