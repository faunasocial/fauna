"""Tier 3: a mail app keeps sending month after month with nothing re-entered
— the background submission-token refresh (`docs/goal/behavior/mail-credentials.md`
§ Architectural rules, rule 10: "SubmissionToken refreshes on a 7-day cadence
… any online client refreshes"; turn-on-mail outcome 25).

The outbound `SubmissionToken` the app seals under each credential lives 30
days, and the nest refuses an expired one at the outbound enqueue
(`mail_submitter_proof.rs`, the submitter proof's expiry check — the MTA answers
`550 5.7.1`). The app re-mints every credential's token once it is a week old,
on every connect and once a day in between
(`rpc_glue::refresh_epoch_schedule_on_every_connect` → `MailSettingsMachine::
refresh_submission_tokens`).

The journey, on real binaries:

  the nest admin enables mail in the app (PLAIN) → a mail app submits to an
  outside recipient: relayed → the nest's outbound clock moves 31 days on: the
  same submission is refused 550 (the token the app minted at enable has expired —
  the hazard this refresh exists for) → the app's token clock moves 8 days on and
  the nest bounces, so the app's running session reconnects and its connect-time
  pass re-mints the week-old token → the nest's clock moves 31 days on again:
  the mail app's submission, same password, is relayed.

Both clocks are convention 14's fake clocks, never a sleep: the nest's
outbound clock (`POST /api/v1/test/outbound/clock`, `test-hooks`) and the app's
token-refresh clock (the `mail_token_refresh_advance_clock` agent command),
which moves the re-mint's `now` and the new token's `issued_at` only, never
the bearer clock. The re-mint is driven by the production trigger — a real
reconnect — and observed by its own log line; the outcome is the relay at the
stub MX.
"""

from __future__ import annotations

import time

import pytest

from common.nest import restart_nest
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.mail_wire import (
    _connect_submission_tls,
    _smtp_auth_plain,
    _wait_for_tagged,
    outbound_clock,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
]

_MUA_PASSWORD = "token-refresh-plain-pw-1"
_DAY = 86_400
#: One day past the token's 30-day lifetime (`SUBMISSION_TOKEN_LIFETIME_SECS`).
_PAST_EXPIRY = 31 * _DAY
#: One day past the 7-day re-mint cadence
#: (`token_refresh::SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS`).
_PAST_CADENCE = 8 * _DAY

#: The line the connect-time trigger logs when its pass re-minted a token
#: (`libs/fauna-client-mail-settings/src/rpc_glue.rs`).
_REMINTED = "mail submission-token refresh: re-minted"


def _submit(handle, sender: str, token: str, final_code: str) -> str:
    """One mail-app submission to an outside recipient, authenticated with the
    app-minted password; `final_code` is the reply the end-of-data `.` must
    get. AUTH unwraps the submission token the app sealed under the password;
    the enqueue behind `.` is where the nest checks the token's expiry."""
    rcpt = "recipient@external.test"
    message = "\r\n".join([
        f"From: <{sender}>",
        f"To: {rcpt}",
        f"Subject: token refresh {token}",
        f"Message-ID: <{token}@{handle.domain}>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"body {token}",
    ]) + "\r\n"
    deadline = time.monotonic() + 40.0
    with _connect_submission_tls(handle.submission_port_465, handle.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {handle.domain}", "250", deadline)
        _smtp_auth_plain(conn, sender, _MUA_PASSWORD, deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(message.encode())
        reply = conn.cmd(".", final_code, deadline)
        conn.cmd("QUIT", "221", deadline)
    return reply


def _relayed(handle, sender: str, what: str) -> None:
    token = f"tokenrefresh{int(time.time() * 1000)}qx"
    _submit(handle, sender, token, "250")
    assert _wait_for_tagged(handle.stub_mx, token, timeout=20.0) is not None, (
        f"{what}: the accepted submission never reached the outside MX "
        f"(see {handle.bridge_log_hint()})"
    )


def _remints(app) -> int:
    return app.driver.app_stderr_text().count(_REMINTED)


@pytest.mark.feature("turn-on-mail")
def test_a_mail_app_keeps_sending_after_the_first_token_would_have_expired(
    app, dedicated_mail_nest, request
):
    app.mail_settings.require_token_refresh_clock_supported()
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest

    # ── 1. Mail on in the app; a mail app sends with the password it minted.
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    sender = alias_admin_to_address(nest, handle.domain)
    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "enabling mail must flip the page to enabled; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    handle.rebind_after_enable()
    _relayed(handle, sender, "right after enabling mail")

    try:
        # ── 2. A month on, the token minted at enable has expired: the nest
        # refuses the submitter, and the mail app is told so.
        outbound_clock(nest["url"], set=int(time.time()) + _PAST_EXPIRY)
        refused = _submit(handle, sender, f"expired{int(time.time() * 1000)}qx", "550")
        assert "5.7.1" in refused, f"an expired token must be refused 5.7.1; got {refused!r}"
        outbound_clock(nest["url"], set=0)

        # ── 3. A week on for the app, then a real reconnect: the connect-time
        # pass re-mints the week-old token.
        app.mail_settings.advance_token_refresh_clock(_PAST_CADENCE)
        remints_before = _remints(app)
        restart_nest(nest, graceful=True)
        wait_until(
            lambda: _remints(app) > remints_before,
            ORCHESTRATION_STEP_S,
            diagnose=lambda: "the app's reconnect never re-minted the week-old token; "
            "the app's mail log:\n" + "\n".join(
                line for line in app.driver.app_stderr_text().splitlines()[-200:]
                if "mail" in line or "reconnect" in line
            ),
        )

        # ── 4. A month on from the enable again: the re-minted token is
        # inside its lifetime, so the same mail app, same password, sends.
        outbound_clock(nest["url"], set=int(time.time()) + _PAST_EXPIRY)
        _relayed(handle, sender, "a month after enabling, past the first token's expiry")
    finally:
        outbound_clock(nest["url"], set=0)
        app.mail_settings.advance_token_refresh_clock(0)
