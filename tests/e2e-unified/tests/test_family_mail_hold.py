"""The guardian's held-mail queue, driven from the app over real inbound SMTP.

`family-safety.md` § Guardian policy — the enforcement split (the `hold` arm of
`unknown_sender_mail`: the guardian's queue surfaces "envelope metadata only
(sender address, timestamp — never subject-derived text, never content)") and
§ Reach approvals ("Approve = move to INBOX + allowlist the sender; deny =
discard").

A `mail_hold` queue entry exists only when real inbound mail meets a ward whose
`unknown_sender_mail` is `hold` — the nest has no test door for one — so this
module rides the real Go mail bridge (`mail_bridge_inbound_to_imap`: an MTA on
port 25 and the MDA's IMAPS on one claimed domain, tier_3, no docker). The nest
half of the gate is witnessed on the image by
`platform/docker/test_mail_family_gate_docker.py`; this is the app half.

Its own module rather than `test_family.py`, for two reasons: it needs the mail
bridge fixture that module never stands up, and its ward needs a mail identity
the shared `_shared_family` ward does not carry. The ward still costs ONE
anonymous `invite_request.submit` (`helpers/family_wire.py::admit_ward` — the
budget is documented on `submit_invite_request`), and this module admits exactly
one.

Data flow witnessed: guardian sets `family-policy-unknown-sender-select` =
hold in the editor → `fauna.family.policy.update` → stranger's SMTP DATA → MTA
`ingest_inbound_mail` → the nest's `guardian_mail_verdict` places it in the
ward's held mailbox + `guardian_mail_holds` sidecar → `fauna.family.approvals.list`
→ the guardian's `family-approval-item` row (`approval_item_text`: sender ·
time) → Approve/Deny → `fauna.family.approvals.decide` → IMAP INBOX on the MDA.
"""

import time

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.family_wire import admit_adult_directly, admit_ward
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _imap_auth_plain,
    _imap_cmd,
    _imap_seq_fetch_body,
    _imaps_connect,
)
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: The ward's IMAP MUA-AUTH password (wraps its MSEK; test-local).
WARD_PASSWORD = "family-mail-hold-ward-1"

#: How long one inbound transaction plus the nest's hold placement may take to
#: surface as a queue row, and a decided release to reach the MDA's INBOX view.
DELIVERY_S = 30.0


def _suffix() -> str:
    return f"{int(time.time() * 1000) % 10**9}"


def _send(handle, sender: str, rcpt: str, marker: str) -> None:
    """One real inbound SMTP transaction from `sender` to `rcpt`. The body and
    subject both carry `marker`, so a row that leaked either would show it."""
    raw = "\r\n".join([
        f"From: <{sender}>",
        f"To: {rcpt}",
        f"Subject: subject {marker}",
        f"Message-ID: <{marker}@external.test>",
        "Date: Mon, 25 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The body {marker} is the ward's to read, never the guardian's.",
    ]) + "\r\n"
    deadline = time.monotonic() + 40.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw.encode())
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _inbox_bodies(handle, username: str) -> list[bytes]:
    """Every message body in the ward's INBOX, read over the MDA's IMAPS (AUTH
    PLAIN is the MSEK unwrap, FETCH the ward-side open)."""
    deadline = time.monotonic() + 30.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(sock, buf, "a1", username, WARD_PASSWORD, deadline) == "OK", (
            "the ward's IMAP AUTH must succeed"
        )
        status, resp = _imap_cmd(sock, buf, "a2", "SELECT INBOX", deadline)
        assert status == "OK", f"SELECT INBOX: {status} {resp!r}"
        count = next(
            int(p[1]) for p in (line.split() for line in resp.split("\n"))
            if len(p) >= 3 and p[0] == "*" and p[2].upper() == "EXISTS"
        )
        bodies = []
        for seq in range(1, count + 1):
            fetch_status, body = _imap_seq_fetch_body(sock, buf, f"f{seq}", seq, deadline)
            assert fetch_status == "OK" and body is not None, f"FETCH {seq}: {fetch_status}"
            bodies.append(body)
        sock.sendall(b"a99 LOGOUT\r\n")
    return bodies


def _in_inbox(handle, username: str, marker: str) -> bool:
    return any(marker.encode() in body for body in _inbox_bodies(handle, username))


def _clocks_between(t0: float, t1: float) -> set[str]:
    """Every local `HH:MM` a hold created between `t0` and `t1` (epoch s) can
    show, a minute of slack each side — the row's clock is the tui process's
    local time, and the test runs on the same box."""
    return {
        time.strftime("%H:%M", time.localtime(t))
        for t in range(int(t0) - 60, int(t1) + 61, 30)
    }


@pytest.fixture
def mail_ward(request, nest_instance, seal_helper_binary, mail_bridge_inbound_to_imap):
    """A guardian (direct admission, no anonymous submit) and ONE supervised ward
    with a mail identity on the bridge's domain: the ward is admitted under the
    guardian (`admit_ward`, the module's one submit), then the production MSEK
    recipient recipe — alias, seal pubkey, wrapped MSEK, MLS snapshot — is layered
    onto that ward actor, as the ward's own primary app would provision it."""
    from conftest import _provision_msek_recipient

    suffix = _suffix()
    guardian_handle = f"family-mailg-{suffix}"
    ward_handle = f"family-mailw-{suffix}"
    guardian = admit_adult_directly(nest_instance, guardian_handle)
    ward = admit_ward(nest_instance, guardian, ward_handle)
    recipient = _provision_msek_recipient(
        nest_instance=nest_instance,
        seal_helper_binary=seal_helper_binary,
        domain=mail_bridge_inbound_to_imap.domain,
        local_part=ward_handle,
        password=WARD_PASSWORD,
        actor={
            "actor_id_bytes": bytes.fromhex(ward["actor_id_hex"]),
            "signing_key": ward["signing_key"],
        },
    )
    return {
        "guardian": guardian,
        "ward_handle": ward_handle,
        "username": recipient.username,
    }


@pytest.mark.feature("family-safety")
def test_family_guardian_releases_and_discards_held_mail_seeing_only_sender_and_time(
    admin_app, request, nest_instance, mail_bridge_inbound_to_imap, mail_ward
):
    """With unknown-sender mail on `hold`, a stranger's mail to the child waits in
    the guardian's queue as a row reading exactly "<sender> · <time>" — no subject,
    no body. Approve puts it in the child's INBOX and lets that sender's next mail
    straight through; Deny discards a second stranger's, which never arrives.
    """
    from conftest import _login_app_as

    app = admin_app
    bridge = mail_bridge_inbound_to_imap
    bridge.assert_mta_running()
    ward_handle, username = mail_ward["ward_handle"], mail_ward["username"]

    # ── The guardian holds unknown-sender mail through the editor (point 8).
    app.driver.reset()
    _login_app_as(app, request, nest_instance, mail_ward["guardian"])
    app.family.navigate()

    def _ward_listed() -> bool:
        if ward_handle in app.family.ward_handles():
            return True
        app.family.reload()
        return False

    wait_until(
        _ward_listed,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"ward {ward_handle!r} not listed. error: {app.error_text()!r}",
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), f"editor did not load: {app.error_text()!r}"
    app.family.set_unknown_sender("hold")
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    suffix = _suffix()
    kept, dropped = f"kept-{suffix}@external.test", f"dropped-{suffix}@external.test"
    first, later, unwanted = (f"holdmark{n}{suffix}qx" for n in ("a", "b", "c"))

    def _rows() -> list[str]:
        return [app.family.approval_text(i) for i in range(app.family.approval_count())]

    def _row_of(sender: str) -> int | None:
        return next((i for i, row in enumerate(_rows()) if sender in row), None)

    def _await_row(sender: str) -> int:
        def _listed() -> bool:
            if _row_of(sender) is not None:
                return True
            app.family.reload()
            return False

        wait_until(
            _listed,
            DELIVERY_S,
            diagnose=lambda: (
                f"{sender}'s held mail must wait in the guardian's queue; rows={_rows()!r} "
                f"error: {app.error_text()!r} (see {bridge.bridge_log_hint()})"
            ),
        )
        return _row_of(sender)

    # ── Two strangers mail the child; both are held, neither reaches the INBOX.
    t0 = time.time()
    _send(bridge, kept, username, first)
    _send(bridge, dropped, username, unwanted)
    row = _await_row(kept)
    _await_row(dropped)
    t1 = time.time()
    assert not _in_inbox(bridge, username, first), "a held mail must not be in the INBOX"
    assert not _in_inbox(bridge, username, unwanted), "a held mail must not be in the INBOX"

    # ── Outcome 17: the row is exactly who sent it and when — nothing it says.
    text = app.family.approval_text(row)
    for leak in (first, f"subject {first}"):
        assert leak not in text, f"the guardian's row must never show the mail's content: {text!r}"
    # The row element's text begins "<sender> · <time>"; on web (and wherever the
    # row also holds the ward's handle and the Approve/Deny labels) that text
    # follows, space-separated, in the same element — so the time is the token
    # right after the separator, not the whole remainder.
    prefix = f"{kept} · "
    assert text.startswith(prefix), f"the row must name the sender: {text!r}"
    tail = text[len(prefix):]
    clocks = _clocks_between(t0, t1)
    assert any(tail == w or tail.startswith(w + " ") for w in (*clocks, "Yesterday")), (
        f"the row must show when the mail was held (one of {sorted(clocks)!r}): {text!r}"
    )

    # ── Outcome 16a: Approve releases it to the child's INBOX…
    app.family.approve(row)
    assert not app.has_error(), f"approve raised: {app.error_text()!r}"
    wait_until(
        lambda: _row_of(kept) is None or app.family.reload(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"the released mail must leave the queue; rows={_rows()!r}",
    )
    wait_until(
        lambda: _in_inbox(bridge, username, first),
        DELIVERY_S,
        diagnose=lambda: f"the released mail must reach the ward's INBOX ({username})",
    )

    # …and lets that sender's later mail straight through, unheld.
    _send(bridge, kept, username, later)
    wait_until(
        lambda: _in_inbox(bridge, username, later),
        DELIVERY_S,
        diagnose=lambda: (
            f"the released sender's next mail must arrive unheld; rows={_rows()!r}"
        ),
    )
    app.family.reload()
    assert _row_of(kept) is None, f"the released sender must not be held again: {_rows()!r}"

    # ── Outcome 16b: Deny discards the other stranger's mail; it never arrives.
    app.family.deny(_await_row(dropped))
    assert not app.has_error(), f"deny raised: {app.error_text()!r}"
    wait_until(
        lambda: _row_of(dropped) is None or app.family.reload(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"the discarded mail must leave the queue; rows={_rows()!r}",
    )
    bodies = _inbox_bodies(bridge, username)
    assert not any(unwanted.encode() in b for b in bodies), (
        "a discarded mail must never reach the child's INBOX"
    )
    assert sum(first.encode() in b or later.encode() in b for b in bodies) == 2, (
        f"the INBOX holds exactly the released mail and the later one; {len(bodies)} bodies"
    )
