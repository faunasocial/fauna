"""A setting that changed while the mail server was cut off from the nest
applies when the mail server reconnects (`mail-bridge-lifecycle.md` §
Lifecycle phases — the config snapshot is fetched at startup and on every
reconnect; § Reconnecting — an in-process reconnect re-runs `fetch_config`).

The setting is the admin's message-size cap (`max_message_bytes`), whose
effect is visible over SMTP as a `552 5.3.4` on a message above it. The
nest under the session bridge is stopped through the harness's own handle
(`stop_nest`, never a signal or a name match), so the bridge loses its
connection while the nest cannot take an admin call or send the
`config_changed` nudge. The harness has no door that cuts the bridge's link
while the nest stays up, so the changed setting is written into the stopped
nest's own settings row (the very row `put_spam_policy` upserts — see
`CacheDb::write_singleton_json`); the nest comes back on the same data dir
and the bridge reconnects on its own. Nothing but the reconnect's re-fetch
can carry the new value to the bridge.
"""

import json
import secrets
import sqlite3
import time

import pytest

from helpers.budgets import MAIL_BRIDGE_RECONNECT_S
from helpers.mail_wire import _connect_smtp_starttls, _read_final_reply

pytestmark = pytest.mark.tier_3

_CAP = 16_384


def _message(to_addr: str, filler_bytes: int = 0) -> bytes:
    lines = [
        "From: External Sender <sender@external.test>",
        f"To: {to_addr}",
        f"Subject: config on reconnect {secrets.token_hex(4)}",
        f"Message-ID: <cr-{secrets.token_hex(8)}@external.test>",
        "Date: Sun, 24 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "A message for the config-on-reconnect witness.",
    ]
    lines += ["x" * 76] * (filler_bytes // 78)
    return ("\r\n".join(lines) + "\r\n").encode()


def _inbound_final_reply(handle, rcpt: str, body: bytes) -> str:
    """One inbound transaction on port 25: the final reply to its DATA (or the
    first refusal before it)."""
    mail_from = f"sender-{secrets.token_hex(4)}@external.test"
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


def _write_stored_policy(db_path, *, cap=None, raw=None):
    """Write the stopped nest's spam-policy row: either `raw` JSON text verbatim,
    or the existing row with `max_message_bytes` set to `cap`. Returns the row's
    previous JSON text (None when there was no row)."""
    db = sqlite3.connect(db_path, timeout=10.0)
    try:
        row = db.execute("SELECT overrides_json FROM mail_spam_policy WHERE id = 1").fetchone()
        previous = row[0] if row else None
        if raw is None:
            stored = json.loads(previous) if previous else {}
            stored["max_message_bytes"] = cap
            raw = json.dumps(stored)
        db.execute(
            "INSERT INTO mail_spam_policy (id, overrides_json) VALUES (1, ?1)"
            " ON CONFLICT(id) DO UPDATE SET overrides_json = excluded.overrides_json",
            (raw,),
        )
        db.commit()
        return previous
    finally:
        db.close()


def _while_nest_is_down(nest_instance, change):
    from common.nest import start_nest_in_place, stop_nest

    stop_nest(nest_instance, graceful=True)
    try:
        return change()
    finally:
        start_nest_in_place(nest_instance)


@pytest.mark.feature("mail-server")
def test_a_setting_changed_while_the_mail_server_was_cut_off_applies_when_it_reconnects(
    mail_bridge_mta, nest_instance
):
    """With the nest down (the mail server cut off from it) the admin's size cap
    is changed; once the nest is back and the mail server has reconnected, a
    message above the new cap is refused `552 5.3.4` and one under it is still
    accepted — while before the change the same large message was accepted."""
    handle = mail_bridge_mta
    db_path = nest_instance["db_path"]
    rcpt = f"{handle.recipient_local_part}@{handle.domain}"

    def large():
        return _message(rcpt, filler_bytes=4 * _CAP)

    before = _inbound_final_reply(handle, rcpt, large())
    assert before.startswith("250"), (
        f"before the change a message above the future cap must be accepted: {before!r}; "
        f"bridge log: {handle.log_file}"
    )

    previous = None
    changed = False

    def _change():
        nonlocal previous, changed
        previous = _write_stored_policy(db_path, cap=_CAP)
        changed = True

    try:
        _while_nest_is_down(nest_instance, _change)

        replies = []
        deadline = time.monotonic() + MAIL_BRIDGE_RECONNECT_S
        while True:
            reply = _inbound_final_reply(handle, rcpt, large())
            replies.append(reply)
            if reply.startswith("552"):
                break
            assert time.monotonic() < deadline, (
                f"the cap changed while disconnected never applied after the reconnect; "
                f"replies: {replies[-5:]}; bridge log: {handle.log_file}"
            )
            time.sleep(1.0)  # sleep-ok: cadence of a deadline poll on the SMTP reply, not a timed wait
        assert "5.3.4" in replies[-1], replies[-1]

        small = _inbound_final_reply(handle, rcpt, _message(rcpt))
        assert small.startswith("250"), f"a message under the cap must still deliver: {small!r}"
    finally:
        if changed:
            # Put the stored policy back exactly as it was and let the shared
            # session bridge take it through its next reconnect.
            restored = previous if previous is not None else "{}"
            _while_nest_is_down(
                nest_instance, lambda: _write_stored_policy(db_path, raw=restored)
            )
            end = time.monotonic() + MAIL_BRIDGE_RECONNECT_S
            while not _inbound_final_reply(handle, rcpt, large()).startswith("250"):
                assert time.monotonic() < end, "the session bridge never took the restored cap"
                time.sleep(1.0)  # sleep-ok: cadence of a deadline poll on the SMTP reply, not a timed wait
