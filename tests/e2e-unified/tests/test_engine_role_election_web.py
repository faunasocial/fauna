"""tier_3 e2e: web's engine-role election — two tabs, ONE account, one writer.

``docs/goal/architecture/apps/account-scoping.md`` § Concurrent instances →
*Web*. Row 44 landed the per-tab account pin, which isolates two tabs holding
**different** accounts. This file witnesses the leg that was left open: two tabs
holding the **same** account, where the pin isolates nothing because there is
nothing to isolate — both tabs legitimately serve one account, and the hazard is
that both then drive the MLS commit/save path.

**Why that is a corruption bug and not a tidiness one.** Every tab of one origin
holding one account shares that account's ``localStorage`` device id
(``$lib/device-id``), hence one MLS **device leaf**,
and one account-scoped ``provider`` replica both CAS-put on the nest. Two
engines advancing one leaf is precisely the fork
``libs/fauna-client-mls-sync/src/commit_gate.rs`` forbids ("a shared single leaf
never forks a ratchet generation"), and the replica's three-way merge resolves a
genuine both-sides-changed key **theirs-wins**
(``libs/fauna-mls/src/state_replica.rs``) — lossy for exactly that state. So the
losing tab must not merely avoid *sending*: it must run no engine at all, since
the manager builder itself re-seals the replica, publishes crash-staged folder
rotations and sweeps owner markers before it returns.

**Why the assertion is polarised.** Asserting only that tab B shows the refusal
would be satisfied by a tab that failed to boot for any unrelated reason. So the
witness is a PAIR: tab B renders the standing refusal, *and* tab A's compose
affordance is live — ``new-conversation-button`` is bound to ``disabled={!manager}``,
so an enabled button is positive evidence that tab A really did take the role
rather than both tabs failing. A pre-fix run fails on tab B (no error rendered,
because the old code happily built a second writing engine).

**Same string as every other app.** The rendered text is
``conversations.errors.served_elsewhere`` — the one linux's ``page_error_text``
and apple's ``ConversationsVM`` already paint for their native role-lock
refusal, and it reads correctly for a tab as it does for an OS process.

tier_3: needs a real ``fauna-nest`` binary; web app only (the mechanism is the
Web Locks API — every native app takes the kernel file lock instead).
"""
from __future__ import annotations

import time

import pytest

from helpers.web_tabs import open_same_account_tab

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

CONVERSATIONS_NAV = {"nav": {"stack": [{"view": "conversations"}]}}
ERROR_MESSAGE = "error-message"
NEW_CONVERSATION_BUTTON = "new-conversation-button"


def _await_text(driver, testid, *, budget_s=60, what="the element"):
    """Deadline-poll until ``testid`` carries non-empty text.

    Point 14: a named generous budget over latency-independent state, never a
    settle-sleep. The budget is a ceiling on "the second tab booted and asked for
    the role", which is one Web Locks round trip plus a page boot — far under it
    on any non-pathological run.
    """
    deadline = time.monotonic() + budget_s
    last = ""
    while time.monotonic() < deadline:
        try:
            last = driver.get_text(testid) or ""
        except Exception:  # noqa: BLE001 — mid-boot the element may not exist yet
            last = ""
        if last.strip():
            return last
        time.sleep(0.5)
    raise AssertionError(f"{what}: {testid} carried no text within {budget_s}s")


@pytest.mark.feature("second-identity-in-its-own-window")
def test_a_second_tab_on_one_account_runs_no_second_mls_engine(logged_in_app):
    """One browser profile, one account, two tabs: exactly one drives MLS.

    The loser renders the standing "served elsewhere" refusal rather than
    crashing (which would be a bug) or writing (which would fork the ratchet).
    """
    # No in-body platform guard: `pytest.mark.web` above is authoritative and the
    # collection hook deselects every non-web parametrization this file's `app`
    # dependency would otherwise acquire (conftest `pytest_collection_modifyitems`
    # — `params & marker_platforms`). A `pytest.skip` here would be the exact
    # anti-pattern that hook exists to delete: it inflates the skip count and
    # hides genuinely-missing coverage behind a gate that never had to exist.
    app = logged_in_app
    tab_b = None
    try:
        # (1) Tab A is the signed-in app. Put it on conversations and let it take
        # the role: the manager is built by the layout poll, and the compose
        # button is bound to `disabled={!manager}`, so an ENABLED button is the
        # observable for "this tab holds the engine".
        app.driver.set_state(CONVERSATIONS_NAV)
        app.driver.wait_for(NEW_CONVERSATION_BUTTON, timeout=60)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not app.driver.is_enabled(NEW_CONVERSATION_BUTTON):
            time.sleep(0.5)
        assert app.driver.is_enabled(NEW_CONVERSATION_BUTTON), (
            "tab A never built its conversations manager, so this test cannot say "
            "anything about the SECOND tab — the compose button stayed disabled "
            f"(disabled={{!manager}}). error={app.error_text()!r}"
        )

        # (2) A second TAB of the same profile — one shared origin store, so the
        # same device leaf and the same provider replica. `open_twin_page()`
        # would be a second *device* and could not witness this at all.
        #
        # `open_same_account_tab` is load-bearing, not tidiness: `logged_in_app`
        # signs web in through the session patch, which writes the page's
        # IN-MEMORY identity store and not `localStorage` — so a plain new tab
        # boots from an empty origin store ("load_identity":"NONE" in its own
        # launch log), lands on onboarding as nobody, and renders no refusal
        # because it is not the same account (nor any). The helper makes the two
        # tabs one profile — one origin store, one device leaf — and signs this
        # one in the way every other web test does. Measured 2026-09-20: that is
        # why this test had never had a green recorded run.
        tab_b = open_same_account_tab(app.driver)
        tab_b.set_state(CONVERSATIONS_NAV)

        # (3) Tab B asked for the role, was refused, and says so. This is the
        # assertion the pre-fix tree fails: it used to build a second writing
        # engine here, silently.
        text = _await_text(
            tab_b, ERROR_MESSAGE, what="the second tab's standing engine-role refusal"
        )
        assert "another instance" in text.lower(), (
            "the second tab on the same account rendered no engine-role refusal — "
            "it either built a SECOND MLS engine over the shared device leaf (the "
            "ratchet fork `commit_gate.rs` forbids) or failed some other way. "
            f"error-message read {text!r}, expected the shared "
            "`conversations.errors.served_elsewhere` text"
        )

        # (4) And tab B is refused, not broken: its compose affordance is dead,
        # which is what "no engine in this tab" must look like.
        assert not tab_b.is_enabled(NEW_CONVERSATION_BUTTON), (
            "the second tab renders the served-elsewhere refusal but still offers a "
            "live compose button — a user could drive a send through an engine this "
            "tab was told not to run"
        )

        # (5) Tab A is UNDISTURBED. The election must not have cost the winner
        # anything: a refusal that also breaks the holder is not an election.
        assert app.driver.is_enabled(NEW_CONVERSATION_BUTTON), (
            "tab A lost its engine when tab B opened — the role was handed over "
            "rather than held, so the two tabs would trade the leaf back and forth"
        )
        assert not app.has_error(), (
            f"tab A shows an error after tab B was refused: {app.error_text()!r}"
        )

        # (6) Tab B is a second window ON THE SAME IDENTITY, signed in — not a
        # tab that booted as someone else or sat unauthenticated behind its
        # refusal. This is the "opening an identity that is already open gives
        # you a second window on it" half the page's outcome 4 names; the native
        # witnesses read the same `session.actor_id` off both instances.
        a_session = (app.driver.get_state() or {}).get("session", {})
        b_session = (tab_b.get_state() or {}).get("session", {})
        assert a_session.get("actor_id"), f"tab A reports no signed-in actor: {a_session!r}"
        assert b_session.get("actor_id") == a_session.get("actor_id"), (
            "tab B must serve the same identity tab A does — a tab that booted as "
            "nobody would render the refusal for no account at all; "
            f"tab A={a_session!r}, tab B={b_session!r}"
        )
    finally:
        if tab_b is not None:
            tab_b.teardown()
