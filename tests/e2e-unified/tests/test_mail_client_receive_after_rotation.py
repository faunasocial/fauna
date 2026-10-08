"""tier_3: mail received before a rotation of the mail keys stays readable
afterwards.

The witness for ``docs/features/email-in-conversations.md`` outcome 11, and
the rule ``docs/goal/behavior/mail-app-surface.md`` § Inbound client receive
ratifies (built 2026-09-15): the client opens mail with *the complete standing
set, never the current generation alone* — the current MSEK's keypair plus one
per prior generation in ``MailConfig.prior_mseks`` — *"so after a
rotate-mail-keys the user's pre-rotation standing mail stays readable in the
conversations view for ever"* (every generation is carried since 2026-10-06;
``test_mail_survives_four_key_rotations.py`` is the four-rotation witness).
Before that, a rotation blinded the app to every record sealed before it. The
shared derivation is pinned in Rust; this is the journey through the app.

The journey, through the app UI alone (convention 8):

  mail on → email A arrives, sealed to the first key generation, and shows
  decrypted in Conversations → the user rotates the mail keys (Settings →
  Mail → Rotate mail keys → confirm) and the rotation finishes → email B
  arrives, sealed to the new generation → **the app is restarted** → A and B
  both show decrypted.

**Why the restart is load-bearing.** Within one process A is already past the
receive cursor and on screen, so nothing re-opens it. A fresh process
re-drains the mailbox from UID 0 under the keys the account holds *now*, which
is the only moment the standing set is actually exercised
(``test_mail_client_receive_after_reenable.py`` makes the same restart for the
same reason, and is this test's opposite: there the keys are torn down, so the
old record is skipped).

**What makes the witness non-vacuous.** If the rotation did nothing, A and B
would both open under the one unchanged key and the test would pass for the
wrong reason. So it checks that the rotation happened: the recipient key the
nest seals inbound mail to (``actor_mls_pubkeys``, republished by the rotation)
changed. B, delivered after that, is sealed to the new generation, and it
showing after the restart proves the restarted app holds the new key as
current — the finished rotation, not an interrupted one. A showing beside it
is the standing set at work: its key survives only as a prior generation.

**The finish is observed, not slept for.** The rotate form holds open with its
progress line while the rotation runs and closes when it returns
(``ui/mail-settings.md`` § Element visibility), so the restart can never cut a
rotation short (convention 14).

**A dedicated account.** A rotation ages the account's key history by one
generation; the session's shared user is rotated by other modules, so this
journey runs on a fresh account whose history it alone decides.

tier_3: every binary real, the real SMTP wire, a real process restart
(``hard_reload()``, replaying the login).
"""

from __future__ import annotations

import sqlite3
import time
import uuid

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, ORCHESTRATION_STEP_S
from helpers.mail_client_ui import (
    deliver_inbound,
    plain_message,
    wait_for_thread_with_nonce,
)
from helpers.mail_aliases import add_exact_alias

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
    pytest.mark.windows,
    pytest.mark.tui,
    pytest.mark.real_conversations,
]

SENDER = "sender@external.test"


@pytest.mark.feature("email-in-conversations")
def test_mail_received_before_a_key_rotation_stays_readable_after_it(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    # ── 1. Mail on, and an address routed to this account.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    recipient = add_exact_alias(
        nest_instance["url"], user["signing_key"], mail_bridge_mta.domain,
        f"e2erotate{uuid.uuid4().hex[:8]}",
    )

    def deliver(nonce: str) -> None:
        deliver_inbound(
            mail_bridge_mta.mx_port, mail_bridge_mta.domain, SENDER, recipient,
            plain_message(SENDER, recipient, nonce), time.monotonic() + 40.0,
        )

    def wait_for(nonce: str, what: str):
        return wait_for_thread_with_nonce(
            app, nonce, what=what, budget_s=MLS_HANDSHAKE_S, bridge_log=mail_bridge_mta.log_file,
        )

    # ── 2. Email A arrives under the first key generation and shows.
    nonce_a = f"rotatea{uuid.uuid4().hex[:10]}qx"
    deliver(nonce_a)
    app.conversations.navigate()
    wait_for(nonce_a, "before the rotation")

    # ── 3. The user rotates the mail keys, and the rotation finishes.
    key_before = _recipient_key(nest_instance, user["actor_id_bytes"])
    app.mail_settings.navigate()
    app.mail_settings.rotate_keys()
    assert app.mail_settings.wait_for_rotation_to_finish(ORCHESTRATION_STEP_S), (
        "the rotation never finished: the rotate form is still open with "
        f"progress {app.driver.get_text('mail-rotate-keys-progress-indicator')!r}"
    )
    # The rotation's outcome folds its error in the same step that closes the
    # form, so one read after the close is the whole answer.
    page_error = (
        app.driver.get_text("error-message") if app.driver.is_visible("error-message") else ""
    )
    assert not page_error, f"the rotation reported an error: {page_error!r}"
    key_after = _recipient_key(nest_instance, user["actor_id_bytes"])
    assert key_after and key_after != key_before, (
        "rotating the mail keys must republish the recipient key inbound mail is "
        "sealed to; it is unchanged, so this run would witness nothing"
    )

    # ── 4. Email B arrives, sealed to the new generation.
    nonce_b = f"rotateb{uuid.uuid4().hex[:10]}qx"
    deliver(nonce_b)

    # ── 5. Restart: a fresh receive loop re-drains the mailbox under the keys
    # the account holds now. The session's config is nest-side, so the
    # re-confirm only re-fetches it.
    app.driver.hard_reload()
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    app.conversations.navigate()

    wait_for(nonce_b, "after the rotation + restart (sealed to the new generation)")
    found_a = wait_for(nonce_a, "after the rotation + restart (received before it)")
    assert found_a.rail == "Smtp", f"received mail must land on the Smtp rail; got {found_a.rail!r}"


def _recipient_key(nest_instance, actor_id: bytes) -> bytes | None:
    """The recipient key the MTA seals this account's inbound mail to."""
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        row = conn.execute(
            "SELECT mls_pubkey FROM actor_mls_pubkeys WHERE actor_id = ?1", (actor_id,)
        ).fetchone()
    finally:
        conn.close()
    return row[0] if row else None
