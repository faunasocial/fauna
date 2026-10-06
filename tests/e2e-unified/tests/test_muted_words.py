"""Cross-app Muted keywords — the Q3 conversation keyword-mute.

``docs/goal/behavior/moderation.md`` § Muted keywords;
``docs/goal/architecture/content-moderation-and-ranking.md`` § Q3;
``tests/e2e-unified/ui.yaml`` ``muted-words`` page + ``dm-message-muted`` /
``dm-message-muted-reveal-button``.

Two per-app surfaces:
  1. A "Muted words" Settings sub-page — add / list / remove a single user-global
     sealed keyword list over the shared client-config seam
     (the ``fauna.state.moderation`` ``muted_keywords``; linux calls
     ``fauna_client_config`` directly, web the wasm ``mutedKeywords{List,Set}``).
     No new WS-RPC kind — the list is sealed client-side, nest-opaque.
  2. A conversation collapse render — a decrypted DM whose body matches the list
     (shared ``fauna_core::keyword::body_excludes_matches`` — FFI
     ``matches_muted_keywords`` / wasm ``matchesMutedKeywords``) is collapsed
     behind ``dm-message-muted`` with a ``dm-message-muted-reveal-button`` that
     reveals it for the session. It is a **hide/collapse, NOT a spam-queue flag**
     (it does not feed the ``LocalDetectionStore`` / moderation queue).

tier_2: the real client driver renders both surfaces; the config CRUD writes the
real sealed list to the real nest's ``fauna.state.moderation`` plane (the nest is
needed for auth + the config seam), and the inbound DM is **seeded** via ``inject_inbound_for_test``
(the same inject-seam shape as ``test_conversations_link_preview.py``) — fixture
setup arranging the precondition, while the mutations
under test (add a muted word, reveal) go through the client UI. web +
linux + windows adopt here; the macos / android render legs add their marker
when their per-app UI lands. tui adopted 2026-07-29 (the 7th client — both
surfaces over the same shared seams in direct Rust: ``fauna_client_config`` for
the list, ``fauna_core::keyword::body_excludes_matches`` for the collapse).
"""

import pytest

pytestmark = [pytest.mark.tier_2]

MUTED_TERM = "lottery"
# Matches MUTED_TERM case-insensitively (the shared matcher is a case-insensitive
# substring, OR across terms).
MUTED_BODY = "Congratulations — you just won the LOTTERY jackpot!"
# No muted term — stays visible.
CLEAN_BODY = "hey, are we still on for lunch tomorrow?"


def _clear_muted_words(mw) -> None:
    """Remove every listed term, each removal waited on — the teardown for the
    session-scoped actor, which a later test here needs empty."""
    mw.navigate()
    for _ in range(mw.row_count()):
        before = mw.row_count()
        if not before:
            return
        mw.remove(0)
        mw.wait_for_row_count(before - 1)


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("muted-words")
def test_muted_words_settings_crud(logged_in_app):
    """The Muted words Settings sub-page adds, lists, and removes terms over the
    shared sealed ``fauna.state.moderation`` ``muted_keywords`` seam (normalize-on-write:
    trim, drop blanks, case-insensitive dedupe)."""
    mw = logged_in_app.muted_words
    mw.navigate()
    assert mw.is_page_visible(), "muted-words sub-page did not render"

    # Empty state before any term. The barrier, not a bare read: the placeholder
    # paints only once the muted-keyword read has RESOLVED, so this wait IS the
    # "page loaded and found nothing" assertion.
    assert mw.wait_for_empty_state(), (
        "empty-state placeholder never appeared on a fresh account's list; "
        f"rows={mw.row_count()} words={mw.words()!r} error={logged_in_app.error_text()!r}"
    )
    assert mw.row_count() == 0, f"expected an empty list; words={mw.words()!r}"

    try:
        mw.add("spam")
        assert mw.wait_for_row_count(1), f"add did not land one row; words={mw.words()!r}"
        assert "spam" in mw.words()

        mw.add(MUTED_TERM)
        assert mw.wait_for_row_count(2), f"second add did not land; words={mw.words()!r}"
        assert set(mw.words()) == {"spam", MUTED_TERM}

        # Remove the first row; the other survives.
        mw.remove(0)
        assert mw.wait_for_row_count(1), f"remove did not drop a row; words={mw.words()!r}"
        remaining = mw.words()
        assert len(remaining) == 1 and remaining[0] in {"spam", MUTED_TERM}

        mw.remove(0)
        assert mw.wait_for_row_count(0), f"list not empty after removing all; words={mw.words()!r}"
        assert mw.wait_for_empty_state(), (
            "empty-state placeholder should return once the list is empty"
        )
    finally:
        # The actor is session-scoped, and the next test here asserts the empty
        # state: a red above used to leave its terms for it to trip on.
        _clear_muted_words(mw)


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("muted-words")
def test_muted_words_empty_state_marks_a_loaded_page_not_a_loading_one(logged_in_app):
    """A fresh account's Muted words page finishes loading and SAYS it has no
    muted terms, under ``muted-word-empty`` (``docs/goal/ui/README.md``
    § *List pages: loading is not empty*).

    Why the gate exists: the term list is empty both before the first
    plane read returns and after one that found nothing, so every app
    announced "You haven't muted any words yet" over a list nobody had read.
    ``MutedWordsSnapshot.loaded`` — minted only by a completed round trip through
    the shared ``load_muted_words`` / ``save_muted_words`` seam — is the second
    painting condition.

    Scope split, deliberately, mirroring the media/labeler precedents: the
    NEGATIVE half — that a page still loading paints no empty state — is a race
    at this level, so it is pinned deterministically one tier down (the shared
    ``the_empty_state_paints_only_for_a_read_that_found_nothing``, tui's
    ``the_empty_state_does_not_paint_before_the_read_resolves``, linux's
    ``the_empty_state_is_hidden_until_a_read_resolves``, android's
    ``noEmptyStateBeforeTheReadResolves`` and windows'
    ``Loaded_IsFalseUntilAReadReturns`` — each red if its own gate is removed).

    What THIS test is the only witness to: that the gate did not break the
    genuine empty state. An app that adds ``loaded &&`` to the paint condition
    but never wires the field through would make the empty state vanish forever
    — a worse bug than the one being fixed, invisible to a tier_1 test against a
    fake seam, and caught here the moment ``wait_for_empty_state`` times out.
    """
    mw = logged_in_app.muted_words
    mw.navigate()

    assert mw.wait_for_empty_state(), (
        "the Muted words page never reported itself loaded-and-empty within "
        f"{mw.LOAD_BUDGET_S:.0f}s: no rows and no muted-word-empty. "
        f"rows={mw.row_count()} error={logged_in_app.error_text()!r}; "
        f"{logged_in_app.driver.diagnose('muted-word-empty')}"
    )
    assert mw.row_count() == 0, (
        f"a loaded empty page must have no rows; words={mw.words()!r}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.feature("muted-words")
def test_muted_message_collapses_behind_reveal(logged_in_app):
    """A decrypted DM whose body matches a muted term is collapsed behind
    ``dm-message-muted`` with a ``dm-message-muted-reveal-button``; a non-matching
    message in the same thread stays visible; the reveal button shows the body for
    the session. The mute is a hide/collapse, not a spam-queue flag."""
    app = logged_in_app
    conv = app.conversations
    d = app.driver

    # Mute a term via the real Settings UI (the mutation under test goes through
    # the client). test_user is session-scoped, so this durable per-actor
    # mutation MUST be undone regardless of outcome (the same convention
    # _isolate_spam_model follows for a seeded spam model) — a later test in
    # the same run (test_muted_words_settings_crud) asserts an EMPTY list.
    app.muted_words.navigate()
    app.muted_words.add(MUTED_TERM)

    try:
        # Inside the try: a term that lands after this gives up must still be
        # undone below, or the next test's empty list is not empty.
        assert app.muted_words.wait_for_word(MUTED_TERM), (
            f"muted term did not persist; words={app.muted_words.words()!r}"
        )
        # Seed two inbound DMs from the same sender (one matches, one clean) —
        # fixture setup arranging the precondition (e2e point 8b), not the
        # behavior under test.
        conv.inject_inbound_for_test(rail="FaunaMls", sender="carol-muted@self-nest.test", body=CLEAN_BODY)
        # The second inbound merges into the same carol thread; open it by the identity
        # the inject resolves to, never by a "carol" needle (5 test files share it).
        conv.inject_and_open_thread(
            rail="FaunaMls", sender="carol-muted@self-nest.test", body=MUTED_BODY
        )

        # The matching message collapses; the clean one still shows its body.
        assert d.count("dm-message-muted") == 1, (
            "exactly the matching DM should collapse behind the muted placeholder; "
            f"muted={d.count('dm-message-muted')} text={d.count('dm-message-text')} "
            f"error={app.error_text()!r}"
        )
        assert d.count("dm-message-text") == 1, (
            "the non-matching DM should still render its body; "
            f"text={d.count('dm-message-text')}"
        )
        assert d.is_visible("dm-message-muted-reveal-button"), "collapsed DM missing its reveal button"

        # Reveal is session-local: the muted body appears; the placeholder is gone.
        d.click("dm-message-muted-reveal-button")
        assert d.count("dm-message-text") == 2, (
            f"reveal should show the muted body; text={d.count('dm-message-text')}"
        )
        assert d.count("dm-message-muted") == 0, "the placeholder should be gone after reveal"
    finally:
        _clear_muted_words(app.muted_words)
