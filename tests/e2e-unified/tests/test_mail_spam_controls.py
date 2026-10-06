"""E2E journeys for what the spam page SHOWS about a person's filter, and what it
lets them SET on it.

Target state: docs/goal/behavior/mail-spam.md § Training signal sources (each
history entry names where the lesson was given), § Reset (the confirm says it
cannot be undone), § Encrypted-mode interaction (contributing is off by default
and is a grant the owner can revoke), § 1. Explicit "Mark as spam" gesture (only
on received messages) and § Scoring placement (the account's own spam
threshold); `docs/goal/behavior/mail-aliases.md` § Spam-threshold override (the
threshold rides each message as a delivery stamp). UX/IDs:
tests/e2e-unified/ui.yaml `mail-spam` page + `mail-spam-training-history-list`,
the conversations bubble overflow, and the `nests` trust facet.

The sibling `test_mail_spam.py` owns the page's core lifecycle (history, undo,
reset, the two switches round-tripping). Every mutation here is a gesture in the
app; seeding the history rows and the trained model is arrangement, exactly as
the sibling and `test_mail_client_spam_receive.py` arrange them.

tier_3: a real app driver against a real fauna-nest; the threshold journey adds
the real MTA bridge, because the threshold is folded onto a message when the MTA
delivers it.
"""

import sqlite3
import time
import uuid

import pytest

from helpers.waiting import wait_until
from i18n.strings import S
from helpers.mail_aliases import add_exact_alias

# tui leads (the lead app); the other six apps join by adding their marker once
# their run is green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.macos, pytest.mark.ios]

_UI_S = 20.0

# Bodies from `test_moderation_client_model_write.py`'s calibration: the shared
# text heuristic flags them, and the mark-as-spam train reads the body.
_SPAM_BODY = "BUY NOW!!! CLICK HERE for FREE MONEY!!! Limited time — act now!!!"

# The threshold journey's tokens — the balanced, full-confidence calibration
# `test_mail_client_spam_receive.py` uses (spam ~10k milli, ham ~90), with their
# own strings so the two tests' models never overlap.
_SPAM_TOKEN = "threshspamtokenqz"
_HAM_TOKEN = "threshhamtokenqz"
_SENDER_DOMAIN = "external.test"


def _wait(pred, budget_s: float = _UI_S) -> bool:
    """A deadline poll (convention 14) that answers instead of raising, so the
    caller's assert carries its own diagnosis."""
    try:
        return bool(wait_until(pred, budget_s))
    except AssertionError:
        return False


def _seed_history(app, nest_instance, test_user, seal_helper_binary, rows):
    """Seed the logged-in user's training history, sealed to their own recipient
    key as every row rests. Mail is enabled first: that is what puts the key on
    file, and the app must hold the matching MSEK to unwrap each row's subject
    for display (`_seed_spam_training_history`)."""
    from conftest import _seed_spam_training_history

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    return _seed_spam_training_history(
        db_path=nest_instance["db_path"],
        actor_id=bytes.fromhex(test_user["actor_id_hex"]),
        seal_helper_binary=seal_helper_binary,
        rows=rows,
    )


# ── outcome 9 ──────────────────────────────────────────────────────────────


@pytest.mark.feature("spam")
def test_history_rows_show_the_message_the_lesson_and_where_it_was_given(
    logged_in_app, nest_instance, test_user, seal_helper_binary
):
    """Outcome 9: each history entry shows the message, whether it was marked
    spam or not spam, and where — a mail app's Junk flag, a Junk move, or this
    app's own button.

    The two mail-app lessons are seeded (the sealed rows the mail bridge writes
    when an IMAP app's `\\Junk` gesture trains agent-side); the app-button lesson is given here, through the
    bubble's ⋯ → Mark as spam, and must join the list with its own source."""
    from actions.mail_spam import MailSpamActions

    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    stamp = uuid.uuid4().hex[:6]
    flagged, moved = f"Prize draw {stamp}", f"Team lunch {stamp}"
    _seed_history(app, nest_instance, test_user, seal_helper_binary, [
        {"message_id": bytes([0xC1] * 32), "mailbox": "INBOX", "subject": flagged,
         "label": "spam", "source": "imap_junk_flag"},
        {"message_id": bytes([0xC2] * 32), "mailbox": "Junk", "subject": moved,
         "label": "ham", "source": "imap_junk_move"},
    ])

    spam = MailSpamActions(app.driver)
    spam.navigate()
    assert spam.wait_for_history_count(2), (
        f"seeded 2 lessons; list shows {spam.history_count()} (error: {spam.error_text()!r})"
    )

    def _row(subject: str) -> int:
        return next(i for i, m in enumerate(spam.history_messages()) if subject in m)

    i = _row(flagged)
    assert spam.history_message(i) == f"{flagged} · INBOX"
    assert spam.history_label(i) == S.mail_spam.label_spam
    assert spam.history_source(i) == S.mail_spam.source_imap_junk_flag
    i = _row(moved)
    assert spam.history_message(i) == f"{moved} · Junk"
    assert spam.history_label(i) == S.mail_spam.label_ham
    assert spam.history_source(i) == S.mail_spam.source_imap_junk_move

    # The app's own gesture on a received message.
    app.conversations.inject_and_open_thread(
        rail="FaunaMls", sender=f"prize-{stamp}@self-nest.test", body=_SPAM_BODY
    )
    app.conversations.mark_message_spam(0)

    spam.navigate()
    assert spam.wait_for_history_count(3), (
        f"the app-button lesson never joined the history; list shows "
        f"{spam.history_count()} (error: {spam.error_text()!r})"
    )
    sources = [spam.history_source(j) for j in range(3)]
    assert sources[0] == S.mail_spam.source_explicit_button, (
        f"the newest lesson was given with this app's button; sources {sources!r}"
    )
    assert spam.history_label(0) == S.mail_spam.label_spam


# ── outcome 10 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("spam")
def test_reset_confirm_says_it_cannot_be_undone(
    logged_in_app, nest_instance, test_user, seal_helper_binary
):
    """Outcome 10: resetting the filter asks first, and the ask says it cannot
    be undone. The first press only arms — the history is still all there —
    and relabels the button to say so; the second press resets."""
    from actions.mail_spam import MailSpamActions

    _seed_history(logged_in_app, nest_instance, test_user, seal_helper_binary, [
        {"message_id": bytes([0xD1] * 32), "mailbox": "INBOX", "subject": "Keep me",
         "label": "spam", "source": "imap_junk_flag"},
        {"message_id": bytes([0xD2] * 32), "mailbox": "INBOX", "subject": "And me",
         "label": "ham", "source": "imap_junk_move"},
    ])
    spam = MailSpamActions(logged_in_app.driver)
    spam.navigate()
    assert spam.wait_for_history_count(2)
    assert spam.reset_button_text() == S.mail_spam.reset_button

    spam.arm_reset()
    assert _wait(lambda: spam.reset_button_text() == S.mail_spam.reset_confirm), (
        f"the armed reset must ask, saying it cannot be undone; label reads "
        f"{spam.reset_button_text()!r}"
    )
    assert "cannot be undone" in spam.reset_button_text()
    assert spam.history_count() == 2, "arming alone resets nothing"

    logged_in_app.driver.click("mail-spam-reset-model-button")
    assert spam.wait_for_history_count(0), (
        f"the confirmed reset clears the history; list shows {spam.history_count()} "
        f"(error: {spam.error_text()!r}; "
        f"{logged_in_app.driver.diagnose('mail-spam-reset-model-button')})"
    )


# ── outcome 13 ─────────────────────────────────────────────────────────────


@pytest.fixture
def contribute_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh nest: its admin has never touched the contribute
    switch, so the default is the nest's own, and the grant log is this test's
    alone (a grant cannot be un-minted from the session nest's log)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "spam-contribute-nest")
    yield nest
    cleanup()


def _spam_model_grant_rows(nest) -> list[bytes]:
    """The grant ids the nest holds for its admin — what the aggregation
    holder's fetch answers from."""
    owner = bytes.fromhex(nest["admin"]["actor_id_hex"])
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        return [r[0] for r in conn.execute(
            "SELECT grant_id FROM capability_grants WHERE owner_actor_id = ?", (owner,)
        ).fetchall()]
    finally:
        conn.close()


@pytest.mark.feature("spam")
def test_contributing_is_off_until_turned_on_and_is_a_revocable_grant(app, contribute_nest):
    """Outcome 13: contributing training to the nest's shared starting point is
    off until the person turns it on, and turning it on shows in their grant
    log as trust they can revoke there (`mail-spam.md` § Encrypted-mode
    interaction: the switch mints the keyless `content.read{spam-model}` grant
    to the box's aggregation holder)."""
    from actions.mail_spam import MailSpamActions
    from tests.test_nest_trust_grants import _login_as_admin, _seed_mda_holder

    app.nest_trust.require_mint_test_setup_supported()
    nest = contribute_nest
    _login_as_admin(app, nest)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    _seed_mda_holder(nest)

    spam = MailSpamActions(app.driver)
    spam.navigate()
    assert spam.is_page_visible()
    assert not spam.is_contribute_baseline_on(), "contributing is off by default"

    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible()
    assert app.nest_trust.grant_count() == 0, "no trust exists before the switch"

    spam.navigate()
    spam.toggle_contribute_baseline()
    assert _wait(spam.is_contribute_baseline_on), (
        f"the contribute switch never read on (error: {spam.error_text()!r})"
    )
    assert _wait(lambda: len(_spam_model_grant_rows(nest)) == 1), (
        "turning contributing on must deposit one grant on the nest"
    )

    app.linked_nests.navigate()
    assert app.nest_trust.wait_for_grant_count(1), (
        f"the contribution grant never showed in the grant log. error: {app.error_text()!r}"
    )
    scope = app.nest_trust.grant_scope_text(0)
    assert S.nests.scope_spam_model in scope, (
        f"the grant row must say what it lets the nest read; got {scope!r}"
    )

    app.nest_trust.revoke(0)
    assert _wait(lambda: app.nest_trust.grant_count() == 0), (
        f"a revoked grant leaves the Now lens; still {app.nest_trust.grant_count()}"
    )
    assert _wait(lambda: not _spam_model_grant_rows(nest)), (
        "revoking from the grant log deletes the grant on the nest"
    )


# ── outcome 14 ─────────────────────────────────────────────────────────────


@pytest.mark.feature("spam")
def test_mark_as_spam_is_offered_on_received_messages_only(logged_in_app):
    """Outcome 14: Mark as spam is offered on a message you received and never
    on your own. Each bubble's ⋯ menu is opened; the own one's Delete (an
    own-only action) proves its menu has rendered before its lack of Mark as
    spam is read."""
    conv = logged_in_app.conversations
    stamp = uuid.uuid4().hex[:6]

    conv.inject_and_open_thread(
        rail="FaunaMls", sender=f"stranger-{stamp}@self-nest.test", body=_SPAM_BODY
    )
    conv.open_message_actions(0)
    assert _wait(lambda: conv.driver.count("dm-message-mark-as-spam-button") >= 1), (
        "a received message's menu must offer Mark as spam; "
        f"{conv.driver.diagnose('dm-message-mark-as-spam-button')}"
    )

    conv.seed_own_message(f"My own words {stamp}", recipient=f"friend-{stamp}@self-nest.test")
    conv.open_message_actions(0)
    assert _wait(lambda: conv.driver.count("dm-message-delete-button") >= 1), (
        "the own message's menu never rendered (its Delete is missing); "
        f"{conv.driver.diagnose('dm-message-delete-button')}"
    )
    assert conv.driver.count("dm-message-mark-as-spam-button") == 0, (
        "Mark as spam must never be offered on your own message"
    )


# ── outcome 15 ─────────────────────────────────────────────────────────────


@pytest.fixture
def _threshold_cleanup(nest_instance, test_user):
    """The override and the seeded model are the session user's: put both back
    so later tests see the deployment default and a cold-start model (the
    contamination `test_mail_client_spam_receive.py::_isolate_spam_model`
    documents)."""
    yield
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    ) as api:
        api.call("fauna.bridges.set_spam_threshold_override", {"spam_threshold_override": None})
    conn = sqlite3.connect(nest_instance["db_path"], timeout=10.0)
    try:
        conn.execute("DELETE FROM spam_models WHERE actor_id = ?1", (test_user["actor_id_bytes"],))
        conn.commit()
    finally:
        conn.close()


def _deliver(mta, recipient: str, subject: str, body: str, deadline: float) -> None:
    from helpers.mail_wire import _connect_smtp_starttls

    raw = ("\r\n".join([
        f"From: Someone <sender@{_SENDER_DOMAIN}>",
        f"To: {recipient}",
        f"Subject: {subject}",
        f"Message-ID: <{uuid.uuid4().hex}@{_SENDER_DOMAIN}>",
        "Date: Mon, 21 Sep 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        body,
    ]) + "\r\n").encode()
    with _connect_smtp_starttls(mta.mx_port, mta.domain, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{_SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("spam")
@pytest.mark.real_conversations
def test_your_own_spam_threshold_sorts_your_mail(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, _threshold_cleanup,
    seal_helper_binary,
):
    """Outcome 15: the person sets their account's spam threshold on the spam
    page, and their mail is sorted by it — here, in the app itself, whose
    on-device scorer judges each message by the threshold stamped on it at
    delivery (`mail-aliases.md` § Spam-threshold override).

    Two rounds against one trained model, differing only in the threshold the
    page set before delivery. With 0 ("never file to Junk") a message the model
    calls spam stays in the inbox; with the threshold cleared, the same kind of
    message is filed away. Each round delivers its spam before a ham and waits
    for the ham: messages are scored in arrival order, so the ham showing proves
    the spam was already judged — no sleep decides anything."""
    from actions.mail_spam import MailSpamActions
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _seed_spam_model

    app = logged_in_app
    actor_id = test_user["actor_id_bytes"]
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    local_part = "threshuser" + uuid.uuid4().hex[:4]
    recipient = f"{local_part}@{mail_bridge_mta.domain}"
    add_exact_alias(
        nest_instance["url"], test_user["signing_key"], mail_bridge_mta.domain, local_part
    )
    _seed_spam_model(
        db_path=nest_instance["db_path"], actor_id=actor_id,
        seal_helper_binary=seal_helper_binary,
        ngrams={_SPAM_TOKEN: (110, 0), _HAM_TOKEN: (0, 110)},
        spam_messages=110, ham_messages=110,
    )

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

    nonce = uuid.uuid4().hex[:8]
    spam = MailSpamActions(app.driver)

    # ── Round 1: the page sets 0 — "never file my mail to Junk".
    spam.navigate()
    spam.set_threshold_override("0")
    assert _wait(lambda: _override() == 0), (
        f"the page's threshold never reached the account (reads {_override()!r}; "
        f"error: {spam.error_text()!r})"
    )
    kept, ham1 = f"Kept offer {nonce}", f"Notes one {nonce}"
    deadline = time.monotonic() + 60.0
    _deliver(mail_bridge_mta, recipient, kept, f"Act now: the {_SPAM_TOKEN} offer.", deadline)
    _deliver(mail_bridge_mta, recipient, ham1, f"The {_HAM_TOKEN} notes.", deadline)
    assert _wait(lambda: _shown(ham1), 60.0), (
        f"the round-1 ham never arrived in the app; threads {_subjects()!r}; "
        f"bridge log {mail_bridge_mta.log_file}"
    )
    assert _shown(kept), (
        f"with the account threshold at 0 the spam must stay in the inbox; "
        f"threads {_subjects()!r}"
    )

    # ── Round 2: the page clears it — the deployment default files spam away.
    spam.navigate()
    spam.set_threshold_override("")
    assert _wait(lambda: _override() is None), (
        f"clearing the page's threshold never reached the account (reads {_override()!r})"
    )
    filed, ham2 = f"Filed offer {nonce}", f"Notes two {nonce}"
    deadline = time.monotonic() + 60.0
    _deliver(mail_bridge_mta, recipient, filed, f"Act now: the {_SPAM_TOKEN} offer.", deadline)
    _deliver(mail_bridge_mta, recipient, ham2, f"The {_HAM_TOKEN} notes.", deadline)
    assert _wait(lambda: _shown(ham2), 60.0), (
        f"the round-2 ham never arrived in the app; threads {_subjects()!r}"
    )
    assert not _shown(filed), (
        f"under the default threshold the spam must be filed to Junk, out of the "
        f"inbox; threads {_subjects()!r}"
    )
