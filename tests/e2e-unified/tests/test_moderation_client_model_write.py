"""Tier_3 tier-1 spam-model **client-write** round-trip (leg 1e).

The first tier-1-sealing slice end-to-end (`docs/goal/architecture/
content-moderation-and-ranking.md` § Sealing tier-1; `docs/goal/behavior/
mail-spam.md` § Encrypted-mode interaction): **bob** corrects a post-decrypt
spam local detection through the moderation queue's real
``train-correction-button``, and — because the nest advertises
``spam-model-sealed-at-rest`` and bob has mail enabled (an MSEK) — the ham
train runs **client-side**: unwrap → mutate → re-seal to bob's own recipient
key → ``fauna.bridges.put_spam_model``. Ground truth is read at **nest-disk
rest**: ``spam_models.model_json`` holds an opaque sealed blob (never
plaintext ``serde_json`` — the nest refuses one and trains nothing itself; the
moderation-queue correction's nest half, ``fauna.moderation.train``, only
captures the report), and a SECOND
correction still succeeds and rewrites the blob — which proves the full loop:
``fetch_spam_model`` returned the client-sealed blob **verbatim** (no
double-seal) and the client unwrapped its own seal, mutated, and re-sealed.

Builds on ``test_moderation_local_detection.py`` (the read/score half): same
two-driven-client harness — only a member holding the private key decrypts,
and only decrypted content is classified/trainable. The client-side train uses
the SAME text the classifier saw (``ConversationsManager::message_body``), the
one plaintext position in encrypted mode (`content-scoring.md`).

The two-real-client tests run on **tui and linux** — the two apps carrying
the real-engine ``second_real_faunamls_app`` fixture (tui is the lead app since
2026-08-01, `testing.md` § Default app and nest mode; linux was the original
1d-surface lead). The single-client mark-as-spam witness at the bottom
(``test_mark_as_spam_trains_sealed_model``) needs no second engine, so it
speaks for every app whose automation surface carries ``inject_inbound``:
tui, linux, web, windows, macos and ios. android is left unmarked because its
column runs on no development machine today (`feature-catalog.md` § Columns
today); it gains its mark with the run that witnesses it.
"""

from __future__ import annotations

import json
import sqlite3
import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S, UI_SETTLE_S
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from tests.api import conv_api

pytestmark = pytest.mark.tier_3

# Bodies the shared `fauna_core::text_heuristic::classify_text` flags as spam
# well above the 0.3 retention gate (several spam phrases each), distinct so the
# two detections are two queue rows for two independent corrections.
SPAM_BODY_1 = "BUY NOW!!! CLICK HERE for FREE MONEY!!! Limited time — act now!!!"
SPAM_BODY_2 = "FREE MONEY waiting — ACT NOW, click here, buy now, limited time!!!"


def _stored_model(db_path: str, actor_id_hex: str) -> bytes | None:
    """bob's ``spam_models.model_json`` at nest-disk rest (None = no row)."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT model_json FROM spam_models WHERE actor_id = ?",
            (bytes.fromhex(actor_id_hex),),
        ).fetchone()
        return None if row is None else bytes(row[0])
    finally:
        conn.close()


def _is_plaintext_model(blob: bytes) -> bool:
    """True iff the stored blob decodes as plaintext serde_json — a shape no
    writer may store any more (the nest's ``is_sealed_model_blob`` inverse)."""
    try:
        json.loads(blob)
        return True
    except (ValueError, UnicodeDecodeError):
        return False


def _wait_stored_model(db_path: str, actor_id_hex: str, until, timeout: float = 20.0):
    deadline = time.time() + timeout
    blob = None
    while time.time() < deadline:
        blob = _stored_model(db_path, actor_id_hex)
        if until(blob):
            return blob
        time.sleep(0.5)
    return blob


# App scope, declared where COLLECTION can see it. The in-body
# `require_second_real_engine_fixture_supported()` below stays as the runtime
# backstop and remains the single place the supported set is described — but a
# guard in the body runs AFTER every fixture, and `real_faunamls_app` is now a
# real precondition that raises on a launch-gate app whose invocation never set
# `FAUNA_E2E_REAL_CONVERSATIONS` (helpers/real_rail_control.py). Marking the app
# scope deselects this at collection instead, which is the shape
# `pytest_collection_modifyitems` already prefers ("without it they are kept and
# then `pytest.skip` in-body under the wrong client, inflating the skip count").
# ⚠ Widening the second-real-engine fixture means widening BOTH — the guard's
# docstring says so.
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("moderation-queue")
def test_train_correction_writes_sealed_model_at_rest(
    real_faunamls_app, second_real_faunamls_app, nest_instance
):
    alice = real_faunamls_app
    alice.moderation.require_second_real_engine_fixture_supported(
        flow="the moderation-queue + mail UI"
    )
    bob_app, bob = second_real_faunamls_app
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]

    # ── bob enables mail through his client UI (the MSEK the model seals
    # under; without it no sealed write is possible and the correction trains
    # no model at all).
    from actions.mail_settings import MailSettingsActions

    mail = MailSettingsActions(bob_app.driver)
    mail.navigate()
    assert mail.is_page_visible(), "bob's mail-settings page not reachable"
    # Idempotent two-phase enable (status-indicator gated + credential settle) —
    # the proven path the sibling mail tests use.
    mail.ensure_mail_enabled()

    # ── Precondition (as the sibling read/score test): bob's engine published a
    # key package so alice's MLS bootstrap can reach him.
    wait_until(
        lambda: conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "bob's linux/tui engine never published a key package",
    )

    def deliver_and_correct(body: str, expect_snippet: str) -> None:
        # alice → bob over the real MLS wire.
        alice.conversations.real_resolve_send_new(bob["actor_id_hex"], body)
        # bob decrypts. ⚠ **The anchor is a receive CYCLE, not a tick**
        # (e2e-conventions.md convention 14 mechanism 3) — bob is a
        # `second_real_faunamls_app` (push suppressed by construction, backstop
        # ticker MUTED for every consumer of this fixture), so the poke is his
        # ONLY delivery trigger. Same shape as
        # `test_fauna_mls_two_client_inbox_drain.py`.
        cycles = conv_receive_cycles(bob_app.driver)
        started = cycles[0] if cycles else None
        poke_receive_cycle(bob_app.driver)
        await_receive_cycle_after(
            bob_app.driver, started, budget_s=RECEIVE_CYCLE_S,
            what=f"alice's {expect_snippet!r} message reaching bob's durable inbox",
        )

        def _decrypted():
            fauna = [
                t for t in bob_app.conversations.list_threads() if t.rail == "FaunaMls"
            ]
            return any(expect_snippet in t.snippet for t in fauna)

        wait_until(
            _decrypted,
            RECEIVE_CYCLE_S,
            diagnose=lambda: (
                f"bob never received/decrypted {expect_snippet!r}; threads: "
                f"{[(t.rail, t.snippet) for t in bob_app.conversations.list_threads()]}"
            ),
        )
        # The local detection surfaces as a queue row with the real correction
        # button; click it (the 1d train surface). Bounce off the feed each
        # attempt: the moderation view fetches on map, so a same-view
        # `navigate_to("moderation")` (nav stack already `[moderation]` after a
        # prior correction round) is a no-op that would never repaint the queue.
        # Client-local UI settle (the classify already ran, proven above), not a
        # nest round-trip.
        wait_until(
            lambda: (
                bob_app.driver.navigate_to("feed"),
                bob_app.moderation.navigate(),
                bob_app.moderation.correction_count() >= 1,
            )[2],
            UI_SETTLE_S,
            diagnose=lambda: (
                "the post-decrypt local detection never surfaced a "
                "train-correction-button row"
            ),
        )
        bob_app.moderation.train_correction(0)

    # ── Correction 1: the ham train runs client-side and the model lands
    # SEALED at rest (an opaque blob, never plaintext serde_json) —
    # `put_spam_model` stored it verbatim.
    deliver_and_correct(SPAM_BODY_1, "FREE MONEY")
    first = _wait_stored_model(db_path, bob["actor_id_hex"], lambda b: b is not None)
    assert first is not None, (
        "the train correction should write bob's per-user spam model "
        "(fauna.bridges.put_spam_model)"
    )
    assert not _is_plaintext_model(first), (
        "the model at nest-disk rest must be the client-sealed opaque blob, "
        "never plaintext serde_json (mail-spam.md § Encrypted-mode interaction)"
    )

    # ── Correction 2: the full round-trip proof. To apply a second train the
    # client must fetch_spam_model (returned VERBATIM — no seal-on-read
    # double-seal), unwrap its OWN sealed blob, mutate, and re-seal: only then
    # does the stored blob change while staying opaque.
    deliver_and_correct(SPAM_BODY_2, "ACT NOW")
    second = _wait_stored_model(
        db_path, bob["actor_id_hex"], lambda b: b is not None and b != first
    )
    assert second is not None and second != first, (
        "the second correction must rewrite the sealed model — the client "
        "unwraps its own sealed blob (fetch returned it verbatim), mutates, "
        "and re-seals"
    )
    assert not _is_plaintext_model(second), (
        "the re-written model must still be sealed at rest"
    )


# App scope, declared where COLLECTION can see it. The in-body
# `require_second_real_engine_fixture_supported()` below stays as the runtime
# backstop and remains the single place the supported set is described — but a
# guard in the body runs AFTER every fixture, and `real_faunamls_app` is now a
# real precondition that raises on a launch-gate app whose invocation never set
# `FAUNA_E2E_REAL_CONVERSATIONS` (helpers/real_rail_control.py). Marking the app
# scope deselects this at collection instead, which is the shape
# `pytest_collection_modifyitems` already prefers ("without it they are kept and
# then `pytest.skip` in-body under the wrong client, inflating the skip count").
# ⚠ Widening the second-real-engine fixture means widening BOTH — the guard's
# docstring says so.
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("spam")
def test_mark_as_spam_writes_sealed_history_row_then_undo(
    real_faunamls_app, second_real_faunamls_app, nest_instance
):
    """The **mail-surface train UI** (the live ``Insert`` consumer) + the 1e
    undo-half round-trip.

    bob marks a **received** conversation message as spam via the per-bubble
    ``dm-message-mark-as-spam-button`` (in the ⋯ overflow). Unlike the
    moderation-queue *social* train (``history_op: None``), a mail-surface train
    seals an **atomic audit row** (``SpamHistoryOp::Insert``) alongside the model
    re-seal — so a real UI train writes a **sealed** ``spam_training_history``
    row. That row renders on the ``mail-spam`` page; bob undoes it, and the row
    is gone (``list_spam_training_history`` → 0) while the model is inverted +
    still sealed at nest-disk rest. This is the round-trip deferred
    (it needed a UI path to create the sealed row it then undoes) — proven
    end-to-end through two real GUI apps.
    """
    from actions.mail_settings import MailSettingsActions
    from actions.mail_spam import MailSpamActions

    alice = real_faunamls_app
    alice.moderation.require_second_real_engine_fixture_supported(
        flow="the moderation-queue + mail UI"
    )
    bob_app, bob = second_real_faunamls_app
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]

    # ── bob enables mail (the MSEK his model + the sealed audit row seal under).
    mail = MailSettingsActions(bob_app.driver)
    mail.navigate()
    assert mail.is_page_visible(), "bob's mail-settings page not reachable"
    mail.ensure_mail_enabled()

    # ── bob's OWN spam threshold must not collapse the very message he is
    # about to mark as spam.
    #
    # The content-policy render engine binds on the conversations render
    # (`family-safety.md` § Content policy — scope is "the social rail (feed /
    # posts / conversations)"), composing the *viewer's own* `spam_threshold`
    # into a `Collapse` verdict over the post-decrypt `classify_text` labels.
    # A collapsed bubble paints a notice + timestamp only — no
    # `dm-message-text`, and no per-bubble ⋯ overflow, so the
    # `mark-as-spam-button` this test drives is suppressed along with it.
    #
    # The two SPAM_BODY_* constants were tuned for the LocalDetection
    # *retention* gate (0.3 → 300 per-mille, so the queue keeps them). That is
    # a different consumer of the same score than the render collapse, whose
    # default threshold is 800: SPAM_BODY_1 scores 850, so it collapses. The
    # bodies were written before any app enforced the render half (linux
    # 2026-07-18, tui 2026-08-02), which is why this
    # only started failing later — and on every app with the engine, not just
    # one.
    #
    # Arrange the threshold explicitly instead of re-tuning the body strings:
    # a future heuristic tweak must not silently re-break this test, and a
    # reader should not have to know a magic score to see why the bubble
    # renders. The save rebinds the render engine live on both apps.
    bob_app.settings.save_spam_preferences(spam_threshold="1.0")

    # bob's engine published a key package so alice's MLS bootstrap can reach him.
    wait_until(
        lambda: conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "bob's linux/tui engine never published a key package",
    )

    # ── alice → bob over the real MLS wire; bob decrypts. ⚠ **The anchor is a
    # receive CYCLE, not a tick** (e2e-conventions.md convention 14 mechanism 3) —
    # bob is a `second_real_faunamls_app` (push suppressed by construction,
    # backstop ticker MUTED for every consumer of this fixture), so the poke is
    # his ONLY delivery trigger. Same shape as
    # `test_fauna_mls_two_client_inbox_drain.py`.
    alice.conversations.real_resolve_send_new(bob["actor_id_hex"], SPAM_BODY_1)
    cycles = conv_receive_cycles(bob_app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(bob_app.driver)
    await_receive_cycle_after(
        bob_app.driver, started, budget_s=RECEIVE_CYCLE_S,
        what="alice's spam message reaching bob's durable inbox",
    )

    def _decrypted():
        fauna = [t for t in bob_app.conversations.list_threads() if t.rail == "FaunaMls"]
        return any("FREE MONEY" in t.snippet for t in fauna)

    wait_until(
        _decrypted,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            f"bob never received/decrypted the spam; threads: "
            f"{[(t.rail, t.snippet) for t in bob_app.conversations.list_threads()]}"
        ),
    )

    # ── bob opens the thread and marks alice's (received, !is_own) message as
    # spam via the per-bubble ⋯ → dm-message-mark-as-spam-button.
    # Disambiguate by the snippet the receive-wait above just confirmed: a
    # session-scoped bob accumulates several FaunaMls threads, and a bare
    # first-match pick opens whichever sorts first (see open_thread_by_rail).
    bob_app.conversations.open_thread_by_rail("FaunaMls", snippet="FREE MONEY")
    wait_until(
        lambda: bob_app.conversations.driver.count("dm-message-text") >= 1,
        UI_SETTLE_S,
        diagnose=lambda: "alice's received message bubble never rendered in the open thread",
    )
    bob_app.conversations.mark_message_spam(0)

    # ── The client-side train landed the model SEALED at nest-disk rest.
    trained = _wait_stored_model(db_path, bob["actor_id_hex"], lambda b: b is not None)
    assert trained is not None, (
        "mark-as-spam should write bob's per-user spam model "
        "(fauna.bridges.put_spam_model, client-side sealed)"
    )
    assert not _is_plaintext_model(trained), (
        "the model at rest must be the client-sealed opaque blob, never "
        "plaintext serde_json (mail-spam.md § Encrypted-mode interaction)"
    )

    # ── A sealed training-history row now exists — the live `Insert` (the whole
    # point: the social moderation-queue train writes none). Read over the real
    # `list_spam_training_history` RPC via the mail-spam page.
    spam = MailSpamActions(bob_app.driver)
    spam.navigate()
    assert spam.is_page_visible(), "bob's mail-spam page not reachable"
    spam.wait_for_history_count(1)

    # ── Undo half: the per-row undo inverts the sealed model + deletes the row,
    # fully client-side (a sealed row is nest-opaque — the client unwraps its own
    # sealed delta and commits the inverse + history-DELETE atomically).
    spam.undo_training(0)
    spam.wait_for_history_count(0)

    inverted = _wait_stored_model(
        db_path, bob["actor_id_hex"], lambda b: b is not None and b != trained
    )
    assert inverted is not None and inverted != trained, (
        "the undo must rewrite the sealed model (inverse delta applied + re-sealed)"
    )
    assert not _is_plaintext_model(inverted), (
        "the inverted model must still be sealed at rest"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("spam")
def test_mark_as_spam_trains_sealed_model(logged_in_app, nest_instance, test_user):
    """The per-bubble ``dm-message-mark-as-spam-button`` trains the sealed
    tier-1 spam model over a **received** message's body — the client-side
    sealed train path (``mail-spam.md`` § Training signal sources 1), proven at
    nest-disk rest. One body for every app: the gesture on each rides the one
    shared façade ``MailSettingsMachine::train_spam_model_client_mail``.

    The train reads ``MessageSnapshot.body`` and hits the real
    ``MailSettingsMachine`` → real nest, so it is indifferent to *how* the
    message arrived: a single-client + ``inject_inbound`` (``!is_own``) harness
    is a faithful stand-in for a real MLS-decrypted message for **this**
    mechanism (the button → ``!is_own`` gate → dispatch). The two-real-client
    MLS end-to-end **and** the sealed-history-row **undo** half live in the
    linux-lead ``test_mark_as_spam_writes_sealed_history_row_then_undo`` above,
    which needs the second real engine only tui and linux carry.
    """
    from actions.mail_settings import MailSettingsActions

    app = logged_in_app
    db_path = nest_instance["db_path"]
    actor_id_hex = test_user["actor_id_hex"]

    # ── enable mail: mints the MSEK the model seals under. The nest advertises
    # the always-on ``spam-model-sealed-at-rest`` token, so the train takes the
    # sealed client-side path (unwrap → mutate → re-seal → put_spam_model) — the
    # only path there is.
    mail = MailSettingsActions(app.driver)
    mail.navigate()
    assert mail.is_page_visible(), "mail-settings page not reachable"
    mail.ensure_mail_enabled()

    # ── a received (!is_own) spammy message; open its thread by identity.
    app.conversations.inject_and_open_thread(
        rail="FaunaMls", sender="spammer@self-nest.test", body=SPAM_BODY_1
    )
    assert app.conversations.driver.count("dm-message-text") >= 1, (
        "the received bubble should render in the open thread"
    )

    # ── mark it as spam via the ⋯ overflow (gated !is_own — a received bubble).
    app.conversations.mark_message_spam(0)

    # ── the client-side train landed the model SEALED at nest-disk rest.
    trained = _wait_stored_model(db_path, actor_id_hex, lambda b: b is not None)
    assert trained is not None, (
        "mark-as-spam should write the actor's per-user spam model "
        "(fauna.bridges.put_spam_model, client-side sealed)"
    )
    assert not _is_plaintext_model(trained), (
        "the model at rest must be the client-sealed opaque blob, never "
        "plaintext serde_json (mail-spam.md § Encrypted-mode interaction)"
    )
