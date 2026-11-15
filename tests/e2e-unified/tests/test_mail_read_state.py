"""tier_3: mail read state rides the mailbox's own IMAP ``\\Seen`` flag
(``docs/goal/behavior/conversation-read-state.md`` § Mail: ``\\Seen`` is the
marker; the wire is ``mail-app-surface.md`` § Read state).

Two journeys, every binary real — a real inbound mail through the real MTA,
the real shared receive loop, the real nest flag write:

* **Unread at launch.** A delivered mail the user never opened is still unread
  after the app restarts. Before the mailbox flag was the marker, a restart's
  launch floor made every message stamped before it read — history — so a mail
  that arrived while the app was closed showed no indicator at all.
* **Read on one device, read on the other.** Two devices of one account both
  show the mail unread; opening it on the first sets ``\\Seen`` on the nest (one
  batched ``fauna.email.inbox.mark_seen``), the nest's ``fauna.mail.flags_changed``
  wake reaches the second, whose cursored ``flag_changes`` drain clears it —
  without a relaunch. The second device then restarts and the mail is still
  read, because the flag is on the nest, not in either app.
* **Read in the app, read in a mail client — and back.** Opening the mail in
  the app sets ``\\Seen``, which a scripted IMAP mail client then reads; the
  mail client's mark-unread and mark-read (``STORE ∓FLAGS (\\Seen)``) each move
  the running app's mark through the same wake — the flag is the truth in both
  directions.

Both assert state (``unread_count`` off the published thread rows) under a named
budget, never a timing (convention 14). Threads are found by a per-test nonce in
the subject, so mail other tests leave in the session's shared inbox never
matches — and each device's own ``thread_id`` is looked up on that device, since
a thread id is minted per app run.

Test taxonomy:
- ``tier_3`` (mocking depth): every binary real, real SMTP wire, real seal →
  fetch → open → ingest, and the real flag write, wake and delta fetch.
"""

import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import (
    deliver_inbound,
    plain_message,
    route_inbound_mail_to_app,
    thread_with_nonce,
    wait_for_thread_with_nonce,
)
from helpers.waiting import wait_until

# tui leads (the default app set); linux shares the same session loop and the
# second-device fixture, and is listed so its sweep carries the witness too. The
# web, android and windows marks sit on the tests, not the module: windows
# carries the restart and two-device journeys, while the mail-client journey
# (outcome 20) is not lifted there yet.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.real_conversations,
]

SENDER = "reader@external.test"


def _deliver(mail_bridge_mta, recipient_addr: str, nonce: str) -> None:
    deliver_inbound(
        mail_bridge_mta.mx_port,
        mail_bridge_mta.domain,
        SENDER,
        recipient_addr,
        plain_message(SENDER, recipient_addr, nonce),
        time.monotonic() + 40.0,
    )


def _unread(app, nonce: str):
    """The nonce thread's ``unread_count`` on ``app``, or ``None`` when it is not
    in the list (yet)."""
    thread = thread_with_nonce(app, nonce)
    return None if thread is None else thread.unread_count


def _wait_unread(app, nonce: str, expected: int, *, what: str, bridge_log) -> None:
    wait_until(
        lambda: _unread(app, nonce) == expected,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"{what}: the mail tagged {nonce!r} should read unread_count == "
            f"{expected}; it reads {_unread(app, nonce)!r}.\n"
            f"  threads: {[(t.label, t.unread_count, t.rail) for t in app.conversations.list_threads()]}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {bridge_log}"
        ),
    )


@pytest.mark.web
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.feature("email-in-conversations")
def test_a_mail_nobody_opened_is_still_unread_after_a_restart(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    app = logged_in_app
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "readstate-restart"
    )
    nonce = f"unreadrestart{int(time.time() * 1000)}qx"
    _deliver(mail_bridge_mta, recipient, nonce)
    wait_for_thread_with_nonce(
        app, nonce, what="before the restart", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )
    _wait_unread(app, nonce, 1, what="arrived while running",
                 bridge_log=mail_bridge_mta.log_file)

    # The restart re-drains the mailbox from the nest. Every record is stamped
    # before this run began, so the launch floor would call it history; the
    # mail rail reads the missing `\Seen` instead.
    app.driver.hard_reload()
    wait_for_thread_with_nonce(
        app, nonce, what="after the restart", budget_s=MLS_HANDSHAKE_S,
        bridge_log=mail_bridge_mta.log_file,
    )
    _wait_unread(app, nonce, 1, what="after the restart, never opened",
                 bridge_log=mail_bridge_mta.log_file)


@pytest.mark.web
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.feature("email-in-conversations")
def test_a_mail_read_on_one_device_reads_on_the_other_and_stays_read(
    request, logged_in_app, android_other_device_seat, mail_bridge_mta, nest_instance,
    test_user
):
    # On android the roles swap (`alice_second_device`'s docstring): the
    # outcome is about the OTHER device hearing the read, so a tui seat is
    # device A, who reads, and the phone — signed in all along — is device B.
    seat = android_other_device_seat
    device_a = logged_in_app if seat is None else seat("read-state-reader")[0]
    recipient = route_inbound_mail_to_app(
        device_a, mail_bridge_mta, nest_instance, test_user, "readstate-seats"
    )
    nonce = f"readseats{int(time.time() * 1000)}qx"
    _deliver(mail_bridge_mta, recipient, nonce)
    log = mail_bridge_mta.log_file

    wait_for_thread_with_nonce(device_a, nonce, what="device A", budget_s=MLS_HANDSHAKE_S,
                               bridge_log=log)
    _wait_unread(device_a, nonce, 1, what="device A before any read", bridge_log=log)

    # The same account's second device: its own launch, its own receive loop,
    # the same mailbox. Web's second device is a twin page in its own
    # BrowserContext; every other app launches a second process; on android it
    # is the phone itself.
    if seat is not None:
        device_b = logged_in_app
    else:
        second_device = (
            "alice_second_web_device" if device_a.driver.is_web() else "alice_second_device"
        )
        device_b, _driver_b = request.getfixturevalue(second_device)
    wait_for_thread_with_nonce(device_b, nonce, what="device B", budget_s=MLS_HANDSHAKE_S,
                               bridge_log=log)
    _wait_unread(device_b, nonce, 1, what="device B before any read", bridge_log=log)

    # Device A opens the thread — the read, and the `\Seen` write it owes.
    device_a.conversations.open_thread_by_id(thread_with_nonce(device_a, nonce).thread_id)
    _wait_unread(device_a, nonce, 0, what="device A after opening it", bridge_log=log)

    # Device B hears of it with no relaunch: the nest's flag-change wake, then
    # one cursored flag_changes drain.
    _wait_unread(device_b, nonce, 0,
                 what="device B, after device A read the mail (no relaunch)",
                 bridge_log=log)

    # And it is the nest's flag, not device B's memory: a restart re-drains the
    # mailbox and still finds the mail read.
    device_b.driver.hard_reload()
    wait_for_thread_with_nonce(device_b, nonce, what="device B after its restart",
                               budget_s=MLS_HANDSHAKE_S, bridge_log=log)
    _wait_unread(device_b, nonce, 0, what="device B after its restart", bridge_log=log)


# The PLAIN password the app mints when it enables mail, and the mail client
# signs in with.
_MAIL_CLIENT_PASSWORD = "read-state-mail-client-pw-1"


class _MailClient:
    """A scripted IMAP mail client on the account's own mailbox: IMAPS, AUTH
    PLAIN with the password the app minted, ``INBOX`` selected. It reads flags
    with ``FETCH (FLAGS)`` only — never ``BODY[]``, whose implicit ``\\Seen``
    would be the client marking the mail read behind the test's back."""

    def __init__(self, handle, addr: str, password: str):
        from helpers.mail_wire import _imap_auth_plain, _imaps_connect

        self._deadline = time.monotonic() + 120.0
        self._sock, self._buf = _imaps_connect(handle, self._deadline)
        self._n = 0
        status = _imap_auth_plain(self._sock, self._buf, self._tag(), addr, password,
                                  self._deadline)
        assert status == "OK", (
            f"the mail client must sign in with the password the app minted; got {status}"
        )

    def _tag(self) -> str:
        self._n += 1
        return f"rs{self._n}"

    def cmd(self, command: str) -> str:
        from helpers.mail_wire import _imap_cmd

        status, text = _imap_cmd(self._sock, self._buf, self._tag(), command, self._deadline)
        assert status == "OK", f"IMAP {command!r} must succeed; got {status}: {text!r}"
        return text

    def inbox_count(self) -> int:
        """A fresh ``SELECT INBOX``'s ``EXISTS`` — re-selecting is also what
        makes every later ``FETCH`` read the mailbox as it is now."""
        for line in self.cmd("SELECT INBOX").split("\n"):
            parts = line.split()
            if len(parts) >= 3 and parts[0] == "*" and parts[2].upper() == "EXISTS":
                return int(parts[1])
        raise AssertionError("SELECT INBOX reported no EXISTS line")

    def seen(self, seq: int) -> bool:
        self.inbox_count()
        text = self.cmd(f"FETCH {seq} (FLAGS)")
        line = next((l for l in text.split("\n") if f"* {seq} FETCH" in l), "")
        assert "FLAGS" in line, f"FETCH {seq} (FLAGS) returned no flags: {text!r}"
        return "\\Seen" in line

    def close(self) -> None:
        try:
            self._sock.sendall(b"rs99 LOGOUT\r\n")
        finally:
            self._sock.close()


@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("email-in-conversations")
def test_a_mail_read_in_the_app_reads_in_a_mail_client_and_the_mail_client_moves_it_back(
    app, dedicated_mail_nest, request
):
    """Outcome 20 (`conversation-read-state.md` § Mail: `\\Seen` is the marker):
    the app and a third-party mail client share ONE read flag.

    * Opening the mail in the app sets `\\Seen`, and the mail client reads it.
    * The mail client's "mark as unread" (`STORE -FLAGS (\\Seen)`) re-enters
      the mail into the running app's unread set, and its "mark as read" takes it
      out again — each through the nest's `flags_changed` wake, with no relaunch.

    While the mail client moves the flag, the app has ANOTHER thread open: the
    thread a user has open is read by definition (`conversations.md` § *When a
    thread is read*), so marking the open thread unread would be re-read at once
    and witness nothing.

    The account is the dedicated nest's admin with a client-minted PLAIN
    password — the realistic single-user deployment, and the one credential a
    scripted mail client can sign in with (`test_mail_sent_feed.py`'s preamble).
    """
    from helpers.mail_client_ui import deliver_inbound, plain_message
    from helpers.mail_dedicated_nest import (
        alias_admin_to_address,
        dedicated_node_url,
        login_as_nest_admin,
    )

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    admin_addr = alias_admin_to_address(nest, domain)
    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_MAIL_CLIENT_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(
        1, timeout=app.mail_settings.ENABLE_SETTLE_S
    ), f"enabling mail must mint the first credential; error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    assert app.mail_settings.wait_for_enabled_status(timeout=app.mail_settings.ENABLE_SETTLE_S), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    # The enable opened the deployment gates; the binaries venue has no s6, so
    # play supervisor (the sibling MUA tests' step).
    handle.rebind_after_enable()

    stamp = int(time.time() * 1000)
    target = f"mailclientread{stamp}qx"
    parked = f"mailclientpark{stamp}qx"
    # The target first, so it is the mailbox's message 1: the fresh admin
    # mailbox holds exactly these two, and the 250 on each `.` follows its
    # ingest, so the order is the delivery order.
    for sender, nonce in ((SENDER, target), ("parked@external.test", parked)):
        deliver_inbound(handle.mx_port, domain, sender, admin_addr,
                        plain_message(sender, admin_addr, nonce), time.monotonic() + 40.0)
    log = handle.bridge_log_hint()

    for nonce in (target, parked):
        wait_for_thread_with_nonce(app, nonce, what="arrived", budget_s=MLS_HANDSHAKE_S,
                                   bridge_log=log)
    _wait_unread(app, target, 1, what="before any read", bridge_log=log)

    client = _MailClient(handle, admin_addr, _MAIL_CLIENT_PASSWORD)
    try:
        assert client.inbox_count() == 2, "the fresh mailbox holds the two delivered mails"
        assert not client.seen(1), "nobody has read the mail yet, so it carries no \\Seen"

        # ── The app reads it → the mail client sees `\Seen`.
        app.conversations.open_thread_by_id(thread_with_nonce(app, target).thread_id)
        _wait_unread(app, target, 0, what="after opening it in the app", bridge_log=log)
        wait_until(
            lambda: client.seen(1),
            MLS_HANDSHAKE_S,
            diagnose=lambda: "the app read the mail, but the mail client still sees "
            f"it without \\Seen; {log}",
        )

        # Leave the target: open the other mail, so the target is not the thread
        # being read while the mail client moves its flag.
        app.conversations.open_thread_by_id(thread_with_nonce(app, parked).thread_id)
        _wait_unread(app, parked, 0, what="the other mail, opened", bridge_log=log)
        _wait_unread(app, target, 0, what="the target, after leaving it", bridge_log=log)

        # ── The mail client marks it unread → the running app shows it unread.
        client.cmd("STORE 1 -FLAGS (\\Seen)")
        assert not client.seen(1), "the mail client's own mark-unread must hold on the nest"
        _wait_unread(app, target, 1,
                     what="after the mail client marked it unread (no relaunch)",
                     bridge_log=log)

        # ── The mail client marks it read again → the running app follows.
        client.cmd("STORE 1 +FLAGS (\\Seen)")
        _wait_unread(app, target, 0,
                     what="after the mail client marked it read again (no relaunch)",
                     bridge_log=log)
    finally:
        client.close()
