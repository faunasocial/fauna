"""tier_3 witnesses for what an admin's mail-policy save DOES to mail.

`test_mail_admin_policy.py` proves a save lands; this file proves the saved
value then changes what happens to a message or a connection, observed from
outside as an SMTP peer would see it (`docs/features/admin-mail-policy.md`,
the `[nest]` outcomes; `docs/goal/behavior/mail-policy-config.md`).

Every test runs against the session's `mail_bridge_mta` (a real nest and a real
Go MTA on loopback ports), or for the mail-app knobs its `mail_bridge_mda`. A
spam-group save reaches the running bridge by the
`config_changed` push and applies at the next connection, so each test POLLS
the observable verdict until the new value shows, never sleeps for it; and
because `put_spam_policy` replaces the whole group, each test that changes it
puts back `MTA_FIXTURE_SPAM_POSTURE` — the posture every other mail test on
this nest relies on — in a `finally`.
"""

import base64
import contextlib
import secrets
import socket
import time

import pytest

from conftest import MTA_FIXTURE_SPAM_POSTURE
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _connect_submission_tls,
    _find_tagged,
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imaps_connect,
    _poke_outbound_bridge,
    _read_final_reply,
    _SmtpConn,
    _wait_for_tagged,
    await_outbound_terminal,
    outbound_clock,
    outbound_clock_now,
    outbound_msgid_key,
    outbound_queue,
    outbound_attempt_at as _attempt_at,
    outbound_clock_on_real_time as _outbound_clock_on_real_time,
    outbound_row as _row,
    await_outbound_row as _await_row,
    restore_outbound_clock,
    submit_one_message,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

# How long a saved value may take to reach the running bridge (push → re-fetch
# → swap at the next connection). A budget for the poll, not a sleep.
_APPLY_BUDGET_S = 60.0


def _admin_ws(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@contextlib.contextmanager
def _spam_policy(nest, **overrides):
    """Put the fixture posture plus `overrides`; put the bare posture back after."""
    with _admin_ws(nest) as ws:
        ws.call("fauna.bridges.put_spam_policy", {**MTA_FIXTURE_SPAM_POSTURE, **overrides})
    try:
        yield
    finally:
        with _admin_ws(nest) as ws:
            ws.call("fauna.bridges.put_spam_policy", dict(MTA_FIXTURE_SPAM_POSTURE))


def _poll(probe, want, what: str):
    """Run `probe()` until `want(result)` holds or the apply budget runs out."""
    deadline = time.monotonic() + _APPLY_BUDGET_S
    seen = []
    while True:
        got = probe()
        if want(got):
            return got
        seen.append(got)
        if time.monotonic() > deadline:
            raise AssertionError(f"{what}: never observed within {_APPLY_BUDGET_S}s; saw {seen[-5:]!r}")
        time.sleep(0.5)


def _message(to_addr: str, filler_bytes: int = 0) -> bytes:
    lines = [
        "From: External Sender <sender@external.test>",
        f"To: {to_addr}",
        f"Subject: policy effect {secrets.token_hex(4)}",
        f"Message-ID: <pe-{secrets.token_hex(8)}@external.test>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "A message for the mail-policy effect witnesses.",
    ]
    lines += ["x" * 76] * (filler_bytes // 78)
    return ("\r\n".join(lines) + "\r\n").encode()


def _inbound_final_reply(handle, rcpt: str, body: bytes, mail_from: str | None = None) -> str:
    """One inbound transaction on port 25; the final reply to its DATA (or the
    first refusal before it)."""
    mail_from = mail_from or f"sender-{secrets.token_hex(4)}@external.test"
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        for command in (f"MAIL FROM:<{mail_from}>", f"RCPT TO:<{rcpt}>", "DATA"):
            conn.send_raw((command + "\r\n").encode())
            reply = _read_final_reply(conn, deadline)
            if not reply.startswith(("250", "354")):
                return reply
        conn.send_raw(body)
        conn.send_raw(b".\r\n")
        return _read_final_reply(conn, deadline)


def _recipient(handle) -> str:
    return f"{handle.recipient_local_part}@{handle.domain}"


@pytest.mark.feature("admin-mail-policy")
def test_admin_size_cap_refuses_larger_mail_inbound_and_from_a_mail_app(mail_bridge_mta, nest_instance):
    """Mail larger than the size the admin sets is refused as too large
    (`552 5.3.4`), arriving from outside and sent from a mail app alike, while
    mail under it is still accepted; and a cap above 250 MB cannot be saved —
    the refused save leaves the admin's cap as it was."""
    from clients.ws_rpc_admin_client import RpcCallError

    handle = mail_bridge_mta
    rcpt = _recipient(handle)
    cap = 16_384
    with _spam_policy(nest_instance, max_message_bytes=cap):
        reply = _poll(
            lambda: _inbound_final_reply(handle, rcpt, _message(rcpt, filler_bytes=4 * cap)),
            lambda r: r.startswith("552"),
            "an inbound message over the admin's size cap refused 552",
        )
        assert "5.3.4" in reply, reply
        small = _inbound_final_reply(handle, rcpt, _message(rcpt))
        assert small.startswith("250"), f"a message under the cap must still deliver: {small!r}"

        sender = f"{handle.submission_sender_local}@{handle.domain}"
        auth = base64.b64encode(
            b"\x00" + sender.encode() + b"\x00" + handle.submission_credential
        ).decode()
        deadline = time.monotonic() + 30.0
        with _connect_submission_tls(handle.submission_port_465, handle.domain) as conn:
            conn.expect("220", deadline)
            conn.cmd(f"EHLO {handle.domain}", "250", deadline)
            conn.cmd(f"AUTH PLAIN {auth}", "235", deadline)
            conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
            conn.cmd("RCPT TO:<recipient@external.test>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(_message("recipient@external.test", filler_bytes=4 * cap))
            conn.send_raw(b".\r\n")
            sub_reply = _read_final_reply(conn, deadline)
        assert sub_reply.startswith("552") and "5.3.4" in sub_reply, (
            f"a mail-app submission over the cap must be refused 552 5.3.4: {sub_reply!r}"
        )

        with _admin_ws(nest_instance) as ws:
            with pytest.raises(RpcCallError):
                ws.call(
                    "fauna.bridges.put_spam_policy",
                    {**MTA_FIXTURE_SPAM_POSTURE, "max_message_bytes": 250_000_001},
                )
            spam = ws.call("fauna.bridges.get_mail_config", {})["spam"]
        assert spam["max_message_bytes"] == cap, f"a refused cap must change nothing: {spam!r}"


def _greeting(port: int) -> str:
    """Open one connection to port 25 and return the reply to its EHLO — `250`
    admitted, `421` turned away by the connection-rate limit. (The session, and
    with it the limit, opens at the first command, not at the `220` banner.)"""
    deadline = time.monotonic() + 15.0
    raw = socket.create_connection(("127.0.0.1", port), timeout=15.0)
    with _SmtpConn(raw) as conn:
        try:
            banner = _read_final_reply(conn, deadline)
            if not banner.startswith("220"):
                return banner
            conn.send_raw(b"EHLO external.test\r\n")
            return _read_final_reply(conn, deadline)
        except AssertionError as e:  # closed before a full line
            return f"closed: {e}"


@pytest.mark.feature("admin-mail-policy")
def test_connection_rate_limit_turns_fast_server_away_and_zero_admits_all(mail_bridge_mta, nest_instance):
    """A server opening connections faster than the admin's per-minute limit is
    turned away (`421`), and a limit of zero admits every connection."""
    port = mail_bridge_mta.mx_port
    with _spam_policy(nest_instance, max_conn_per_min=2):
        reply = _poll(
            lambda: _greeting(port),
            lambda r: r.startswith("421"),
            "a connection over the admin's 2/min limit turned away 421",
        )
        assert "4.7.0" in reply or "rate" in reply.lower(), reply

        with _admin_ws(nest_instance) as ws:
            ws.call(
                "fauna.bridges.put_spam_policy",
                {**MTA_FIXTURE_SPAM_POSTURE, "max_conn_per_min": 0},
            )
        # Zero has applied once 20 connections in a row are all admitted —
        # ten times the old limit inside one minute.
        streak = {"n": 0}

        def _admitted_streak():
            streak["n"] = streak["n"] + 1 if _greeting(port).startswith("250") else 0
            return streak["n"]

        _poll(_admitted_streak, lambda n: n >= 20, "limit 0 admitting 20 connections in a row")


@pytest.mark.feature("admin-mail-policy")
def test_greylisting_off_accepts_a_first_time_sender_on_its_first_try(mail_bridge_mta, nest_instance):
    """With greylisting off, a sender never seen before is accepted on its very
    first attempt — no `451` deferral to retry through (the deferral with it
    on is `test_mail_bridge_mta.py::test_inbound_greylist_defers_first_then_passes_on_retry`)."""
    handle = mail_bridge_mta
    rcpt = _recipient(handle)
    with _spam_policy(nest_instance, greylist_enabled=False, greylist_delay_secs=300):
        first_time_sender = f"never-seen-{secrets.token_hex(6)}@external.test"
        reply = _inbound_final_reply(handle, rcpt, _message(rcpt), mail_from=first_time_sender)
    assert reply.startswith("250"), f"greylisting off must accept a first try: {reply!r}"


@pytest.mark.feature("admin-mail-policy")
def test_plus_addressing_off_stops_tagged_mail_reaching_the_member(mail_bridge_mta, nest_instance):
    """With plus-addressing on (the default) `member+tag@` reaches the member;
    the admin switching it off makes the same address unknown (`550`)."""
    handle = mail_bridge_mta
    tagged = f"{handle.recipient_local_part}+news{secrets.token_hex(2)}@{handle.domain}"

    def _rcpt_code() -> str:
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<sender-{secrets.token_hex(3)}@external.test>", "250", deadline)
            conn.send_raw(f"RCPT TO:<{tagged}>\r\n".encode())
            return _read_final_reply(conn, deadline)

    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_alias_policy", {})  # the defaults: plus-addressing on
    assert _rcpt_code().startswith("250"), "plus-addressing on: the tagged address must resolve"
    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_alias_policy", {"subaddressing_enabled": False})
    try:
        reply = _poll(_rcpt_code, lambda r: not r.startswith("250"), "plus-addressing off refusing member+tag@")
        assert reply.startswith("550"), f"plus-addressing off must refuse member+tag@ as unknown: {reply!r}"
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_alias_policy", {})


@pytest.mark.feature("admin-mail-policy")
def test_wildcard_prefixes_off_stops_mail_under_the_prefix_reaching_the_member(mail_bridge_mta, nest_instance):
    """With wildcard prefixes on (the default) an address under a member's
    wildcard prefix reaches them; the admin switching them off makes the same
    address unknown (`550`). The on-state twin is
    `test_mail_bridge_alias_delivery.py::test_wildcard_prefix_address_reaches_the_owner`."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register
    from helpers.mail_aliases import add_exact_alias
    from helpers.recipient_seal_key import provision_recipient_seal_key

    handle = mail_bridge_mta
    member = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    with _admin_ws(nest_instance) as admin:
        add_exact_alias(nest_instance["url"], member["signing_key"], handle.domain,
                        f"wcm{secrets.token_hex(4)}")
        provision_recipient_seal_key(admin, member["actor_id_bytes"])
    prefix = f"wc{secrets.token_hex(3)}-"
    with WsRpcAdminClient(nest_instance["url"], actor_id=member["actor_id_bytes"],
                          signing_key=bytes(member["signing_key"])) as ws:
        ws.call("fauna.bridges.create_account_alias", {
            "kind": "wildcard_prefix", "local_domain": handle.domain, "pattern": prefix,
            "controls": {"label": ""},
        })
    under_prefix = f"{prefix}some-shop@{handle.domain}"

    def _rcpt_code() -> str:
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<sender-{secrets.token_hex(3)}@external.test>", "250", deadline)
            conn.send_raw(f"RCPT TO:<{under_prefix}>\r\n".encode())
            return _read_final_reply(conn, deadline)

    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_alias_policy", {})  # the defaults: wildcard prefixes on
    assert _rcpt_code().startswith("250"), "wildcard prefixes on: the address must resolve"
    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_alias_policy", {"wildcard_prefix_enabled": False})
    try:
        reply = _poll(_rcpt_code, lambda r: not r.startswith("250"),
                      "wildcard prefixes off refusing an address under the prefix")
        assert reply.startswith("550"), (
            f"wildcard prefixes off must refuse an address under the prefix as unknown: {reply!r}")
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_alias_policy", {})


# ── Spam score thresholds ──────────────────────────────────────────────
#
# The fixture's fake rspamd scores a message by its `X-Test-Rspamd-Score`
# header (standing in for content the shared rules would score that high), on
# rspamd's native scale; the bridge scales it by the catalog's 0.5 onto the
# 0–15 combined score the admin's thresholds are written in. So a header of 16
# is a combined 8.


def _scored(to_addr: str, rspamd_score: float) -> bytes:
    raw = _message(to_addr).decode()
    head, _, body = raw.partition("\r\n\r\n")
    return f"{head}\r\nX-Test-Rspamd-Score: {rspamd_score}\r\n\r\n{body}".encode()


def _mailbox_counts(db_path, actor_id) -> tuple[int, int]:
    """`(inbox, junk)` copies the nest holds for `actor_id` — read as deltas,
    since the session's recipient accumulates every test's mail."""
    import sqlite3

    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        counts = dict(
            conn.execute(
                "SELECT mailbox, COUNT(*) FROM bridge_imap_messages "
                "WHERE actor_id = ?1 AND mailbox IN ('INBOX', 'Junk') GROUP BY mailbox",
                (actor_id,),
            ).fetchall()
        )
        return int(counts.get("INBOX", 0)), int(counts.get("Junk", 0))
    finally:
        conn.close()


def _deliver_scored(handle, nest, rspamd_score: float) -> str:
    """Send one message scored `rspamd_score`; return where it went:
    `INBOX`, `Junk`, or the refusal line. An accepted message that lands in
    neither folder within the budget is a failure — the nest holds nothing back."""
    rcpt = _recipient(handle)
    actor_id = handle.recipient_actor["actor_id_bytes"]
    before = _mailbox_counts(nest["db_path"], actor_id)
    reply = _inbound_final_reply(handle, rcpt, _scored(rcpt, rspamd_score))
    if not reply.startswith("250"):
        return reply
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        inbox, junk = _mailbox_counts(nest["db_path"], actor_id)
        if junk > before[1]:
            return "Junk"
        if inbox > before[0]:
            return "INBOX"
        time.sleep(0.25)
    raise AssertionError(
        f"accepted (score {rspamd_score}) but in neither INBOX nor Junk after 30 s — held back"
    )


@pytest.mark.feature("admin-mail-policy")
def test_junk_threshold_moves_scored_mail_and_reject_threshold_refuses_it(mail_bridge_mta, nest_instance):
    """One message scoring a combined 8 goes to Junk under the default junk
    threshold of 5; raising the junk threshold to 12 delivers the same score to
    the inbox; and once the admin sets a reject threshold of 7 it is refused to
    the sending server (`550 5.7.1`) instead of delivered."""
    handle = mail_bridge_mta
    with _spam_policy(nest_instance, max_score_before_spam_folder=5, max_score_before_reject=0):
        _poll(lambda: _deliver_scored(handle, nest_instance, 16), lambda r: r == "Junk",
              "a combined 8 filed to Junk under junk threshold 5")
    with _spam_policy(nest_instance, max_score_before_spam_folder=12, max_score_before_reject=0):
        _poll(lambda: _deliver_scored(handle, nest_instance, 16), lambda r: r == "INBOX",
              "a combined 8 delivered to the inbox under junk threshold 12")
    with _spam_policy(nest_instance, max_score_before_spam_folder=5, max_score_before_reject=7):
        reply = _poll(lambda: _deliver_scored(handle, nest_instance, 16),
                      lambda r: r not in ("INBOX", "Junk"),
                      "a combined 8 refused above reject threshold 7")
    assert reply.startswith("550") and "5.7.1" in reply, f"the score refusal is 550 5.7.1: {reply!r}"


@pytest.mark.feature("admin-mail-policy")
def test_no_mail_is_held_back_on_its_spam_score(mail_bridge_mta, nest_instance):
    """Whatever the admin's thresholds, a scored message is delivered, filed to
    Junk, or refused — never held back: with junk at 5 and reject at 10, scores
    below, between and above both each end in exactly one of the three, and
    every accepted one is in the member's mailbox."""
    handle = mail_bridge_mta
    with _spam_policy(nest_instance, max_score_before_spam_folder=5, max_score_before_reject=10):
        # Wait for the thresholds to apply (the top score is refused).
        _poll(lambda: _deliver_scored(handle, nest_instance, 24), lambda r: r.startswith("550"),
              "reject threshold 10 applied")
        outcomes = {score: _deliver_scored(handle, nest_instance, score) for score in (2, 9, 12, 19, 24)}
    assert outcomes[2] == "INBOX" and outcomes[9] == "INBOX", outcomes
    assert outcomes[12] == "Junk" and outcomes[19] == "Junk", outcomes
    assert outcomes[24].startswith("550"), outcomes


# ── Allowances set to zero ─────────────────────────────────────────────


def _submission_rcpt_reply(handle, rcpt: str) -> str:
    """The submission sender's RCPT reply for one outside recipient."""
    sender = f"{handle.submission_sender_local}@{handle.domain}"
    auth = base64.b64encode(b"\x00" + sender.encode() + b"\x00" + handle.submission_credential).decode()
    deadline = time.monotonic() + 30.0
    with _connect_submission_tls(handle.submission_port_465, handle.domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {handle.domain}", "250", deadline)
        conn.cmd(f"AUTH PLAIN {auth}", "235", deadline)
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.send_raw(f"RCPT TO:<{rcpt}>\r\n".encode())
        return _read_final_reply(conn, deadline)


@pytest.mark.feature("admin-mail-policy")
def test_allowances_set_to_zero_are_accepted_and_members_get_none(mail_bridge_mta, nest_instance):
    """An allowance of zero is a legitimate admin choice, not a refused save:
    with daily sends at 0 a mail app's send is refused (`452 4.7.12`), with the
    mailbox size at 0 mail to a member is refused as mailbox full (`552 5.2.2`),
    and with extra addresses at 0 a member cannot add one."""
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
    from common.auth import create_actor_and_register

    handle = mail_bridge_mta
    rcpt = _recipient(handle)

    with _admin_ws(nest_instance) as ws:
        assert ws.call("fauna.bridges.put_submission_policy", {"max_per_day": 0}).get("ok") is True
    try:
        reply = _submission_rcpt_reply(handle, f"zero-{secrets.token_hex(3)}@external.test")
        assert reply.startswith("452") and "4.7.12" in reply, f"no daily sends left: {reply!r}"
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_submission_policy", {})

    with _admin_ws(nest_instance) as ws:
        assert ws.call("fauna.bridges.put_imap_policy", {"storage_bytes_default": 0}).get("ok") is True
    try:
        reply = _inbound_final_reply(handle, rcpt, _message(rcpt))
        assert reply.startswith("552") and "5.2.2" in reply, f"a 0-byte mailbox takes nothing: {reply!r}"
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_imap_policy", {})

    member = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    with _admin_ws(nest_instance) as ws:
        assert ws.call("fauna.bridges.put_alias_policy", {"exact_aliases_max": 0}).get("ok") is True
    try:
        with WsRpcAdminClient(nest_instance["url"], actor_id=member["actor_id_bytes"],
                              signing_key=bytes(member["signing_key"])) as client:
            with pytest.raises(RpcCallError) as excinfo:
                client.call("fauna.bridges.create_account_alias", {
                    "kind": "exact",
                    "local_domain": handle.domain,
                    "pattern": f"zero{secrets.token_hex(3)}",
                    "controls": {"label": ""},
                })
        assert excinfo.value.code == "fauna.bridges.alias_cap_exceeded", excinfo.value.code
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_alias_policy", {})


# ── Outbound delivery ──────────────────────────────────────────────────
#
# The retry schedule, the give-up time, the delay-warning time and the
# bounce-repeat window are read nest-side from the stored policy each time a
# delivery fails, so a save applies at the next failure with no push to wait
# on. Their boundaries are hours and days apart: `POST
# /api/v1/test/outbound/clock` moves the outbound clock every queue time is
# read on, and each witness first puts it on the real clock (so the rows it
# creates are timed from now, whatever a sibling left frozen) and hands it back
# as it found it. Every message a witness sends is driven to a terminal state
# before it returns — a pending row left behind would bounce into a sibling's
# window. The temporary-codes list and the IPv6 switch are the bridge's (it owns
# the delivery attempt), so those two reach it by the `config_changed` push and
# are polled for, like the spam group above.


@contextlib.contextmanager
def _outbound_policy(nest, **overrides):
    """Save `overrides` as the outbound group; put the defaults back after
    (`put_outbound_policy` replaces the whole group, and no fixture sets it)."""
    with _admin_ws(nest) as ws:
        assert ws.call("fauna.bridges.put_outbound_policy", overrides).get("ok") is True
    try:
        yield
    finally:
        with _admin_ws(nest) as ws:
            ws.call("fauna.bridges.put_outbound_policy", {})


def _delay_warnings_for(nest_url, row_id: int) -> int:
    """Delay-warning DSNs the nest has enqueued for queue row `row_id` (minted
    `<delay-<now>-<row id>@…>`)."""
    return sum(
        1 for r in outbound_queue(nest_url)
        if r["original_msgid"].startswith("<delay-") and f"-{row_id}@" in r["original_msgid"]
    )


# Every send authenticates the fixture's one sender, and the nest lets a
# bridge fetch that credential 30 times a minute — so a witness waiting for a
# pushed knob sends a handful of messages, spaced, never one per poll tick: a
# spent budget fails every sibling's submission for the next minute.
_PUSH_SENDS = 5
_PUSH_SEND_SPACING_S = 2.0


def _send_until_applied(send_one, applied, what: str):
    """Call `send_one()` until `applied(result)` holds, at most `_PUSH_SENDS` times."""
    seen = []
    for _ in range(_PUSH_SENDS):
        got = send_one()
        if applied(got):
            return got
        seen.append(got)
        time.sleep(_PUSH_SEND_SPACING_S)  # sleep-ok: paces sends under the nest's real-clock 30-a-minute token-fetch limit
    raise AssertionError(f"{what}: not applied after {_PUSH_SENDS} sends; saw {seen!r}")


def _first_attempt(handle, rcpt: str, token: str) -> dict:
    """Send one message tagged `token` to `rcpt`; its row once tried once."""
    submit_one_message(handle, rcpt, token)
    return _await_row(
        handle.nest_url, token, rcpt,
        lambda r: r["attempt_count"] >= 1 or r["status"] != "pending",
        "the first attempt",
    )


@pytest.mark.feature("admin-mail-policy")
def test_admin_retry_schedule_warning_and_give_up_times_decide_a_failing_message(mail_bridge_mta, nest_instance):
    """A message whose recipient's server keeps saying "try later" is retried
    on the admin's schedule, its sender is warned at the admin's delay-warning
    time and gets it back at the admin's give-up time — not the defaults
    (5 min first retry, a warning at 4 h, given up after 5 days).

    The admin sets a 10-minute retry step, a warning at 1 h and a give-up at 2 h.
    The stub MX answers `451` for every `tempfail*@external.test` recipient."""
    handle = mail_bridge_mta
    nest_url = handle.nest_url
    token = f"retry-{secrets.token_hex(6)}"
    rcpt = f"tempfail-{token}@external.test"
    with _outbound_clock_on_real_time(nest_url), _outbound_policy(
        nest_instance,
        retry_schedule_seconds=[0] + [600] * 9,
        delay_warning_at_hours=1,
        permanent_failure_timeout_hours=2,
    ):
        row = _first_attempt(handle, rcpt, token)
        assert row["status"] == "pending", f"a 451 is retried: {row!r}"
        created = row["created_at"]
        # The schedule: the next try is the admin's 10 min out (±10 % jitter,
        # plus the seconds the first attempt took), not the default 5 min.
        step = row["next_attempt_at"] - created
        assert 535 <= step <= 680, f"next try {step}s after the message arrived; the admin set 600: {row!r}"

        # 30 min in: retried, still before the admin's 1 h — no warning yet.
        row = _attempt_at(nest_url, token, rcpt, created + 1800)
        assert row["status"] == "pending", row
        assert _delay_warnings_for(nest_url, row["id"]) == 0, "warned before the admin's 1 h"

        # Past 1 h (the default waits for 4 h): the sender is warned, once.
        row = _attempt_at(nest_url, token, rcpt, created + 3600 + 120)
        assert row["status"] == "pending", row
        assert _delay_warnings_for(nest_url, row["id"]) == 1, (
            "no delay warning enqueued after the admin's 1 h delay-warning time"
        )

        # Past 2 h (the default gives up after 5 days): given up and bounced
        # back to the sender.
        row = _attempt_at(nest_url, token, rcpt, created + 2 * 3600 + 120)
        assert row["status"] == "bounced", f"not given up at the admin's 2 h: {row!r}"


@pytest.mark.feature("admin-mail-policy")
def test_admin_bounce_repeat_window_decides_when_the_same_message_bounces_again(mail_bridge_mta, nest_instance):
    """The admin's bounce-repeat window decides whether a second failure of the
    same message bounces back to its sender again: with a 1-day window, a repeat
    two days later bounces (the 7-day default would hold it back) and a repeat
    half a day after that does not.

    The same Message-ID is sent three times, each to a recipient the stub MX
    refuses with `550` (`nonexistent*@external.test`)."""
    handle = mail_bridge_mta
    nest_url = handle.nest_url
    token = f"ndrwin-{secrets.token_hex(6)}"
    msgid = f"<{token}@{handle.domain}>"
    with _outbound_clock_on_real_time(nest_url), _outbound_policy(nest_instance, ndr_rate_limit_days=1):
        start = outbound_clock(nest_url, advance=0)

        def _bounce_at(at: int, label: str) -> str:
            outbound_clock(nest_url, set=at)
            rcpt = f"nonexistent-{label}-{token}@external.test"
            submit_one_message(handle, rcpt, token)
            return await_outbound_terminal(nest_url, msgid, rcpt)["status"]

        assert _bounce_at(start, "first") == "bounced"
        assert _bounce_at(start + 2 * 86_400, "after-window") == "bounced", (
            "a repeat two days on is outside the admin's 1-day window and must bounce again"
        )
        assert _bounce_at(start + 2 * 86_400 + 43_200, "inside-window") == "suppressed_rate", (
            "a repeat half a day after the last bounce is inside the 1-day window"
        )


@pytest.mark.feature("admin-mail-policy")
def test_codes_the_admin_lists_as_temporary_are_retried_not_bounced(mail_bridge_mta, nest_instance):
    """A `550 5.1.1` normally bounces at once; once the admin lists `5.1.1` as
    temporary the same refusal is retried instead, and with the list cleared
    again the waiting message bounces on its next try."""
    handle = mail_bridge_mta
    nest_url = handle.nest_url
    with _outbound_clock_on_real_time(nest_url):
        with _outbound_policy(nest_instance, treat_5xx_as_transient=["5.1.1"]):
            # The list reaches the bridge by push; until it applies a send
            # still bounces, so send fresh messages until one is retried.
            def _send_one():
                token = f"t5xx-{secrets.token_hex(6)}"
                rcpt = f"nonexistent-{token}@external.test"
                return token, rcpt, _first_attempt(handle, rcpt, token)

            token, rcpt, row = _send_until_applied(
                _send_one, lambda got: got[2]["status"] == "pending",
                "a 550 5.1.1 retried once the admin lists 5.1.1 as temporary",
            )
            assert "5.1.1" in (row["last_error"] or ""), row
            assert row["next_attempt_at"] > row["created_at"], f"a retry is scheduled: {row!r}"

        # Cleared: the same refusal is permanent again and the message bounces.
        at = {"t": row["next_attempt_at"]}

        def _next_try():
            at["t"] += 3600
            return _attempt_at(nest_url, token, rcpt, at["t"])

        row = _poll(_next_try, lambda r: r["status"] != "pending",
                    "the waiting message bouncing once the list is cleared")
        assert row["status"] == "bounced", row


@pytest.mark.feature("admin-mail-policy")
def test_ipv6_off_delivers_over_ipv4_only(mail_bridge_mta, nest_instance):
    """With IPv6 off, mail still goes out over IPv4 but a server reachable only
    over IPv6 is never dialled: the message waits; switched back on, the same
    message is delivered there.

    `v6only.test` is routed at a stub MX listening on `::1` alone; `external.test`
    at one on `127.0.0.1`."""
    handle = mail_bridge_mta
    if handle.v6_stub_mx is None:
        pytest.skip("no IPv6 loopback on this box: the IPv6-only stub MX could not bind ::1")
    nest_url = handle.nest_url
    with _outbound_clock_on_real_time(nest_url):
        with _outbound_policy(nest_instance, ipv6_enabled=False):
            # The switch reaches the bridge by push; until it applies a send to
            # the IPv6-only server is still delivered, so send fresh ones until
            # one is held back.
            def _send_one():
                token = f"v6off-{secrets.token_hex(6)}"
                rcpt = f"someone-{token}@{handle.v6_domain}"
                return token, rcpt, _first_attempt(handle, rcpt, token)

            token, rcpt, row = _send_until_applied(
                _send_one, lambda got: got[2]["status"] == "pending",
                "a message to the IPv6-only server held back with IPv6 off",
            )
            assert "tcp4" in (row["last_error"] or ""), f"dialled over IPv4 only: {row!r}"
            assert _find_tagged(handle.v6_stub_mx, token) is None, "delivered over IPv6 with IPv6 off"

            # IPv4 delivery is untouched by the switch.
            v4_token = f"v4on-{secrets.token_hex(6)}"
            submit_one_message(handle, f"someone-{v4_token}@external.test", v4_token)
            assert _wait_for_tagged(handle.stub_mx, v4_token, MAIL_OUTBOUND_CYCLE_S) is not None, (
                "with IPv6 off, mail to a server on IPv4 must still be delivered"
            )

        # Back on: the waiting message reaches the IPv6-only server.
        at = {"t": row["next_attempt_at"]}

        def _next_try():
            delivered = _find_tagged(handle.v6_stub_mx, token)
            if delivered is None:
                at["t"] += 3600
                _attempt_at(nest_url, token, rcpt, at["t"])
                delivered = _find_tagged(handle.v6_stub_mx, token)
            return delivered

        _poll(_next_try, lambda raw: raw is not None,
              "the held message delivered over IPv6 once switched back on")


# ── Mail-app knobs (the MDA's IMAP surface) ────────────────────────────


def _imap_message(to_addr: str) -> bytes:
    return (
        "From: External Sender <sender@external.test>\r\n"
        f"To: {to_addr}\r\n"
        f"Subject: kept {secrets.token_hex(4)}\r\n"
        f"Message-ID: <pe-{secrets.token_hex(8)}@external.test>\r\n"
        "Date: Sun, 24 May 2026 12:00:00 +0000\r\n"
        "\r\n"
        "A message that keeps its folder non-empty.\r\n"
    ).encode()


@pytest.mark.feature("admin-mail-policy")
def test_delete_nonempty_allowed_lets_a_mail_app_delete_a_folder_still_holding_mail(
        mail_bridge_mda, nest_instance, seal_helper_binary):
    """Under the default a mail app's `DELETE` of a folder that still holds mail
    is refused (`NO`) and the folder stays; once the admin allows it, the same
    `DELETE` succeeds and the folder is gone. The nest reads the admin's choice
    at each `DELETE`, so the second command sees it with no wait."""
    from conftest import _provision_msek_recipient

    mda = mail_bridge_mda
    local = f"delne{secrets.token_hex(3)}"
    r = _provision_msek_recipient(nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
                                  domain=mda.domain, local_part=local, password=f"{local}-password-1")
    folder = f"Kept-{secrets.token_hex(3)}"
    deadline = time.monotonic() + 90.0
    tags = iter(f"t{i}" for i in range(1, 100))

    with _admin_ws(nest_instance) as ws:
        ws.call("fauna.bridges.put_imap_policy", {})  # the default: forbidden
    sock, buf = _imaps_connect(mda, deadline)
    try:
        assert _imap_auth_plain(sock, buf, next(tags), r.username, r.password, deadline) == "OK"
        status, text = _imap_cmd(sock, buf, next(tags), f'CREATE "{folder}"', deadline)
        assert status == "OK", f"CREATE must succeed: {text}"
        status, _ = _imap_append(sock, buf, next(tags), f'"{folder}"', _imap_message(r.username), deadline)
        assert status == "OK", "APPEND into the new folder must succeed"

        status, text = _imap_cmd(sock, buf, next(tags), f'DELETE "{folder}"', deadline)
        assert status == "NO", f"by default a folder still holding mail is not deleted; got {status}: {text}"

        with _admin_ws(nest_instance) as ws:
            assert ws.call("fauna.bridges.put_imap_policy", {"delete_nonempty": "allowed"}).get("ok") is True
        try:
            status, text = _imap_cmd(sock, buf, next(tags), f'DELETE "{folder}"', deadline)
            assert status == "OK", f"once allowed, the non-empty folder must be deleted; got {status}: {text}"
            _, listing = _imap_cmd(sock, buf, next(tags), 'LIST "" "*"', deadline)
            assert folder not in listing, f"the deleted folder must be gone: {listing}"
        finally:
            with _admin_ws(nest_instance) as ws:
                ws.call("fauna.bridges.put_imap_policy", {})
    finally:
        sock.close()


def _non_loopback_address() -> str | None:
    """This box's own outward IPv4 address, or None. Connecting a UDP socket
    sends nothing; it only asks the kernel which source address it would use."""
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("192.0.2.1", 9))  # TEST-NET-1: never reached
        address = probe.getsockname()[0]
    except OSError:
        return None
    finally:
        probe.close()
    return None if address.startswith("127.") or address == "0.0.0.0" else address


@pytest.fixture
def lan_mda(mail_bridge_mda, mail_bridge_binary, nest_instance, tmp_path_factory):
    """A second MDA whose IMAP listens on this box's non-loopback address, so a
    test's connections arrive from a non-loopback source. The bridge counts a
    loopback source against its fixed loopback ceiling, never the admin's cap
    (`internal/connlimit/perip.go`; `mail-policy-config.md` § Submission
    policy), so the cap can only be seen from such a source. Yields
    `(address, imaps_port, domain)`."""
    import uuid

    from conftest import _spawn_mda_bridge

    address = _non_loopback_address()
    if address is None:
        pytest.skip("this box has no non-loopback IPv4 address, and the per-address cap "
                    "never applies to a loopback source")
    spawned = _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=nest_instance,
        tmp=tmp_path_factory.mktemp("mail-bridge-mda-lan"),
        bridge_id=f"test-mda-lan-{uuid.uuid4().hex[:12]}",
        domain=mail_bridge_mda.domain,
        imap_bind_host=address,
    )
    try:
        yield address, spawned.imaps_port, mail_bridge_mda.domain
    finally:
        spawned.cleanup()


def _imap_greeted(address: str, port: int, domain: str):
    """One IMAPS connection; its socket once the server's greeting arrives, or
    None when the server closed it instead (shed at accept)."""
    from helpers.mail_wire import _imaps_connect_addr

    try:
        sock, _ = _imaps_connect_addr(address, port, domain, time.monotonic() + 15.0)
        return sock
    except (OSError, AssertionError):
        return None


def _hold(address: str, port: int, domain: str, n: int):
    """Open `n` connections at once; return the greeted sockets (still open)."""
    return [s for s in (_imap_greeted(address, port, domain) for _ in range(n)) if s is not None]


def _close_all(socks):
    for s in socks:
        s.close()


@pytest.mark.feature("admin-mail-policy")
def test_per_address_connection_cap_closes_extra_mail_app_connections_and_zero_lifts_it(
        lan_mda, nest_instance):
    """Under the admin's cap of 2, one address holding two mail-app connections
    has a third closed at once; when one of its two closes, a new one is let in;
    and a cap of zero lets six in at once."""
    address, port, domain = lan_mda

    def _admitted_of(n):
        held = _hold(address, port, domain, n)
        _close_all(held)
        return len(held)

    with _admin_ws(nest_instance) as ws:
        assert ws.call("fauna.bridges.put_auth_policy", {"max_conn_per_ip": 2}).get("ok") is True
    try:
        _poll(lambda: _admitted_of(3), lambda n: n == 2, "the admin's cap of 2 admitting two of three")

        held = _hold(address, port, domain, 2)
        try:
            assert len(held) == 2, f"two connections fit under a cap of 2; got {len(held)}"
            assert _imap_greeted(address, port, domain) is None, (
                "a third simultaneous connection must be closed")
            held.pop().close()
            held.append(_poll(lambda: _imap_greeted(address, port, domain), lambda s: s is not None,
                              "a connection let in once one of the two closed"))
        finally:
            _close_all(held)

        with _admin_ws(nest_instance) as ws:
            assert ws.call("fauna.bridges.put_auth_policy", {"max_conn_per_ip": 0}).get("ok") is True
        _poll(lambda: _admitted_of(6), lambda n: n == 6, "a cap of zero admitting six at once")
    finally:
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_auth_policy", {})
