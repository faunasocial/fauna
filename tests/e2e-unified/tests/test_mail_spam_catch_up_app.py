"""tier_3 — the app's spam catch-up: mail delivered to a person who reads mail
only in the app, BEFORE their spam model exists, is filed to Junk by their own
app within one mail sweep of the model existing — no relaunch
(`docs/goal/behavior/mail-spam.md` § Scoring placement → *The app's catch-up —
the twin of the MDA's SELECT re-scan*).

The app holds the set of its held `INBOX` copies that lack `$FaunaSpamScored`;
every mail sweep scores from it what the pass's scorer can score — with a
trained model every copy in the set. Before the model exists the app's
report-only scorer leaves an unreported copy un-watermarked, so the message is
shown; once the model exists, the next sweep — woken at once by the
`fauna.bridges.push.spam_model_updated` push every lesson sends the actor's
own sessions — scores it and files it.

1. No model at the start (a fixture precondition, carve-out (b)).
2. A message carrying the spam token arrives by real inbound SMTP; the app
   shows it in the inbox, un-scored. A second, ham message arrives and shows.
3. A model exists but has no say (§ Scoring placement → *The scorer's say*):
   `_seed_spam_model` seals one of 80 samples — confidence 0.2, so even a
   maximal verdict weighs 2.1 points, under the default tier of 5. A marker
   message delivered after it proves a mail pass ran with that model (each
   pass fetches the model afresh); its catch-up takes nothing from the held
   set, so the spam message is still shown and no copy is watermarked — the
   one-shot watermark is not spent on a verdict the ramp decided before the
   open.
4. The model gets a say: `_seed_spam_model` is the nest-side twin of the
   person's first lessons — a model past the confidence ramp, which 50+
   training gestures would otherwise have to build (the ramp clamps the
   per-user term to 0 below `min_samples = 50`, `mail-spam.md` § Combined-score
   formula), so a seeded model is the only way to reach a model that can file
   anything within a test's budget. Then ONE real lesson through the app's own
   mark-as-spam gesture on the ham message sends the model-updated push. The
   lesson re-seals over the seed rather than being refused by it: the app's
   model write reads the stored model and writes under its digest as the
   compare-and-swap precondition (`put_spam_model`'s `expected_model`,
   `mail-spam.md` § Transactional update), so the seed is the model it trains.
5. The earlier message leaves the inbox and rests in Junk carrying the
   watermark; the ham message stays. A backstop tick that runs the catch-up
   before the push arrives files it the same way — both are the one sweep the
   ruling names.

No sleep decides anything: the inbox is read until the message is gone, under
a deadline (convention 14).
"""

import sqlite3
import time

import pytest

from conftest import _seed_spam_model
from helpers.mail_aliases import add_exact_alias
from helpers.waiting import wait_until
from tests.test_mail_report_aggregate_app import _clear_spam_model, _deliver, _message

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

SPAM_TOKEN = "catchupspamtokenqz"
HAM_TOKEN = "catchuphamtokenqz"


@pytest.fixture
def _drop_seeded_model(nest_instance, test_user):
    """Delete the person's model after the test: the session shares the actor,
    and a later receive test must not be scored against the model this one
    seeds (the contamination `test_mail_client_spam_receive._isolate_spam_model`
    documents)."""
    yield
    _clear_spam_model(nest_instance["db_path"], test_user["actor_id_bytes"])


def _inbox_watermarked_since(db_path, actor_id, since_s):
    """The actor's INBOX rows carrying `$FaunaSpamScored`, delivered at or
    after `since_s` — a copy the app scored and left in the inbox."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        (n,) = conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?1 "
            "AND mailbox = 'INBOX' AND flags LIKE '%$FaunaSpamScored%' "
            "AND internal_date >= ?2",
            (actor_id, int(since_s)),
        ).fetchone()
        return int(n)
    finally:
        conn.close()


def _junk_watermarked_since(db_path, actor_id, since_s):
    """The actor's Junk rows carrying `$FaunaSpamScored`, delivered at or after
    `since_s` — the nest half of the filing (subjects rest sealed, so the row
    is named by its delivery time)."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        (n,) = conn.execute(
            "SELECT COUNT(*) FROM bridge_imap_messages WHERE actor_id = ?1 "
            "AND mailbox = 'Junk' AND flags LIKE '%$FaunaSpamScored%' "
            "AND internal_date >= ?2",
            (actor_id, int(since_s)),
        ).fetchone()
        return int(n)
    finally:
        conn.close()


@pytest.mark.feature("spam")
@pytest.mark.real_conversations
def test_mail_delivered_before_the_first_lesson_is_filed_once_the_model_exists(
    logged_in_app, mail_bridge_inbound_to_imap, nest_instance, test_user,
    seal_helper_binary, _drop_seeded_model,
):
    """A message the app showed before the person had any spam model is filed
    to Junk by their app once the model exists — within one mail sweep, with
    no relaunch — and a ham message stays."""
    handle = mail_bridge_inbound_to_imap
    handle.assert_mta_running()
    mda = handle.mda
    db = nest_instance["db_path"]
    app = logged_in_app
    actor_id = test_user["actor_id_bytes"]

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    local_part = "catchup" + str(int(time.time() * 1000))[-8:]
    person = f"{local_part}@{mda.domain}"
    add_exact_alias(nest_instance["url"], test_user["signing_key"], mda.domain, local_part)
    # (1) No model yet.
    _clear_spam_model(db, actor_id)

    def _subjects():
        return [t.label or "" for t in app.conversations.list_threads()]

    def _shown(subject):
        return any(subject in s for s in _subjects())

    nonce = f"catchup{int(time.time() * 1000)}qx"
    spam_subject = f"Claim your prize {nonce}"
    ham_subject = f"Lunch notes {nonce}"
    deadline = time.monotonic() + 240.0
    delivered_since = int(time.time()) - 1
    # (2) Delivered while no model exists: shown, never scored.
    _deliver(
        handle, [person],
        _message(spam_subject, nonce, f"{SPAM_TOKEN} {SPAM_TOKEN} ({nonce})."),
        deadline,
    )
    _deliver(
        handle, [person],
        _message(ham_subject, f"{nonce}-ham", f"The {HAM_TOKEN} notes ({nonce})."),
        deadline,
    )
    wait_until(
        lambda: _shown(spam_subject) and _shown(ham_subject), 60.0,
        diagnose=lambda: f"both messages must reach the inbox while no model exists; "
        f"threads {_subjects()!r}; MDA log {mda.log_file}",
    )

    # (3) A model without a say: 80 samples is a 2.1-point ceiling under the
    # tier of 5. The marker's arrival proves a pass ran with it.
    _seed_spam_model(
        db_path=db, actor_id=actor_id, seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (40, 0), HAM_TOKEN: (0, 40)},
        spam_messages=40, ham_messages=40,
    )
    marker_subject = f"Agenda {nonce}"
    _deliver(
        handle, [person],
        _message(marker_subject, f"{nonce}-marker", f"Agenda for the week ({nonce})."),
        deadline,
    )
    wait_until(
        lambda: _shown(marker_subject), 60.0,
        diagnose=lambda: f"the marker must reach the inbox under the sub-line model; "
        f"threads {_subjects()!r}; MDA log {mda.log_file}",
    )
    assert _shown(spam_subject), (
        "a model without a say files nothing: the spam message stays shown"
    )
    assert _inbox_watermarked_since(db, actor_id, delivered_since) == 0, (
        "a model without a say watermarks no unreported copy — the one shot is "
        "owed to the sweep at which the say begins"
    )

    # (4) The model gets a say; one real lesson sends the model-updated push.
    _seed_spam_model(
        db_path=db, actor_id=actor_id, seal_helper_binary=seal_helper_binary,
        ngrams={SPAM_TOKEN: (110, 0), HAM_TOKEN: (0, 110)},
        spam_messages=110, ham_messages=110,
    )
    app.conversations.open_thread_with_subject(ham_subject)
    app.conversations.mark_message_spam(0)

    # (5) Filed by the person's own app, within one sweep of the say.
    wait_until(
        lambda: not _shown(spam_subject), 60.0,
        diagnose=lambda: f"the message delivered before the model existed must be "
        f"filed once it exists; threads {_subjects()!r}",
    )
    wait_until(
        lambda: _junk_watermarked_since(db, actor_id, delivered_since) >= 1, 30.0,
        diagnose=lambda: "the filed message must rest in Junk carrying "
        "$FaunaSpamScored (the app's disposition watermarks and moves it)",
    )
    assert _shown(ham_subject), "the ham message stays in the inbox"
