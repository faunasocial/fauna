"""tier_3: a full mailbox says so on mail-settings.

A witness for ``docs/features/turn-on-mail.md`` outcome 23 and
``docs/goal/behavior/imap-server.md`` § Composition with submission quota: the
mail-settings page shows how much of the mailbox is used
(``mail-settings-mailbox-quota``), the daily sending allowance beside it
(``mail-settings-submission-quota``) and, once a limit has run out, the
over-quota notice (``mail-settings-over-quota-notice``) — the shared
``mail_quota_text`` / ``mail_over_quota_notice`` over ``get_mail_quotas``.

**How the mailbox fills.** One real message is delivered over the MTA, then the
admin lowers the deployment's mailbox size (``put_imap_policy``
``storage_bytes_default``) to one byte — below what the message already uses —
and the member reopens the page. The admin's write is the arrangement, not the
journey: the journey is what the member sees. The policy is put back to its
default in teardown, as the sibling ``test_mail_policy_effects.py`` does.

**Ground truth.** The readout is compared with the nest's own
``get_mail_quotas`` answer for the same member, read over the same user
WS-RPC surface the page reads (rule 5).

**Convention 14.** Both elements are waited for as UI state; nothing is timed.

tier_3: every binary real, the real SMTP wire.
"""

from __future__ import annotations

import time
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_aliases import add_exact_alias
from helpers.mail_client_ui import deliver_inbound, plain_message, wait_for_thread_with_nonce
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
]

SENDER = "sender@external.test"


def _byte_size(n: int) -> str:
    """`fauna_core::format::byte_size`, resolved: the largest binary unit under
    the value, one decimal, a trailing `.0` dropped."""
    units = [(1 << 40, S.size.tb), (1 << 30, S.size.gb), (1 << 20, S.size.mb), (1 << 10, S.size.kb)]
    for scale, unit in units:
        if n >= scale:
            value = f"{n / scale:.1f}"
            return unit(value=value[:-2] if value.endswith(".0") else value)
    return S.size.bytes(value=str(n))


def _ws(nest_instance, signing_key):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(signing_key.verify_key),
        signing_key=bytes(signing_key),
    )


@pytest.mark.feature("turn-on-mail")
def test_a_full_mailbox_shows_the_notice_and_the_used_size(
    app, request, nest_instance, mail_bridge_mta
):
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    recipient = add_exact_alias(
        nest_instance["url"], user["signing_key"], mail_bridge_mta.domain,
        f"e2efull{uuid.uuid4().hex[:8]}",
    )
    nonce = f"full{uuid.uuid4().hex[:10]}qx"
    deliver_inbound(
        mail_bridge_mta.mx_port, mail_bridge_mta.domain, SENDER, recipient,
        plain_message(SENDER, recipient, nonce), time.monotonic() + 40.0,
    )
    # The message is the member's before the limit drops under it.
    app.conversations.navigate()
    wait_for_thread_with_nonce(
        app, nonce, what="the message that fills the mailbox", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )

    admin_key = nest_instance["admin"]["signing_key"]
    with _ws(nest_instance, admin_key) as admin:
        assert admin.call("fauna.bridges.put_imap_policy", {"storage_bytes_default": 1}).get("ok") is True
    try:
        with _ws(nest_instance, user["signing_key"]) as member:
            quotas = member.call("fauna.bridges.get_mail_quotas", {})
        assert quotas["storage_bytes_limit"] == 1 and quotas["storage_bytes_used"] > 1, (
            f"the lowered limit must sit under what the delivery uses: {quotas!r}"
        )

        # ── What the member sees: reopen mail-settings, the notice is there.
        app.mail_settings.navigate()
        notice = wait_until(
            lambda: app.mail_settings.over_quota_notice_text() or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"mail-settings never showed the over-quota notice with the mailbox "
                f"limit under its use ({quotas!r}).\n"
                f"  readout: {app.mail_settings.mailbox_quota_text()!r}\n"
                f"  page error: {app.error_text()!r}"
            ),
        )
        assert notice == S.mail_settings.over_quota_mailbox, notice
        expected = S.mail_settings.mailbox_quota(
            used=_byte_size(quotas["storage_bytes_used"]),
            limit=_byte_size(quotas["storage_bytes_limit"]),
            messages=str(quotas["message_count_used"]),
            message_limit=str(quotas["message_count_limit"]),
        )
        assert app.mail_settings.mailbox_quota_text() == expected, (
            f"the readout must state the used size against the lowered limit, as "
            f"the nest answers it: {app.mail_settings.mailbox_quota_text()!r} != {expected!r}"
        )
        # The daily sending allowance is its own readout, stated as the nest
        # answers it (recipients used today of the daily limit).
        expected_sending = S.mail_settings.submission_quota(
            used=str(quotas["submission_used_today"]),
            limit=str(quotas["submission_max_per_day"]),
        )
        assert app.mail_settings.submission_quota_text() == expected_sending, (
            f"the sending readout must state today's recipients against the daily "
            f"allowance, as the nest answers it: "
            f"{app.mail_settings.submission_quota_text()!r} != {expected_sending!r}"
        )
    finally:
        with _ws(nest_instance, admin_key) as admin:
            admin.call("fauna.bridges.put_imap_policy", {})
