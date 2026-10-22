"""The two secret-rotation buttons on the Mail page (mail-mass-mailing.md
§ Secret rotation; mail-forwarding.md § SRS scheme; admin.md § 6 Mail).

`admin-mail-list-unsubscribe-secret-rotate-button` rotates the newsletter
unsubscribe secret and `admin-mail-forward-srs-secret-rotate-button` the
forwarding (SRS) secret. Each is confirmed by a second press of the same
button: the first press only relabels it with the cost, the second rotates.
The shared `MailPolicyMachine` holds that arm (`MailPolicyAction::Press`).

tier_3: a real app driver against a real `fauna-nest` binary and the real Go
mail bridge (`mail_bridge_mta`). Each journey reads its ground truth from mail
the bridge actually moves — the unsubscribe link a new issue carries, a bounce
to forwarded mail reaching the member — and, for the forwarding secret, from the
nest's own record of its secrets (`mail_srs_secrets`; the row ids only, never
the bytes): every forward's `SRS0=` address carries its outbound row id, so two
forwards never share an address and the address alone cannot show whether the
secret moved. Never from the UI alone.
Latency-independent throughout (convention 14): each wait polls for an outcome.

tui is the lead app (the first implementation); web and linux joined this
module's marks in the batched trickle-down. android is compile- and
unit-verified (its emulator e2e is host-gated); the rest join as they land.
"""

from __future__ import annotations

import re
import secrets
import sqlite3
import time
import urllib.error
import urllib.request

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers import budgets
from helpers.budgets import MAIL_OUTBOUND_CYCLE_S
from helpers.mail_wire import _connect_smtp_starttls, _wait_for_tagged
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux]

#: A list issue rides the bridge's 30 s outbound poll (no prompt-drain nudge
#: on the list path — tests/api/test_mail_lists_send.py); a ceiling, not a sleep.
_DELIVERY_TIMEOUT = 60.0

_PAGE = S.admin.mail_page


def _wait(predicate, budget_s: float = budgets.RPC_ROUNDTRIP_S) -> bool:
    """The shared deadline poll, returning a bool so each caller's own assert
    carries its self-diagnosing message (convention 6)."""
    try:
        return bool(wait_until(predicate, budget_s))
    except AssertionError:
        return bool(predicate())


def _client(nest_instance, actor_id: bytes, signing_key: bytes) -> WsRpcAdminClient:
    return WsRpcAdminClient(nest_instance["url"], actor_id=actor_id, signing_key=signing_key)


def _open_mail_page(admin_app):
    admin_app.admin.navigate_mail()
    assert _wait(
        lambda: admin_app.admin.mail_list_unsubscribe_secret_rotate_text()
        == _PAGE.list_unsubscribe_secret_rotate
    ), (
        "the unsubscribe-secret rotate button never showed its resting label: "
        f"{admin_app.admin.mail_list_unsubscribe_secret_rotate_text()!r}; "
        f"error: {admin_app.error_text()!r}"
    )


# ── the newsletter unsubscribe secret ────────────────────────────────


def _issue(owner, list_id: str, list_addr: str, domain: str, stub_mx) -> str:
    """Send one list issue and return the one-click unsubscribe token the
    delivered copy's `List-Unsubscribe` link carries."""
    tag = f"rotate-issue-{secrets.token_hex(4)}"
    message = (
        "\r\n".join([
            f"From: {list_addr}",
            f"To: {list_addr}",
            f"Subject: {tag}",
            f"Message-ID: <{tag}@{domain}>",
            "Date: Sun, 14 Jun 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "A newsletter issue.",
            "",
        ])
        + "\r\n"
    ).encode()
    owner.call("fauna.bridges.send_list_message", {"list_id": list_id, "message": message})
    raw = _wait_for_tagged(stub_mx, tag, timeout=_DELIVERY_TIMEOUT)
    assert raw is not None, f"issue {tag} never reached the stub MX"
    m = re.search(rb"/list/unsubscribe\?t=([A-Za-z0-9_\-]+)", raw)
    assert m, f"no List-Unsubscribe link in issue {tag}:\n{raw[:600]!r}"
    return m.group(1).decode()


def _post_unsubscribe(nest_url: str, token: str) -> int:
    """The recipient's one-click `POST /list/unsubscribe?t=<token>`; the HTTP
    status (200 unsubscribed, 404 a link that no longer works)."""
    req = urllib.request.Request(
        f"{nest_url}/list/unsubscribe?t={token}",
        data=b"List-Unsubscribe=One-Click",
        headers={"Content-Type": "application/x-www-form-urlencoded"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=5.0) as resp:
            return resp.status
    except urllib.error.HTTPError as e:
        return e.code


@pytest.mark.feature("admin-mail-policy")
def test_rotating_the_unsubscribe_secret_takes_a_second_press_and_retires_old_links(
    admin_app, nest_instance, mail_bridge_mta
):
    """One press of the rotate button only arms it — the next issue still
    carries the same unsubscribe link; the second press rotates — the next
    issue carries a new link, and the link from mail sent before stops working."""
    handle = mail_bridge_mta
    owner = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _client(nest_instance, owner["actor_id_bytes"], bytes(owner["signing_key"])) as client:
        local_part = "news-" + secrets.token_hex(4)
        list_id = client.call(
            "fauna.bridges.create_account_list",
            {"local_part": local_part, "local_domain": handle.domain},
        )["list_id"]
        list_addr = f"{local_part}@{handle.domain}"
        client.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": f"sub-{secrets.token_hex(4)}@external.test"},
        )
        before = _issue(client, list_id, list_addr, handle.domain, handle.stub_mx)

        _open_mail_page(admin_app)
        admin_app.admin.press_mail_list_unsubscribe_secret_rotate()
        assert _wait(
            lambda: admin_app.admin.mail_list_unsubscribe_secret_rotate_text()
            == _PAGE.list_unsubscribe_secret_rotate_confirm
        ), (
            "the first press did not arm the confirm: "
            f"{admin_app.admin.mail_list_unsubscribe_secret_rotate_text()!r}"
        )
        assert _issue(client, list_id, list_addr, handle.domain, handle.stub_mx) == before, (
            "the first press rotated the secret without a confirm"
        )

        admin_app.admin.press_mail_list_unsubscribe_secret_rotate()
        # Confirming disarms once the rotation has answered.
        assert _wait(
            lambda: admin_app.admin.mail_list_unsubscribe_secret_rotate_text()
            == _PAGE.list_unsubscribe_secret_rotate
        ), (
            "the confirm never came back to rest: "
            f"{admin_app.admin.mail_list_unsubscribe_secret_rotate_text()!r}; "
            f"error: {admin_app.error_text()!r}"
        )
        after = _issue(client, list_id, list_addr, handle.domain, handle.stub_mx)

    assert after != before, "the confirm did not rotate: a new issue carries the old link"
    assert _post_unsubscribe(handle.nest_url, before) == 404, (
        "the link from mail sent before the rotation still works"
    )
    assert _post_unsubscribe(handle.nest_url, after) == 200, "the new issue's link does not work"


# ── the forwarding (SRS) secret ──────────────────────────────────────


@pytest.mark.feature("admin-mail-policy")
def test_rotating_the_forwarding_secret_takes_a_second_press_and_keeps_old_bounces_home(
    admin_app, nest_instance, mail_bridge_mta
):
    """One press of the rotate button only arms it — the nest still holds the
    same newest forwarding secret; the second press rotates — the nest mints a
    new one, and a bounce to mail forwarded before still reaches the
    forwarder's mailbox."""
    handle = mail_bridge_mta
    domain = handle.domain
    recipient_addr = f"{handle.recipient_local_part}@{domain}"
    recipient_actor = handle.recipient_actor["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    stamp = int(time.time() * 1000)
    sender = f"srs-ui-orig-{stamp}@orig.test"

    def _set_forward(value):
        # Fixture state, not the journey: the forwarder's own forward-all
        # target (its setting lives on the member's Mail page, not this one).
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            conn.execute(
                "INSERT INTO mail_account_settings (actor_id, forward_all_to, updated_at)"
                " VALUES (?, ?, ?)"
                " ON CONFLICT(actor_id) DO UPDATE SET"
                " forward_all_to = excluded.forward_all_to, updated_at = excluded.updated_at",
                (recipient_actor, value, int(time.time())),
            )
            conn.commit()
        finally:
            conn.close()

    def _newest_secret_id():
        # Which secret is newest — the row id only, never the secret's bytes.
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            (n,) = conn.execute("SELECT MAX(id) FROM mail_srs_secrets").fetchone()
            return n
        finally:
            conn.close()

    def _inbox_count():
        conn = sqlite3.connect(db_path, timeout=10.0)
        try:
            (n,) = conn.execute(
                "SELECT COUNT(*) FROM bridge_imap_messages"
                " WHERE actor_id = ? AND mailbox = 'INBOX'",
                (recipient_actor,),
            ).fetchone()
            return n
        finally:
            conn.close()

    def _smtp(mail_from: str, rcpt: str, body: str):
        deadline = time.monotonic() + 30.0
        with _connect_smtp_starttls(handle.mx_port, domain, deadline) as conn:
            conn.cmd(f"MAIL FROM:<{mail_from}>", "250", deadline)
            conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
            conn.cmd("DATA", "354", deadline)
            conn.send_raw(body.encode())
            conn.cmd(".", "250", deadline)
            conn.cmd("QUIT", "221", deadline)

    def _forward() -> str:
        """Deliver one message from `sender`; the `SRS0=` envelope its forward
        left under."""
        token = f"srs-ui-{secrets.token_hex(4)}"
        _smtp(sender, recipient_addr, "\r\n".join([
            f"From: External Sender <{sender}>",
            f"To: {recipient_addr}",
            f"Subject: srs rotation {token}",
            f"Message-ID: <{token}@orig.test>",
            "Date: Thu, 16 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "Forward me so a bounce comes back.",
        ]) + "\r\n")
        return wait_until(
            lambda: next(
                (env_from for env_from, raw in handle.stub_mx.records()
                 if token.encode() in raw and env_from.startswith("SRS0=")),
                None,
            ),
            MAIL_OUTBOUND_CYCLE_S,
            diagnose=lambda: f"no SRS-rewritten forward of {token} reached the stub MX "
                             f"(bridge log: {handle.log_file})",
        )

    _set_forward(f"srs-ui-{stamp}@external.test")
    try:
        before = _forward()
        secret_before = _newest_secret_id()

        _open_mail_page(admin_app)
        admin_app.admin.press_mail_forward_srs_secret_rotate()
        assert _wait(
            lambda: admin_app.admin.mail_forward_srs_secret_rotate_text()
            == _PAGE.forward_srs_secret_rotate_confirm
        ), (
            "the first press did not arm the confirm: "
            f"{admin_app.admin.mail_forward_srs_secret_rotate_text()!r}"
        )
        assert _newest_secret_id() == secret_before, (
            "the first press rotated the secret without a confirm"
        )

        admin_app.admin.press_mail_forward_srs_secret_rotate()
        assert _wait(
            lambda: admin_app.admin.mail_forward_srs_secret_rotate_text()
            == _PAGE.forward_srs_secret_rotate
        ), (
            "the confirm never came back to rest: "
            f"{admin_app.admin.mail_forward_srs_secret_rotate_text()!r}; "
            f"error: {admin_app.error_text()!r}"
        )
        assert _wait(lambda: _newest_secret_id() > secret_before), (
            f"the confirm did not rotate: the newest secret is still {secret_before}"
        )

        # The downstream MX bounces the message forwarded before the rotation.
        count = _inbox_count()
        _smtp("", before, "\r\n".join([
            "From: Mail Delivery System <MAILER-DAEMON@external.test>",
            f"To: <{before}>",
            "Subject: Delivery Status Notification (Failure)",
            f"Message-ID: <dsn-ui-{stamp}@external.test>",
            "Date: Thu, 16 May 2026 12:05:00 +0000",
            "Content-Type: text/plain; charset=utf-8",
            "",
            "Your forwarded message could not be delivered.",
            "550 5.1.1 mailbox unavailable",
        ]) + "\r\n")
        wait_until(
            lambda: _inbox_count() > count,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"the forwarder's INBOX is still {_inbox_count()} (was {count})",
        )
    finally:
        _set_forward(None)
