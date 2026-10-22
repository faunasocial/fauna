"""tier_3: the account's switch hides a received message's scan-verdict row.

A witness for ``docs/goal/behavior/mail-content-scanning.md`` § Per-account
opt-out for visible scan details: ``mail-settings-scan-result-visible-toggle``
off, a received message's bubble carries no ``dm-message-scan-result``; back
on, the same row is there again — the nest kept the verdict all along.

**Shared Rust decides it.** The app only writes the switch
(``set_scan_result_visible_and_reload``); the shared conversations projection
re-reads it when a thread opens and paints no row while it is off, so the test
re-opens the thread after each flip.

**Convention 14.** Every assertion waits for a state the UI moves to: the row
present (switch on), gone (off), present again (on). The first is the anchor —
a row that was painted and then disappears is the switch at work, never a
verdict that simply had not arrived yet. Nothing is timed.

tier_3: every binary real, the real SMTP wire and scan pipeline (the fake
clamd and rspamd of conftest's ``mail_bridge_mta``, as the sibling
``test_mail_scan_result_on_bubble.py``).
"""

from __future__ import annotations

import time
import uuid

import pytest

from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_aliases import add_exact_alias
from helpers.mail_client_ui import deliver_inbound, wait_for_thread_with_nonce
from helpers.waiting import wait_until
from tests.test_mail_scan_result_on_bubble import SENDER, _message

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.real_conversations,
]

SWITCH = "mail-settings-scan-result-visible-toggle"


@pytest.mark.feature("email-in-conversations")
def test_the_scan_result_switch_hides_and_restores_the_row(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    assert app.mail_settings.toggle_state(SWITCH) == "on", (
        "the scan-result switch defaults on (mail.account.scan_result_visible)"
    )
    recipient = add_exact_alias(
        nest_instance["url"], user["signing_key"], mail_bridge_mta.domain,
        f"e2eswitch{uuid.uuid4().hex[:8]}",
    )

    nonce = f"switch{uuid.uuid4().hex[:10]}qx"
    deliver_inbound(
        mail_bridge_mta.mx_port, mail_bridge_mta.domain, SENDER, recipient,
        _message(recipient, nonce), time.monotonic() + 40.0,
    )

    app.conversations.navigate()
    thread = wait_for_thread_with_nonce(
        app, nonce, what="the scanned message", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )

    def open_thread_and_wait(present: bool, why: str) -> None:
        app.conversations.navigate()
        app.conversations.open_thread_by_id(thread.thread_id)
        wait_until(
            lambda: bool(app.conversations.scan_result_rows()) == present or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"{why}: dm-message-scan-result rows "
                f"{app.conversations.scan_result_rows()!r} on the thread of {nonce!r}.\n"
                f"  conversations error: {app.error_text()!r}\n"
                f"  bridge log: {mail_bridge_mta.log_file}"
            ),
        )

    def flip_switch_to(want: str) -> None:
        app.mail_settings.navigate()
        app.mail_settings.click_toggle(SWITCH)
        wait_until(
            lambda: app.mail_settings.toggle_state(SWITCH) == want or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"the scan-result switch never repainted {want!r} from the nest's "
                f"re-read; it reads {app.mail_settings.toggle_state(SWITCH)!r}, "
                f"page error {app.mail_settings.page_error_text(timeout=1.0)!r}"
            ),
        )

    # ── The anchor: switch on (the default), the verdict shows.
    open_thread_and_wait(True, "with the switch on the verdict row never showed")

    # ── Off: the row goes, though the nest still holds the verdict.
    flip_switch_to("off")
    open_thread_and_wait(False, "with the switch off the verdict row stayed")

    # ── Back on: the kept verdict shows again.
    flip_switch_to("on")
    open_thread_and_wait(True, "with the switch back on the verdict row never returned")
