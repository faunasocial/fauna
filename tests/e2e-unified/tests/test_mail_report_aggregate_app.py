"""tier_3 — the shared report aggregate applied to mail at the APP's scoring
position: a message already shown in the person's inbox is filed to Junk by
their own app once three people who share reports have reported it
(`docs/goal/behavior/report-sharing.md` § Applying the aggregate — the user's
own threshold; `mail-spam.md` § Scoring placement, § Re-file timing).

The MDA position's witness is `test_mail_bridge_report_aggregate.py`; this is
its twin for a person who reads mail only in the app, whose on-device scorer
(`InboxSpamScorer` — in the shared receive loop every native app runs, and
on web in the browser's mail loop, which fetches the re-opened copy and hands
it to the same shared judgment) is the only agent that ever scores their
INBOX:

1. One message is delivered by real inbound SMTP to the app's person and to
   three IMAP readers in one transaction, so every copy carries the same
   canonical report-hash.
2. The person's account threshold is 3, set on the spam page before delivery
   (the stamp folds at RCPT); their model calls the message ham, so the app's
   first pass scores it, watermarks it and shows it in the inbox.
3. The three readers opt in to report sharing and move their copies to Junk in
   their mail app. The third reaches k = 3: the aggregate lands on every copy
   and the nest RE-OPENS the person's copy (removes its watermark).
4. The app learns of the re-open through the flag-change feed, scores the held
   copy again with the report term (3 points at the person's line of 3) and
   files it: the thread leaves the inbox.

Fixture preconditions (carve-out (b) of the E2E rules): the person's sealed
model is seeded so the first pass watermarks the copy, as the MDA witness and
`test_your_own_spam_threshold_sorts_your_mail` seed theirs. Every gesture under
test — the threshold, delivery, the readers' MOVE, the opt-in — is real; no
sleep decides anything: a ham delivered after the reports proves a receive pass
ran, and the inbox is then read until the filed message is gone, under a
deadline (convention 14).

The untrained twin (`test_three_reports_file_an_unscored_message_already_in_your_inbox`)
is the same journey for a person with NO trained model: their app's
report-only scorer shows the message without watermarking it, so the nest's
re-open has no watermark to remove — it advances the copy's mod-sequence
instead, fires the app's `fauna.mail.flags_changed` wake, and the flag-change
feed carries the copy's `report:spam` row, which is what owes the held copy its
scoring pass (`report-sharing.md` § Applying the aggregate → *The re-open is a
metadata change, not a flag change*).
"""

import secrets
import sqlite3
import time
import uuid

import pytest

from conftest import _provision_msek_recipient, _seed_spam_model
from helpers.mail_aliases import add_exact_alias
from helpers.mail_wire import ImapSession, _connect_smtp_starttls
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.test_mail_spam_controls import _threshold_cleanup  # noqa: F401 — fixture

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.tui,
    pytest.mark.android,
]

HAM_TOKEN = "rptapphamtokenqz"
SPAM_TOKEN = "rptappspamtokenqz"
SENDER = "sender@external.test"


def _fresh(mda, nest_instance, seal_helper_binary, tag):
    local = f"rptapp{tag}{secrets.token_hex(3)}"
    return _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=mda.domain, local_part=local, password=f"{local}-password-1")


def _message(subject, nonce, body):
    return (
        "\r\n".join([
            f"From: Campaign Sender <{SENDER}>",
            "To: undisclosed-recipients:;",
            f"Subject: {subject}",
            f"Message-ID: <{nonce}@external.test>",
            "Date: Mon, 06 Jul 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            body,
        ]) + "\r\n"
    ).encode()


def _deliver(handle, addresses, raw, deadline):
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{SENDER}>", "250", deadline)
        for address in addresses:
            conn.cmd(f"RCPT TO:<{address}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _published(nest_instance, reader):
    """The report-share published list a reader sees — the k-gated aggregate
    (count >= 3) the readers' moves produced, for the failure message only."""
    return ws_api.report_share_status(nest_instance["port"], reader.recipient).get(
        "published", [])


def _clear_spam_model(db_path, actor_id):
    """The session's person may carry a model an earlier test seeded: the
    untrained journey starts from none (a fixture precondition, carve-out (b))."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute("DELETE FROM spam_models WHERE actor_id = ?1", (actor_id,))
        conn.commit()
    finally:
        conn.close()


def _three_reports_file_the_shown_message(
    logged_in_app, mail_bridge_inbound_to_imap, nest_instance, test_user,
    seal_helper_binary, *, trained,
):
    from actions.mail_spam import MailSpamActions
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    mda = handle.mda
    db = nest_instance["db_path"]
    app = logged_in_app
    actor_id = test_user["actor_id_bytes"]

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    local_part = "rptapp" + uuid.uuid4().hex[:6]
    person = f"{local_part}@{mda.domain}"
    add_exact_alias(nest_instance["url"], test_user["signing_key"], mda.domain, local_part)
    if trained:
        _seed_spam_model(
            db_path=db, actor_id=actor_id, seal_helper_binary=seal_helper_binary,
            ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
            spam_messages=110, ham_messages=110,
        )
    else:
        _clear_spam_model(db, actor_id)

    def _override():
        with WsRpcAdminClient(
            nest_instance["url"], actor_id=actor_id,
            signing_key=bytes(test_user["signing_key"]),
        ) as api:
            return api.call("fauna.bridges.get_spam_threshold_override", {}).get(
                "spam_threshold_override")

    def _subjects():
        return [t.label or "" for t in app.conversations.list_threads()]

    def _shown(subject):
        return any(subject in s for s in _subjects())

    # (2) The person's own line of 3, set on the spam page before delivery.
    spam = MailSpamActions(app.driver)
    spam.navigate()
    spam.set_threshold_override("3")
    wait_until(
        lambda: _override() == 3, 20.0,
        diagnose=lambda: f"the page's threshold never reached the account (reads "
        f"{_override()!r}; error: {spam.error_text()!r})",
    )

    readers = [_fresh(mda, nest_instance, seal_helper_binary, f"r{i}") for i in range(3)]
    nonce = f"rptapp{int(time.time() * 1000)}qx"
    subject = f"Your parcel is held {nonce}"
    deadline = time.monotonic() + 240.0
    # (1) One transaction to all four: one canonical report-hash.
    _deliver(
        handle, [person] + [r.username for r in readers],
        _message(subject, nonce, f"Hello, {HAM_TOKEN}: your parcel is held ({nonce})."),
        deadline,
    )
    wait_until(
        lambda: _shown(subject), 60.0,
        diagnose=lambda: f"the message never reached the person's inbox; threads "
        f"{_subjects()!r}; MDA log {mda.log_file}",
    )

    # (3) Three opted-in readers move their copies to Junk.
    for r in readers:
        assert ws_api.report_share_set(nest_instance["port"], r.recipient, True)["share"]
        s = ImapSession(mda, r, deadline)
        try:
            [uid] = s.find("INBOX", nonce)
            s.ok(f"UID MOVE {uid} Junk")
            assert len(s.find("Junk", nonce)) == 1
        finally:
            s.close()

    # A later ham to the person: once it shows, a receive pass (and the
    # flag-change sweep after it) has run since the re-open.
    later = f"Lunch notes {nonce}"
    _deliver(
        handle, [person],
        _message(later, f"{nonce}-later", f"The {HAM_TOKEN} notes ({nonce})."),
        deadline,
    )
    wait_until(
        lambda: _shown(later), 60.0,
        diagnose=lambda: f"the later ham never arrived in the app; threads {_subjects()!r}",
    )
    # (4) Filed by the person's own app, at their own line.
    wait_until(
        lambda: not _shown(subject), 60.0,
        diagnose=lambda: f"three reports must file the already-shown message at the "
        f"person's line of 3; threads {_subjects()!r}; published aggregates "
        f"{_published(nest_instance, readers[0])!r} (one with count 3 = k reached)",
    )
    assert _shown(later), "the ham stays in the inbox"


@pytest.mark.feature("spam")
@pytest.mark.real_conversations
def test_three_reports_file_a_message_already_in_your_inbox(
    logged_in_app, mail_bridge_inbound_to_imap, nest_instance, test_user,
    _threshold_cleanup, seal_helper_binary,  # noqa: F811 — the imported fixture
):
    """Once three people who share reports have reported a message, the app
    files it to Junk against the person's own threshold — even though it was
    already scored and shown in their inbox."""
    _three_reports_file_the_shown_message(
        logged_in_app, mail_bridge_inbound_to_imap, nest_instance, test_user,
        seal_helper_binary, trained=True,
    )


@pytest.mark.feature("spam")
@pytest.mark.real_conversations
def test_three_reports_file_an_unscored_message_already_in_your_inbox(
    logged_in_app, mail_bridge_inbound_to_imap, nest_instance, test_user,
    _threshold_cleanup, seal_helper_binary,  # noqa: F811 — the imported fixture
):
    """A person with no trained model sees the message shown, never scored;
    once three people who share reports have reported it, their app files it
    at their own threshold — the re-open reaches the app as a metadata change
    (`report-sharing.md` § Applying the aggregate → *The re-open is a metadata
    change, not a flag change*) — and a later ham stays."""
    _three_reports_file_the_shown_message(
        logged_in_app, mail_bridge_inbound_to_imap, nest_instance, test_user,
        seal_helper_binary, trained=False,
    )
