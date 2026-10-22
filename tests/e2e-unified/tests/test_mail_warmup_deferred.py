"""tier_3: a sent mail the fresh-IP warm-up holds back says, on its own bubble in
the conversation, when it will be delivered and how many held messages are ahead
of it (`docs/goal/behavior/mail-deliverability.md` § Enforcement at submission
time, the deferred-state bullet; `dm-message-warmup-deferred`).

Production data flow asserted end-to-end on real binaries:

  another user's list send wider than the day-1 ramp (50) spends today's
  deployment-wide budget → the tui user composes a new conversation and clicks
  Send (`ConversationsManager::send_new_thread` → `SmtpBackend::send` →
  `fauna.email.send`) → the nest's warm-up check finds the day's ramp spent and
  holds the message for the next 00:00 UTC, filing its Sent copy → the app's
  receive loop reads `fauna.email.sent.fetch`, whose message carries
  `warmup_hold { release_at, messages_ahead }` → the shared ingest records the
  hold on the message the send echoed → `thread_detail` projects
  `MessageSnapshot.warmup_hold` → the app paints `dm-message-warmup-deferred`
  under the bubble in the shared wording (`warmup_deferred_text` natively over
  UniFFI / directly; on web the wasm `warmupDeferredTextAt` door over the hold
  the JS Sent drain handed `ingestSealedInbound`, the release time formatted by
  `localTimestamp` in the same `YYYY-MM-DD HH:MM` shape).

The painted line is cross-checked against the nest's own read of the same
message (convention 5): the release time and queue position must be the ones the
nest reports, so a client that invented either is red.
"""

import secrets
import time
from datetime import datetime

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.app_surface import skip_unbuilt
from helpers.budgets import RECEIVE_CYCLE_S
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

_MUA_PASSWORD = "warmup-deferred-plain-pw-1"
# Routed to the dedicated nest's stub external MX; nothing here needs it to
# arrive — a held message only sits in the queue.
_EXTERNAL_RECIPIENT = "recipient@external.test"
# Wider than the day-1 cap of 50, so the list send spends today's whole ramp
# (the part past it is held) and the user's own send after it is held too.
_RAMP_SPENDING_MEMBERS = 55


def _spend_todays_ramp(nest, domain: str) -> None:
    """Another user's list send past the day's ramp — the deployment-wide cap
    is the IP's, so it spends the budget for every sender. Setup, not the
    journey: the user's own act under test is the send through the app."""
    burner = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"]
    )
    with WsRpcAdminClient(
        nest["url"],
        actor_id=burner["actor_id_bytes"],
        signing_key=bytes(burner["signing_key"]),
    ) as client:
        # The list send files a Sent copy sealed to its sender.
        provision_recipient_seal_key(client, burner["actor_id_bytes"])
        local_part = "ramp-" + secrets.token_hex(4)
        list_id = client.call(
            "fauna.bridges.create_account_list",
            {"local_part": local_part, "local_domain": domain},
        )["list_id"]
        for _ in range(_RAMP_SPENDING_MEMBERS):
            client.call(
                "fauna.bridges.add_list_member",
                {
                    "list_id": list_id,
                    "recipient_address": f"m-{secrets.token_hex(4)}@external.test",
                },
            )
        message = (
            f"From: {local_part}@{domain}\r\nTo: {local_part}@{domain}\r\n"
            f"Subject: ramp\r\nMessage-ID: <{secrets.token_hex(8)}@{domain}>\r\n"
            "Date: Sat, 17 Oct 2026 12:00:00 +0000\r\n\r\nSpends the ramp.\r\n"
        ).encode()
        client.call("fauna.bridges.send_list_message", {"list_id": list_id, "message": message})


def _expected_line(hold: dict) -> str:
    """The shared wording (`fauna_conversations::message::warmup_deferred_text`)
    over the nest's own hold. The app renders the release in the local clock it
    shares with this process."""
    when = datetime.fromtimestamp(hold["release_at"]).strftime("%Y-%m-%d %H:%M")
    until = S.conversations.unified.dm_message_warmup_deferred(date=when)
    ahead = hold["messages_ahead"]
    if ahead == 0:
        queue = S.conversations.unified.dm_message_warmup_queue_next
    elif ahead == 1:
        queue = S.conversations.unified.dm_message_warmup_queue_one
    else:
        queue = S.conversations.unified.dm_message_warmup_queue(count=str(ahead))
    return f"{until} {queue}"


@pytest.mark.feature("email-in-conversations")
def test_a_sent_mail_the_warm_up_holds_says_when_it_will_be_delivered(
    app, dedicated_mail_nest, request
):
    """Send a mail past the day's ramp through the app, open its conversation,
    and read on its bubble when it will be delivered and how many are ahead —
    the very release time and position the nest's own Sent read reports."""
    if not (
        app.driver.is_tui()
        or app.driver.is_web()
        or app.driver.is_linux()
        or app.driver.is_android()
    ):
        skip_unbuilt(
            app.driver,
            surface="dm-message-warmup-deferred",
            detail="tui, web, linux and android paint it; windows, macOS and iOS "
            "follow in their own trickle-down rows",
            tracked="docs/goal/behavior/mail-deliverability.md § Enforcement at "
            "submission time",
        )

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain
    node_url = dedicated_node_url(app, handle, request)

    login_as_nest_admin(app, nest, node_url)
    alias_admin_to_address(nest, domain)
    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "mail must report enabled after the toggle; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    handle.rebind_after_enable()

    _spend_todays_ramp(nest, domain)

    nonce = f"warmupheld{int(time.time() * 1000)}qx"
    app.conversations.start_new_conversation(
        _EXTERNAL_RECIPIENT,
        subject=f"Held by the warm-up {nonce}",
        body=f"The {nonce} body goes out when the ramp allows.\n",
    )
    send_error = app.error_text()
    assert not send_error, f"the send was refused before it reached the nest: {send_error!r}"

    # The nest's own read of the user's Sent mailbox: the user sent exactly this
    # one message, and the warm-up holds it.
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(nest["admin"]["signing_key"].verify_key),
        signing_key=bytes(nest["admin"]["signing_key"]),
    ) as admin:
        sent = admin.call("fauna.email.sent.fetch", {"after_uid": 0, "limit": 50})["messages"]
    holds = [m["warmup_hold"] for m in sent if m.get("warmup_hold")]
    assert len(holds) == 1, (
        f"the user's one send past the spent ramp is held on the nest: {sent!r}"
    )
    hold = holds[0]
    assert hold["release_at"] > time.time(), f"held until a time still ahead: {hold!r}"

    thread = wait_until(
        lambda: next(
            (
                t
                for t in app.conversations.list_threads()
                if nonce in (t.label or "") or nonce in (t.snippet or "")
            ),
            None,
        ),
        RECEIVE_CYCLE_S,
        diagnose=lambda: f"threads: {[(t.label, t.snippet) for t in app.conversations.list_threads()]}",
    )
    app.conversations.open_thread_by_id(thread.thread_id)
    wait_until(
        lambda: app.driver.count("dm-message-warmup-deferred") > 0,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"bubble texts: {app.driver.get_texts('dm-message-text')!r}, "
            f"nest hold: {hold!r}, error: {app.error_text()!r}"
        ),
    )
    assert app.driver.get_texts("dm-message-warmup-deferred") == [_expected_line(hold)], (
        f"the line says the nest's release time and queue position: {hold!r}"
    )
