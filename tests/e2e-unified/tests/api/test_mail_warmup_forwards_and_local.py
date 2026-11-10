"""tier_3 e2e for who the fresh-IP warm-up counts
(``docs/goal/behavior/mail-deliverability.md`` § Enforcement at submission time,
ruled 2026-10-09): a forward leaving for an external address draws on the same
deployment-wide allowance as the user's own mail and, over it, is HELD for the
next 00:00 UTC — never dropped, never bounced — while mail to people on the
nest's own domain never leaves the deployment and so never counts.

Observed from outside, over the real MTA bridge and the real nest outbound
queue: the warm-up status the admin reads (``fauna.bridges.outbound_warmup_status``,
its ``today_used``), the held row in the nest's outbound queue
(``warmup_held_until``) and the in-process stub MX the fixture routes
``external.test`` to. Every wait is on a named state (convention 14). The
warm-up is deployment-wide on a session-shared nest, so each test resets it
first (a known day-1 budget of 50) and again in a ``finally``.
"""

import secrets
import sqlite3

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import _find_tagged, _poke_outbound_bridge
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.waiting import wait_until

from ..test_mail_bridge_filter_delivery import _placed_count, _provision_recipient
from ..test_mail_bridge_forward_floors import _inbound, _message, _set_forwarding
from ..test_mail_bridge_mta import _admin_ws, _create_admin_forwarder

pytestmark = pytest.mark.tier_3

OUTSIDE = "external.test"
DAY_ONE_CAP = 50


def _warmup(nest_instance, *, reset=False):
    with _admin_ws(nest_instance) as ws:
        kind = "outbound_warmup_reset" if reset else "outbound_warmup_status"
        return ws.call(f"fauna.bridges.{kind}", {})


def _forward_rows(db_path, destination, token):
    """``(warmup_held_until, is_forwarded)`` of every outbound row to
    ``destination`` whose message carries ``token``. The MTA hands a forward to
    the nest before it ACKs the DATA dot, so the rows are final once
    ``_inbound`` returns."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(
            "SELECT warmup_held_until, is_forwarded FROM outbound_mail_queue"
            " WHERE recipient = ? AND CAST(raw_message AS TEXT) LIKE ?",
            (destination, f"%{token}%"),
        ).fetchall()
    finally:
        conn.close()


def _parked(db_path, destination, token):
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM forward_queue WHERE destination_address = ?"
            " AND CAST(raw_message AS TEXT) LIKE ?",
            (destination, f"%{token}%"),
        ).fetchone()[0]
    finally:
        conn.close()


def _list_send(user_ws, domain, members):
    """A list send to ``members`` fresh external addresses — a warm-up-counted
    door, used to spend that much of the day's allowance."""
    local_part = "warmfill-" + secrets.token_hex(4)
    list_id = user_ws.call(
        "fauna.bridges.create_account_list",
        {"local_part": local_part, "local_domain": domain},
    )["list_id"]
    for _ in range(members):
        user_ws.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": f"m-{secrets.token_hex(4)}@{OUTSIDE}"},
        )
    message = (
        f"From: {local_part}@{domain}\r\nTo: {local_part}@{domain}\r\n"
        f"Subject: warm-up fill {secrets.token_hex(3)}\r\n"
        f"Message-ID: <{secrets.token_hex(8)}@{domain}>\r\n"
        "Date: Fri, 09 Oct 2026 12:00:00 +0000\r\n\r\nSpends the day's allowance.\r\n"
    ).encode()
    user_ws.call("fauna.bridges.send_list_message", {"list_id": list_id, "message": message})


def _app_send(user_ws, recipients, token):
    raw = (
        f"From: <sender-{token}@{OUTSIDE}>\r\nTo: <{recipients[0]}>\r\nSubject: warm-up {token}\r\n"
        f"Message-ID: <{token}@{OUTSIDE}>\r\nDate: Mon, 25 May 2026 12:00:00 +0000\r\n"
        "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n"
        f"\r\nA send for the warm-up count {token}.\r\n"
    ).encode()
    return user_ws.call("fauna.email.send", {"recipients": recipients, "raw_rfc5322": raw})


def _user_ws(nest_instance, user):
    return WsRpcAdminClient(nest_instance["url"], actor_id=user["actor_id_bytes"],
                            signing_key=bytes(user["signing_key"]))


@pytest.mark.feature("mail-server")
def test_forwarded_mail_counts_against_the_warmup_and_is_held_not_dropped_over_it(
        mail_bridge_mta, nest_instance):
    """With 49 of the day's 50 spent by a list send, the next forward spends the
    last unit and goes out; the one after, to the same destination, is accepted
    and held in the outbound queue until the next 00:00 UTC — it spends nothing
    more, is not sent, and is not dropped. An admin forwarder's forward is held
    the same way: it is the deployment's IP the cap protects, whichever door
    the mail left through."""
    mta = mail_bridge_mta
    db = nest_instance["db_path"]
    url = nest_instance["url"]
    recipient, _, addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    sender = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    tag = secrets.token_hex(4)
    target = f"warmfwd-{tag}@{OUTSIDE}"
    af_target = f"warmfwd-af-{tag}@{OUTSIDE}"
    af_local = f"warmfwd-af-{tag}"
    first, second, third = (f"warmfwd{n}-{tag}" for n in (1, 2, 3))
    envelope = "sender@orig.test"
    try:
        _warmup(nest_instance, reset=True)
        with _user_ws(nest_instance, sender) as ws:
            provision_recipient_seal_key(ws, sender["actor_id_bytes"])
            _list_send(ws, mta.domain, DAY_ONE_CAP - 1)
        assert _warmup(nest_instance)["today_used"] == DAY_ONE_CAP - 1, (
            "the list send must spend one unit of the allowance per recipient")

        _create_admin_forwarder(nest_instance, mta.domain, af_local, af_target)
        _set_forwarding(url, recipient, target)

        _inbound(mta, envelope, addr, _message(envelope, addr, first))
        assert _warmup(nest_instance)["today_used"] == DAY_ONE_CAP, (
            "a forward within the allowance must spend one unit of it, like the user's own mail")
        rows = _forward_rows(db, target, first)
        assert [r[1] for r in rows] == [1] and rows[0][0] is None, (
            f"a forward within the allowance is queued to leave now; got {rows!r}")

        _inbound(mta, envelope, addr, _message(envelope, addr, second))
        af_addr = f"{af_local}@{mta.domain}"
        _inbound(mta, envelope, af_addr, _message(envelope, af_addr, third))

        held = _forward_rows(db, target, second)
        assert len(held) == 1 and held[0][1] == 1 and held[0][0], (
            f"a forward over the allowance is accepted and held until the next 00:00 UTC; "
            f"got {held!r} (parked: {_parked(db, target, second)})")
        af_held = _forward_rows(db, af_target, third)
        assert len(af_held) == 1 and af_held[0][0], (
            f"an admin forwarder's forward over the allowance is held too; got {af_held!r}")
        assert held[0][0] == af_held[0][0], "both are held to the same next 00:00 UTC"
        assert _warmup(nest_instance)["today_used"] == DAY_ONE_CAP, (
            "a held forward must spend nothing more of the allowance")

        # The first forward going out proves a drain ran; the held ones, due
        # tomorrow, were not part of it.
        _poke_outbound_bridge(url)
        wait_until(lambda: _find_tagged(mta.stub_mx, first), MAIL_OUTBOUND_CYCLE_S,
                   diagnose=lambda: "the forward within the allowance never went out")
        assert _find_tagged(mta.stub_mx, second) is None, "a held forward must not go out"
        assert _find_tagged(mta.stub_mx, third) is None, "a held forward must not go out"
        assert len(_forward_rows(db, target, second)) == 1, "a held forward is never dropped"
        assert len(_forward_rows(db, af_target, third)) == 1, "a held forward is never dropped"
    finally:
        _set_forwarding(url, recipient, None)
        _warmup(nest_instance, reset=True)


@pytest.mark.feature("mail-server")
def test_mail_to_people_on_your_own_nest_never_counts_against_the_warmup(
        mail_bridge_mta, nest_instance):
    """A send to a member of the nest's own domain is delivered to their mailbox
    and spends nothing of the allowance; in a send to one local and one outside
    recipient only the outside one is counted, which the outside-only send
    beside it shows is a unit that does count."""
    mta = mail_bridge_mta
    url = nest_instance["url"]
    db = nest_instance["db_path"]
    _, member_id, member_addr = _provision_recipient(
        nest_instance["port"], url, nest_instance["admin"], mta.domain)
    sender = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    tag = secrets.token_hex(4)
    try:
        _warmup(nest_instance, reset=True)
        with _user_ws(nest_instance, sender) as ws:
            provision_recipient_seal_key(ws, sender["actor_id_bytes"])
            assert _warmup(nest_instance)["today_used"] == 0
            before = _placed_count(db, member_id)

            _app_send(ws, [member_addr], f"local-{tag}")
            assert _placed_count(db, member_id) == before + 1, (
                "the send to a member of the nest's own domain must reach their mailbox")
            assert _warmup(nest_instance)["today_used"] == 0, (
                "mail to a member of your own nest must not count against the warm-up")

            _app_send(ws, [member_addr, f"mixed-{tag}@{OUTSIDE}"], f"mixed-{tag}")
            assert _placed_count(db, member_id) == before + 2
            assert _warmup(nest_instance)["today_used"] == 1, (
                "only the recipient outside the nest counts in a mixed send")

            _app_send(ws, [f"outside-{tag}@{OUTSIDE}"], f"outside-{tag}")
            assert _warmup(nest_instance)["today_used"] == 2, (
                "control: a send to an outside recipient does count")
    finally:
        _warmup(nest_instance, reset=True)
